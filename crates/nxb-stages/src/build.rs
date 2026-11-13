//! `shaderbuild build`: the whole Mac shader step from the split's package records, streaming on the executor:
//! translation batches (`nxb_xlate::msl::translate`, in process) → per-shader finish (libraries, side libraries and
//! variants through the store, reflection) → outputs, then `_b_build.json`, `inputs.tsv` and the guard tables. A paired
//! vertex shader is translated when its pixel shaders are done, because `vs_pad` reads their inputs.

use crate::build_finish::Shared;
use crate::inputs::ShaderInputs;
use nxb_error::Fatal;
use nxb_pyjson::{Value, dumps};
use nxb_sched::{Budget, Executor, Report, SingleFlight, Task};
use nxb_shader::{pch, target::MAC};
use nxb_store::{Durability, Store};
use nxb_tool::{Io, MetalTools, ProcessRegistry, Resolver, SdkRoots};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

pub use crate::build_hooks::{DoneFn, OnDone, OnStart, StartFn};

/// Inputs and tools of a build.
#[derive(Clone, Debug)]
pub struct BuildConfig {
    /// The split's package folder (`<stage>_<group>.bin`).
    pub pkg: PathBuf,
    /// The output cache folder, outputs go to `<out>/b-<chain>/`.
    pub out: PathBuf,
    /// The game profile folder (`shader_map.tsv`, `shader_facts.tsv`, `shader_pairs.tsv`).
    pub profile: PathBuf,
    /// Chain version V (taken as given until the Rust chain-files list exists).
    pub chain: String,
    /// Game id and profile inputs digest for `inputs.tsv`.
    pub game: String,
    /// Profile inputs digest.
    pub inputs: String,
    /// Store root.
    pub store: PathBuf,
    /// Private work folder (header, guard tables, timeline).
    pub work: PathBuf,
    /// Folder for the short-lived files (temp AIR): best on the internal disk. Its `tmp/` is removed at the end.
    pub scratch: PathBuf,
    /// Tokens.
    pub jobs: u32,
    /// Translation batch size after the first ones.
    pub batch: usize,
    /// Size of the first `jobs` translation batches (small, so every token starts at once).
    pub batch_first: usize,
    /// Only these keys (empty: all).
    pub keys: Vec<String>,
    /// The own front end (off when `None`).
    pub own_front: Option<crate::own_front::OwnFrontConfig>,
    /// Per-shader completion callback.
    pub on_done: Option<OnDone>,
    /// The key list, once known.
    pub on_start: Option<OnStart>,
    /// Keys to translate first, in this order (empty: pixel and compute shaders, then paired vertex shaders).
    pub order: Vec<String>,
    /// Keys a title used: translated first, in this order (empty: no change).
    pub manifest: Vec<String>,
    /// Build only the manifest's keys, mark the folder partial (`_b_partial.json`).
    pub partial: bool,
}

/// One line of the timeline: what ran, when, how long.
#[derive(Clone, Debug)]
pub struct Span {
    /// `translate` or `finish`.
    pub what: &'static str,
    /// Key (first key of a batch).
    pub key: String,
    /// Start since the build began.
    pub start: Duration,
    /// Wall time.
    pub wall: Duration,
}

/// The build's result.
#[derive(Debug)]
pub struct BuildReport {
    /// `(key, st, err)` in `_b_build.json` order.
    pub results: Vec<(String, String, String)>,
    /// Executor reports of both phases.
    pub phases: Vec<Report>,
    /// Timeline.
    pub spans: Vec<Span>,
    /// Wall time.
    pub wall: Duration,
    /// Own front end: libraries made, fallbacks to Apple, and why it is on or off.
    pub own: Option<(u64, u64)>,
    /// The note on the own front end.
    pub own_note: String,
    /// Translations answered from the store (early cutoff).
    pub xlate_hits: u64,
    /// Keys the translator refused, by reason (`panic` for a caught panic).
    pub xlate_refused: Vec<(String, u64)>,
}

pub(crate) fn fatal(what: &str, e: impl std::fmt::Display) -> Fatal {
    Fatal::Internal(format!("{what}: {e}"))
}

/// Run the build.
pub fn build(cfg: BuildConfig) -> Result<BuildReport, Fatal> {
    build_for_target(cfg, MAC)
}

/// Run the same shader pipeline for an explicit compilation target.
pub fn build_for_target(
    mut cfg: BuildConfig,
    conv: nxb_shader::target::TargetConv,
) -> Result<BuildReport, Fatal> {
    let t0 = Instant::now();
    let out = cfg.out.join(format!("b-{}", cfg.chain));
    let tmp = cfg.scratch.join("tmp");
    for d in [&out, &tmp, &cfg.work.join("pch")] {
        fs::create_dir_all(d).map_err(|e| fatal("create folder", e))?;
    }
    if cfg.keys.is_empty() {
        let _ = fs::remove_file(out.join("_b_cas.txt"));
    }
    let inputs =
        ShaderInputs::load(&cfg.pkg, &cfg.profile).map_err(|e| fatal("profile tables", e))?;
    let total = crate::build_partial::prepare(&mut cfg, &out, &inputs.loc)?;
    let pairs = crate::build_shared::pairs(&cfg);
    let keys: Vec<String> = if cfg.keys.is_empty() {
        // The keys the split did not find are reported as failed, so `_b_build.json` names every profile key.
        let all: BTreeSet<&String> = inputs.loc.keys().chain(&inputs.unsplit).collect();
        all.into_iter().cloned().collect()
    } else {
        let want: BTreeSet<String> = cfg.keys.iter().cloned().collect();
        let mut k: BTreeSet<String> = want.clone();
        for v in &want {
            for p in pairs.get(v).into_iter().flatten() {
                if inputs.loc.contains_key(p) {
                    k.insert(p.clone());
                }
            }
        }
        k.into_iter().collect()
    };
    // Clear the target before writing any shader. An interrupted build must not certify old libraries.
    {
        let dir = &out;
        fs::create_dir_all(dir).map_err(|e| fatal("output folder", e))?;
        if cfg.keys.is_empty() {
            match fs::remove_file(dir.join("inputs.tsv")) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(fatal("inputs.tsv", e));
                }
                _ => {}
            }
        }
        for entry in fs::read_dir(dir).map_err(|e| fatal("shader outputs", e))? {
            let entry = entry.map_err(|e| fatal("shader output", e))?;
            let n = entry.file_name().to_string_lossy().into_owned();
            let is_out = n.len() > 18
                && n.as_bytes()[..16].iter().all(|c| c.is_ascii_hexdigit())
                && n[16..].starts_with("_b");
            if is_out && (cfg.keys.is_empty() || keys.iter().any(|k| n.starts_with(k.as_str()))) {
                fs::remove_file(entry.path()).map_err(|e| fatal("old shader output", e))?;
            }
        }
    }
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let registry = Arc::new(ProcessRegistry::new());
    let resolver = Resolver {
        xcrun: "/usr/bin/xcrun".into(),
        developer_dir: std::env::var_os("DEVELOPER_DIR"),
        home: home.clone(),
    };
    let roots =
        SdkRoots::resolve(&resolver, &[conv.target], &registry).map_err(|e| fatal("SDK", e))?;
    let env = roots
        .apple_env(conv.target, &tmp, &home)
        .map_err(|e| fatal("environment", e))?;
    let tools = Arc::new(
        MetalTools::resolve(&resolver, conv.target, env, tmp.clone(), registry.clone())
            .map_err(|e| fatal("metal", e))?,
    );
    let (ver, bld) = resolver
        .sdk_version(conv.target, &registry)
        .map_err(|e| fatal("SDK version", e))?;
    let cid = tools
        .compiler_id(&ver, &bld)
        .map_err(|e| fatal("compiler id", e))?;
    // OwnFront currently emits Mac AIR/raw libraries. Do not use it for iOS.
    let (own_cfg, own_note) = if conv.target == nxb_model::Target::Mac {
        crate::own_front::resolve(cfg.own_front.as_ref(), &cid)
    } else {
        (None, "Apple front end for iOS".to_string())
    };
    let own = match &own_cfg {
        Some(o) => Some(
            crate::own_front::OwnFront::open(o, &tools, &resolver)
                .map_err(|e| fatal("own front end", e))?,
        ),
        None => None,
    };
    let mut ident = b"dmsh_mtlcache 2\0".to_vec();
    ident.extend_from_slice(conv.record().as_bytes());
    ident.push(0);
    ident.extend_from_slice(cid.as_bytes());
    let pchp = tools
        .build_pch(
            &conv.pch_flags(),
            conv.compile_flags,
            &pch::header(),
            pch::PROBE,
            &cfg.work.join("pch"),
        )
        .map_err(|e| fatal("header", e))?;
    let mut store = Store::open(&cfg.store).map_err(|e| fatal("store", e))?;
    store.set_durability(Durability::Relaxed);
    let xlate = crate::xlate_cache::Xlate::open(&cfg.store, &cfg.chain)
        .map_err(|e| fatal("translation cache", e))?;
    let shared = Arc::new(Shared {
        conv,
        out: out.clone(),
        chain: cfg.chain.clone(),
        ident: nxb_model::sha256(&ident).to_hex(),
        compiler_id: cid,
        tools,
        own,
        pch: pchp,
        io: Io::Pipes,
        store,
        xlate,
        inputs_of: Mutex::new(BTreeMap::new()),
        inputs,
        pairs,
        flights: SingleFlight::new(),
        gates: Mutex::new(BTreeMap::new()),
        dependents: Mutex::new(BTreeMap::new()),
        results: Mutex::new(BTreeMap::new()),
        on_done: cfg.on_done.clone(),
        used: Mutex::new(BTreeSet::new()),
        spans: Mutex::new(Vec::new()),
        t0,
    });
    if let Some(f) = &cfg.on_start {
        (f.0)(&keys);
    }
    let (phase_a, phase_b): (Vec<String>, Vec<String>) = keys
        .iter()
        .cloned()
        .partition(|k| !shared.pairs.contains_key(k));
    // One streaming run (design-v2 4): a paired vertex shader is translated and finished as soon as its pixel
    // shaders are done (vs_pad reads their inputs), not after a global barrier as in dmsh_build.py.
    {
        let building: BTreeSet<&String> = keys.iter().collect();
        let mut gates = shared.gates.lock().unwrap_or_else(PoisonError::into_inner);
        let mut deps = shared
            .dependents
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for v in &phase_b {
            let partners: Vec<&String> = shared.pairs[v]
                .iter()
                .filter(|p| building.contains(p) && !shared.pairs.contains_key(*p))
                .collect();
            gates.insert(
                v.clone(),
                crate::build_finish::Gate {
                    remaining: 1 + partners.len(),
                    out: None,
                },
            );
            for p in partners {
                deps.entry(p.clone()).or_default().push(v.clone());
            }
        }
    }
    // Batches: small ones first so every token gets work at once, then large ones.
    let mut tasks = Vec::new();
    let mut order: Vec<&String> = phase_a.iter().chain(&phase_b).collect();
    crate::build_partial::sort(&cfg, &mut order);
    let (small, large) = (cfg.batch_first.max(1), cfg.batch.max(1));
    let mut at = 0;
    let mut n = 0usize;
    while at < order.len() {
        let size = if n < cfg.jobs as usize { small } else { large };
        let ks: Vec<String> = order[at..(at + size).min(order.len())]
            .iter()
            .map(|k| (*k).clone())
            .collect();
        at += ks.len();
        let s = shared.clone();
        let prio = -(n as i64);
        tasks.push(
            Task::new(move |ctx| crate::build_xlate::translate_batch(&s, ks, ctx))
                .prio(prio)
                .mem(500 << 20),
        );
        n += 1;
    }
    let phases = vec![
        Executor::new(Budget::fixed(cfg.jobs).with_memory(Budget::default_memory())).run(tasks),
    ];
    let results_map = std::mem::take(
        &mut *shared
            .results
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    let mut results = Vec::new();
    for k in phase_a.iter().chain(&phase_b) {
        let (st, err) = results_map
            .get(k)
            .cloned()
            .unwrap_or_else(|| ("internal error".into(), "no result".into()));
        results.push((k.clone(), st, err));
    }
    let bb = Value::List(
        results
            .iter()
            .map(|(k, st, e)| {
                Value::obj([
                    ("key", Value::str(k.clone())),
                    ("st", Value::str(st.clone())),
                    ("err", Value::str(e.clone())),
                ])
            })
            .collect(),
    );
    fs::write(out.join("_b_build.json"), dumps(&bb)).map_err(|e| fatal("_b_build.json", e))?;
    if cfg.keys.is_empty() {
        let stamp = format!(
            "# the game and the profile inputs this library was built from (dm/gfx/tools/dmsh_chain.py inputs)\ngame\t{}\ninputs\t{}\n",
            cfg.game, cfg.inputs
        );
        fs::write(out.join("inputs.tsv"), &stamp).map_err(|e| fatal("inputs.tsv", e))?;
        // `_b_cas.txt`: the cache entries this full build used, one per line, sorted. Python lists its CAS digests there
        // (its pruner keeps those), ours are store keys, so a Python pruner sees none of its entries in use, which is
        // true for a library built here.
        let used = shared.used.lock().unwrap_or_else(PoisonError::into_inner);
        let text: String = used.iter().map(|k| format!("{k}\n")).collect();
        fs::write(out.join("_b_cas.txt"), text).map_err(|e| fatal("_b_cas.txt", e))?;
        crate::guard_tables::write(&out, &cfg.work.join("tables"), cfg.jobs)
            .map_err(|e| fatal("guard tables", e))?;
    }
    crate::build_partial::mark(&cfg, &out, &results, total)?;
    let _ = fs::remove_dir_all(cfg.scratch.join("tmp"));
    let _ = fs::remove_dir(&cfg.scratch);
    let spans = std::mem::take(&mut *shared.spans.lock().unwrap_or_else(PoisonError::into_inner));
    let own_note = shared
        .own
        .as_ref()
        .map_or(own_note.clone(), |o| o.note(&own_note));
    Ok(BuildReport {
        own: shared.own.as_ref().map(|o| o.counts()),
        own_note,
        xlate_hits: shared.xlate.hits.load(std::sync::atomic::Ordering::Relaxed),
        xlate_refused: shared.xlate.refused(),
        results,
        phases,
        spans,
        wall: t0.elapsed(),
    })
}
