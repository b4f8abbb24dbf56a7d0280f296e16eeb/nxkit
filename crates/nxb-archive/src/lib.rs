//! The pipeline archive stage (design-pipeline-rest 10.4, replaces the batch loop of `pso_offline.py`): the script's
//! render pipeline bases are cut into batches by content ([`mod@cut`]), each batch is compiled by `metal-tt` (Apple's
//! back end stays a tool) in parallel under the scheduler's token and memory budget, kept in the content-addressed
//! store under a key of its inputs (unchanged batches are free on a rebuild, also after insertions elsewhere), and the
//! pieces are merged by `metal-pack` in hash order.
//!
//! Equality: `metal-tt` is not byte-deterministic (the same batch twice differs), so no archive of it is
//! byte-identical to another. "Equal" is what the host relies on: every base of the script is found in the archive by
//! `-[MTL4Archive newRenderPipelineStateWithDescriptor:]` (`tools/archive_probe.m`).
//!
//! The script may also hold compute pipelines (`compute_pipeline_descriptors`). They are compiled in their own small
//! batches ([`mod@compute`]), apart from the render bases.
//!
//! The planning half of `pso_offline.py` (pairs to bases, the script, `prebuilt.tsv`) is in [`mod@plan`].

pub mod cli;
pub mod compute;
pub mod cut;
pub mod plan;
pub mod script;
pub mod stream;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nxb_model::{ContentKey, Digest, sha256};
use nxb_sched::{Budget, Executor, Task};
use nxb_store::Store;
use nxb_tool::{EnvPolicy, Invocation, ProcessRegistry, run_process};

use crate::cut::{Base, cut_for_slots};
use crate::script::Script;

/// Batches compiled so far and in all, for a caller's progress display: one atomic add for each batch.
#[derive(Debug, Default)]
pub struct Batches {
    /// Batches done (compiled, or taken from the store).
    pub done: std::sync::atomic::AtomicU64,
    /// Batches in all (a streamed archive adds each group's batches as the group is planned).
    pub total: std::sync::atomic::AtomicU64,
}

impl Batches {
    pub(crate) fn planned(c: &Config, n: usize) {
        if let Some(b) = &c.progress {
            b.total
                .fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub(crate) fn finished(c: &Config) {
        if let Some(b) = &c.progress {
            b.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// What to build.
#[derive(Clone, Debug)]
pub struct Config {
    /// The script (`bases.mtl4-json`).
    pub script: PathBuf,
    /// GPU architecture (`applegpu_g14s`).
    pub arch: String,
    /// `macos` or `ios`.
    pub platform: String,
    /// Minimum OS (`27.0`).
    pub osmin: String,
    /// `SDKROOT`.
    pub sdkroot: PathBuf,
    /// `metal-tt` and `metal-pack` (resolved with `xcrun -f`).
    pub metal_tt: PathBuf,
    /// `metal-pack`.
    pub metal_pack: PathBuf,
    /// `metal-tt -version` (part of every key).
    pub toolchain: String,
    /// Store root.
    pub store: PathBuf,
    /// Work folder (temporaries).
    pub work: PathBuf,
    /// The archive to write.
    pub out: PathBuf,
    /// Bases per batch (about).
    pub target: usize,
    /// Scheduler tokens (P-core equivalents) and tokens per `metal-tt`.
    pub tokens: u32,
    /// Tokens one `metal-tt` takes (it keeps about two cores busy).
    pub cost: u32,
    /// Memory one compiler run takes per 1000 bases (plus 512 MiB), and the budget for all.
    pub mem_per_batch: u64,
    /// Memory budget.
    pub mem_budget: u64,
    /// `metal-pack -nthreads`.
    pub pack_threads: u32,
    /// Run `applegpu-nt -j N` and `air-pack` directly instead of `metal-tt` (which passes no `-j`).
    pub direct: Option<Direct>,
    /// Nice value of the compiler runs (0 normal, up to 20 lowest), so they yield the CPU to a step running beside.
    pub nice: i32,
    /// Counts the batches for a progress display (`None`: not counted).
    pub progress: Option<Arc<Batches>>,
}

/// The tools `metal-tt` drives, called directly (`-j 1` costs half the CPU of `metal-tt`'s threads).
#[derive(Clone, Debug)]
pub struct Direct {
    /// `usr/metal/<version>/bin/applegpu-nt`.
    pub applegpu_nt: PathBuf,
    /// `usr/metal/<version>/bin/air-pack`.
    pub air_pack: PathBuf,
    /// The SDK version (`27.2`) for `-platform_version`.
    pub sdk_version: String,
    /// `-j`.
    pub jobs: u32,
}

impl Direct {
    /// Finds the tools beside `metal-tt` (`usr/bin/metal-tt` -> `usr/metal/<major of the toolchain>/bin`).
    pub fn find(
        metal_tt: &std::path::Path,
        toolchain: &str,
        sdk: &str,
        jobs: u32,
    ) -> Result<Direct, String> {
        let major = toolchain
            .split_whitespace()
            .last()
            .and_then(|v| v.split('.').next())
            .ok_or("toolchain version")?;
        let usr = metal_tt
            .parent()
            .and_then(|p| p.parent())
            .ok_or("metal-tt path")?;
        let bin = usr.join("metal").join(major).join("bin");
        let (nt, pack) = (bin.join("applegpu-nt"), bin.join("air-pack"));
        if !nt.exists() || !pack.exists() {
            return Err(format!("no applegpu-nt / air-pack in {}", bin.display()));
        }
        let v = std::process::Command::new("/usr/bin/xcrun")
            .args(["--sdk", sdk, "--show-sdk-version"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .map_err(|e| e.to_string())?;
        Ok(Direct {
            applegpu_nt: nt,
            air_pack: pack,
            sdk_version: v,
            jobs,
        })
    }
}

/// A batch's outcome slot (filled by its task).
type Slot = Option<Result<(PathBuf, BatchReport), String>>;

/// One batch's outcome.
#[derive(Clone, Debug)]
pub struct BatchReport {
    /// Bases.
    pub bases: usize,
    /// From the store.
    pub reused: bool,
    /// Wall time of its `metal-tt`.
    pub wall: Duration,
    /// CPU time (user + system) of its `metal-tt`.
    pub cpu: Duration,
    /// Peak RSS.
    pub max_rss: u64,
}

/// What the stage did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Per batch, in merge order.
    pub batches: Vec<BatchReport>,
    /// Seconds: hashing and cutting, compiling, merging.
    pub seconds: (f64, f64, f64),
    /// Peak memory reserved by running batches.
    pub peak_mem: u64,
    /// Most compiler runs at once (streamed archive).
    pub peak_runs: usize,
    /// Output size.
    pub size: u64,
    /// The merged archive came from the store (key: the batches' keys in merge order).
    pub merged_from_store: bool,
}

fn hash_libraries(
    paths: &[(String, String)],
    threads: usize,
) -> Result<HashMap<String, Digest>, String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out = Mutex::new(HashMap::new());
    let err = Mutex::new(None);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((label, path)) = paths.get(i) else {
                        break;
                    };
                    match fs::read(path) {
                        Ok(b) => {
                            if let Ok(mut m) = out.lock() {
                                m.insert(label.clone(), sha256(&b));
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = err.lock() {
                                *g = Some(format!("{path}: {e}"));
                            }
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = err.into_inner().ok().flatten() {
        return Err(e);
    }
    out.into_inner().map_err(|_| "lock".to_string())
}

pub(crate) fn batch_key(c: &Config, batch: &[Base]) -> ContentKey {
    let mut hashes = Vec::with_capacity(batch.len() * 32);
    for b in batch {
        hashes.extend_from_slice(&b.hash);
    }
    ContentKey::builder("nxb-archive batch 1")
        .field_str("arch", &c.arch)
        .field_str("platform", &c.platform)
        .field_str("osmin", &c.osmin)
        .field_str("toolchain", &c.toolchain)
        .field_str(
            "driver",
            &c.direct.as_ref().map_or("metal-tt".to_string(), |d| {
                format!("applegpu-nt -j {}", d.jobs)
            }),
        )
        .field("bases", &hashes)
        .finish()
}

pub(crate) fn compile(
    c: &Config,
    script: &Script,
    batch: &[Base],
    key: &ContentKey,
    store: &Store,
    reg: &ProcessRegistry,
    headroom: Option<&nxb_sched::Headroom>,
) -> Result<(PathBuf, BatchReport), String> {
    let mut rep = BatchReport {
        bases: batch.len(),
        reused: false,
        wall: Duration::ZERO,
        cpu: Duration::ZERO,
        max_rss: 0,
    };
    let piece =
        |m: &nxb_store::Manifest| m.files.first().map(|f| store.blob_path(&f.digest.sha256));
    if let Some(m) = store.lookup(key).map_err(|e| e.to_string())?
        && let Some(p) = piece(&m)
    {
        rep.reused = true;
        return Ok((p, rep));
    }
    // held until the piece is in the store: a run counts against the load until its compilers exited
    let _permit = headroom.map(nxb_sched::Headroom::acquire);
    let dir = c.work.join(key.to_hex());
    let tmp = dir.join("tmp");
    fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let js = dir.join("bases.mtl4-json");
    fs::write(&js, script.batch_text(batch)?).map_err(|e| format!("{}: {e}", js.display()))?;
    let out = dir.join(format!("{}.mtl4a", c.arch));
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let env = EnvPolicy::apple(&c.sdkroot, &tmp, &home).map_err(|e| e.to_string())?;
    let run = |name: &str, inv: Invocation| {
        run_process(
            name,
            &inv.env(env.clone())
                .cwd(&dir)
                .nice(c.nice)
                .timeout(Duration::from_secs(3600)),
            reg,
        )
        .map_err(|e| format!("{name}: {e:?}"))
    };
    let steps = match &c.direct {
        None => vec![run(
            "metal-tt",
            Invocation::new(&c.metal_tt)
                .arg("-arch")
                .arg(&c.arch)
                .arg(format!("-{}-version-min", c.platform))
                .arg(&c.osmin)
                .arg(&js)
                .arg("-o")
                .arg(&out),
        )?],
        Some(d) => {
            let nt_out = dir.join("applegpu-nt.out");
            let a = run(
                "applegpu-nt",
                Invocation::new(&d.applegpu_nt)
                    .args([
                        "-arch",
                        c.arch.as_str(),
                        "-platform_version",
                        c.platform.as_str(),
                        c.osmin.as_str(),
                        d.sdk_version.as_str(),
                    ])
                    .arg("-sysroot")
                    .arg(&c.sdkroot)
                    .arg("-N")
                    .arg(&js)
                    .arg("-o")
                    .arg(&nt_out)
                    .arg("-j")
                    .arg(d.jobs.to_string()),
            )?;
            let b = run(
                "air-pack",
                Invocation::new(&d.air_pack)
                    .arg(&nt_out)
                    .args([
                        "-pack-metallibs",
                        "internal",
                        "-pack-descriptors",
                        "internal",
                        "-pack-reflections",
                        "internal",
                        "-pack-sources",
                        "internal",
                    ])
                    .arg("-o")
                    .arg(&out),
            )?;
            vec![a, b]
        }
    };
    for done in &steps {
        rep.wall += done.wall;
        rep.cpu += done.usage.user + done.usage.system;
        rep.max_rss = rep.max_rss.max(done.usage.max_rss);
    }
    let f = fs::File::open(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let d = store.put_reader(f).map_err(|e| e.to_string())?;
    let mut w = store.entry(*key);
    w.add(&format!("{}.mtl4a", c.arch), d)
        .map_err(|e| e.to_string())?;
    let m = w.commit().map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&dir);
    piece(&m)
        .map(|p| (p, rep))
        .ok_or_else(|| "empty manifest".to_string())
}

/// Builds the archive.
pub fn build(c: &Config) -> Result<Report, String> {
    let t0 = Instant::now();
    // absolute paths: metal-tt runs in its batch folder
    let abs = |p: &PathBuf| std::path::absolute(p).map_err(|e| format!("{}: {e}", p.display()));
    let mut owned = c.clone();
    (owned.work, owned.out, owned.store, owned.script) =
        (abs(&c.work)?, abs(&c.out)?, abs(&c.store)?, abs(&c.script)?);
    let c = &owned;
    let text = fs::read_to_string(&c.script).map_err(|e| format!("{}: {e}", c.script.display()))?;
    let script = Arc::new(Script::parse(&text)?);
    drop(text);
    let libs: Vec<(String, String)> = script.library_paths()?.into_iter().collect();
    let digests = hash_libraries(&libs, 8)?;
    // about four batches per run slot (cut_for_slots), the largest first (task priority = size)
    let slots = (c.tokens / c.cost.max(1)).max(1) as usize;
    let mut batches = cut_for_slots(script.bases(&digests)?, slots, c.target);
    batches.extend(compute::chunks(script.compute_bases(&digests)?));
    let keys: Vec<ContentKey> = batches.iter().map(|b| batch_key(c, b)).collect();
    fs::create_dir_all(&c.work).map_err(|e| format!("{}: {e}", c.work.display()))?;
    let store = Arc::new(Store::open(&c.store).map_err(|e| e.to_string())?);
    let t1 = Instant::now();
    let results: Arc<Mutex<Vec<Slot>>> = Arc::new(Mutex::new(vec![None; batches.len()]));
    let reg = Arc::new(ProcessRegistry::new());
    let batches = Arc::new(batches);
    let cfg = Arc::new(c.clone());
    let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
    Batches::planned(c, batches.len());
    let tasks: Vec<Task> = (0..batches.len())
        .map(|i| {
            let (script, store, results, reg, batches, cfg, key) = (
                script.clone(),
                store.clone(),
                results.clone(),
                reg.clone(),
                batches.clone(),
                cfg.clone(),
                keys[i],
            );
            // the largest batches first (they set the wall time)
            Task::new(move |_| {
                let r = compile(&cfg, &script, &batches[i], &key, &store, &reg, None);
                Batches::finished(&cfg);
                if let Ok(mut g) = results.lock() {
                    g[i] = Some(r);
                }
            })
            .mem(c.mem_per_batch * sizes[i] as u64 / 1000 + (512 << 20))
            .cost(c.cost)
            .prio(sizes[i] as i64)
        })
        .collect();
    let exec = Executor::new(Budget::fixed(c.tokens).with_memory(c.mem_budget));
    let sched = exec.run(tasks);
    let mut report = Report {
        peak_mem: sched.peak_mem,
        ..Report::default()
    };
    let results = std::mem::take(&mut *results.lock().map_err(|_| "lock")?);
    let mut pieces = Vec::new();
    for r in results {
        let (p, b) = r.ok_or("a batch did not run")??;
        pieces.push(p);
        report.batches.push(b);
    }
    let t2 = Instant::now();
    crate::merge(c, &store, &reg, &keys, &pieces, &mut report)?;
    report.size = fs::metadata(&c.out).map(|m| m.len()).unwrap_or(0);
    report.seconds = (
        (t1 - t0).as_secs_f64(),
        (t2 - t1).as_secs_f64(),
        t2.elapsed().as_secs_f64(),
    );
    Ok(report)
}

/// Merge the pieces (in the given order) into `c.out`, or restore the merged archive from the store when the same
/// batches were merged before.
pub(crate) fn merge(
    c: &Config,
    store: &Store,
    reg: &ProcessRegistry,
    keys: &[ContentKey],
    pieces: &[PathBuf],
    report: &mut Report,
) -> Result<(), String> {
    // the merged archive, keyed by the ordered batch keys: an unchanged rebuild restores it instead of merging
    let mut merged = ContentKey::builder("nxb-archive merged 1")
        .field_str("arch", &c.arch)
        .field_str("toolchain", &c.toolchain)
        .field_u64("pack-threads", c.pack_threads as u64);
    for k in keys {
        merged = merged.field_digest("batch", k.digest());
    }
    let merged = merged.finish();
    let part = c.out.with_extension("mtl4a.partial");
    let _ = fs::remove_file(&part);
    let cached = store
        .lookup(&merged)
        .map_err(|e| e.to_string())?
        .and_then(|m| m.files.first().map(|f| store.blob_path(&f.digest.sha256)));
    if let Some(blob) = cached {
        fs::copy(&blob, &part).map_err(|e| format!("{}: {e}", blob.display()))?;
        report.merged_from_store = true;
    } else {
        let tmp = c.work.join("pack-tmp");
        fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let inv = Invocation::new(&c.metal_pack)
            .arg("-nthreads")
            .arg(c.pack_threads.to_string())
            .args(pieces.iter().map(|p| p.as_os_str().to_owned()))
            .arg("-o")
            .arg(&part)
            .env(EnvPolicy::apple(&c.sdkroot, &tmp, &home).map_err(|e| e.to_string())?)
            .timeout(Duration::from_secs(3600));
        run_process("metal-pack", &inv, reg).map_err(|e| format!("metal-pack: {e:?}"))?;
        let f = fs::File::open(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let d = store.put_reader(f).map_err(|e| e.to_string())?;
        let mut w = store.entry(merged);
        w.add(&format!("{}.mtl4a", c.arch), d)
            .map_err(|e| e.to_string())?;
        w.commit().map_err(|e| e.to_string())?;
    }
    fs::rename(&part, &c.out).map_err(|e| format!("{}: {e}", c.out.display()))?;
    report.size = fs::metadata(&c.out).map(|m| m.len()).unwrap_or(0);
    Ok(())
}

impl Config {
    /// The defaults measured best, with the tools found by `xcrun`: `applegpu-nt -j 1` + `air-pack` per
    /// batch (else `metal-tt`), batches of about 1,000 to 2,000 bases, one run per core, 1.9 GiB per 1,000 bases within
    /// physical memory less 6 GiB, `metal-pack -nthreads 1`, the store in `<work>/store`.
    pub fn for_tools(
        platform: &str,
        arch: &str,
        osmin: &str,
        script: &std::path::Path,
        out: &std::path::Path,
        work: &std::path::Path,
    ) -> Result<Config, String> {
        let sdk = if platform == "ios" {
            "iphoneos"
        } else {
            "macosx"
        };
        let metal_tt = xcrun_find(sdk, "metal-tt")?;
        let metal_pack = xcrun_find(sdk, "metal-pack")?;
        let sdkroot = std::process::Command::new("/usr/bin/xcrun")
            .args(["-sdk", sdk, "--show-sdk-path"])
            .output()
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            .map_err(|e| e.to_string())?;
        let toolchain = std::process::Command::new(&metal_tt)
            .arg("-version")
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string()
            })
            .map_err(|e| e.to_string())?;
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8) as u32;
        Ok(Config {
            script: script.to_path_buf(),
            arch: arch.into(),
            platform: platform.into(),
            osmin: osmin.into(),
            sdkroot,
            metal_tt,
            metal_pack,
            toolchain,
            store: work.join("store"),
            work: work.to_path_buf(),
            out: out.to_path_buf(),
            target: 2000,
            tokens: cores,
            cost: 1,
            mem_per_batch: (1.9 * (1u64 << 30) as f64) as u64,
            // physical memory less 6 GiB for the system and the other stages
            mem_budget: nxb_sys::system::memsize()
                .map_or_else(Budget::default_memory, |m| m.saturating_sub(6 << 30)),
            pack_threads: 1,
            direct: None,
            nice: 0,
            progress: None,
        })
        .map(|mut c: Config| {
            // applegpu-nt -j 1 + air-pack, one run per core: half the CPU of metal-tt's own threads.
            // metal-tt when the tools are not where the toolchain keeps them
            c.direct = Direct::find(&c.metal_tt, &c.toolchain, sdk, 1).ok();
            if c.direct.is_none() {
                (c.cost, c.target, c.mem_per_batch) = (2, 1000, (2.2 * (1u64 << 30) as f64) as u64);
            }
            c
        })
    }
}

/// `xcrun -sdk <sdk> -f <tool>`.
pub fn xcrun_find(sdk: &str, tool: &str) -> Result<PathBuf, String> {
    let o = std::process::Command::new("/usr/bin/xcrun")
        .args(["-sdk", sdk, "-f", tool])
        .output()
        .map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("xcrun -f {tool} failed"));
    }
    Ok(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
}
