//! The shader step and the pipeline archive in one process graph: `nxb_stages::build::build` is called as a library
//! (no `shaderbuild` process) and reports every finished shader, the archive's pair groups (`nxb_archive::stream`) start
//! their `metal-tt` batches as soon as their shaders are done, on worker threads beside the shader step, and the shader
//! step translates the archive's shaders first so the groups complete one after another.

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;
use nxb_pyjson::Value;
use nxb_stages::build::{BuildConfig, OnDone};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The shader step's configuration from the build's plan: the chain version
/// and the profile's inputs digest computed as `dmsh_chain.py` does, outputs in `<work>/shader/cache/b-<V>`.
pub fn shader_config(a: &Args, b: &Build) -> Result<BuildConfig, String> {
    // a build without an engine folder (the standalone one, R49) has its own chain version
    let (chain, inputs) = if b.engine.as_os_str().is_empty() {
        (
            nxb_shader::chain::standalone_version(),
            nxb_shader::chain::inputs(&b.profile.0)?,
        )
    } else {
        (
            nxb_shader::chain::version(&b.engine)?,
            nxb_shader::chain::inputs_of(&b.engine, &b.profile.0)?,
        )
    };
    let sh = b.work.join("shader");
    let manifest = match &a.manifest {
        Some(f) => nxb_sched::UseManifest::read(f)?.keys().to_vec(),
        None => Vec::new(),
    };
    Ok(BuildConfig {
        pkg: sh.join("pkg"),
        out: sh.join("cache"),
        profile: b.profile.0.clone(),
        chain,
        game: b.title.clone(),
        inputs,
        store: sh.join("store"),
        work: sh.join("work"),
        scratch: std::env::temp_dir().join(format!("nxb-build-{}", std::process::id())),
        jobs: a.threads.max(1) as u32,
        batch: 128,
        batch_first: 16,
        keys: Vec::new(),
        own_front: (a.own_front != "off").then(|| {
            let mut c = nxb_stages::own_front::default_config();
            if !a.own_front.is_empty() && a.own_front != "on" {
                c.verdicts = PathBuf::from(&a.own_front);
            }
            c
        }),
        on_done: None,
        on_start: None,
        order: Vec::new(),
        manifest,
        partial: a.partial,
        ios_out: a.also_ios.then(|| sh.join("ios")),
    })
}

/// Where this build's shader step writes `c1_full.tsv` and `cb_extent.tsv` (`nxb_stages::guard_tables`). None with
/// `--shaders`, which names a library folder the step did not build: the tables then come from the profile.
pub(crate) fn guard_tables(a: &Args, b: &Build) -> Option<PathBuf> {
    a.shaders
        .is_none()
        .then(|| b.work.join("shader/work/tables"))
}

/// The compute shaders that get compute pipelines in the archive: the profile's prewarm list (`pso_manifest.tsv`, `C`
/// rows first) and the compute rows of its `shader_map.tsv`, as `pso_offline.py --game` reads them.
fn compute_shaders(b: &Build) -> Vec<String> {
    nxb_archive::compute::read_computes(
        &[b.profile.0.join("shader_map.tsv")],
        &[b.profile.0.join("pso_manifest.tsv")],
    )
}

/// The archive's inputs for one architecture over the library folder the shader step writes.
fn archive_config(b: &Build, lib: &Path, arch: &str) -> Result<nxb_archive::Config, String> {
    let (out, work) = (b.work.join("pso/mac"), b.work.join("pso/mac-work"));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let abs = |p: &Path| std::path::absolute(p).map_err(|e| e.to_string());
    let mut c = nxb_archive::Config::for_tools(
        "macos",
        arch,
        &osmin()?,
        &work.join("bases.mtl4-json"),
        &abs(&out.join(format!("{arch}.mtl4a")))?,
        &abs(&work)?,
    )?;
    c.store = abs(&c.store)?;
    let _ = lib;
    Ok(c)
}

/// Pairs per streamed group (about).
const GROUP_TARGET: usize = 400;

/// Compiler runs the streamed archive always has, whatever the load.
const MIN_RUNS: usize = 3;

/// Nice value of the archive's compiler runs: the shader step beside them is the critical path and keeps the cores
/// it needs.
const ARCHIVE_NICE: i32 = 10;

/// The streamed archive's run limits: at most one compiler run per slot and as many as the memory budget holds for a
/// group of twice the average size (a group is at most 4x, PROGRESS X8: about 1.9 GiB per 1,000 bases), at least one.
/// [`MIN_RUNS`] always go when the budget holds them. In between, runs start while CPU and memory stay below
/// `load_percent`.
fn run_limits(c: &nxb_archive::Config, load_percent: u8) -> nxb_sched::Limits {
    let slots = u64::from((c.tokens / c.cost.max(1)).max(1));
    let per = c.mem_per_batch * (2 * GROUP_TARGET as u64) / 1000 + (512 << 20);
    let fit = slots.min(c.mem_budget / per.max(1)).max(1);
    let floor = fit.min(MIN_RUNS as u64) as usize;
    let ceiling = fit as usize;
    let share = f64::from(load_percent.clamp(10, 100)) / 100.0;
    nxb_sched::Limits {
        cpu: share,
        mem: share,
        ..nxb_sched::Limits::new(floor, ceiling, per)
    }
}

/// What the combined step did.
pub struct Outcome {
    /// The shader step's summary.
    pub shaders: String,
    /// The archive's summary (when one was built).
    pub archive: Option<String>,
    /// Why the archive failed: the script treats that as no archive for the app, not as a failed build.
    pub archive_error: Option<String>,
    /// The library folder.
    pub lib: PathBuf,
}

/// Run the shader step and, for the first `--gpu` architecture, the streamed archive beside it.
pub fn shaders_and_archive(a: &Args, b: &Build, p: &Arc<Progress>) -> Result<Outcome, String> {
    let mut cfg = shader_config(a, b)?;
    let lib = cfg.out.join(format!("b-{}", cfg.chain));
    let arch = b.gpus.first().cloned();
    let zero = b.profile.1.get("depth_mode").map(String::as_str) == Some("zero");
    let pair_files = [
        b.profile.0.join("shader_pairs.tsv"),
        b.work.join("shader/pso_pairs.tsv"),
    ];
    let text = nxb_archive::plan::pair_text(&pair_files);
    let batches = Arc::new(nxb_archive::Batches::default());
    let acfg = match &arch {
        Some(arch) => Some(nxb_archive::Config {
            nice: ARCHIVE_NICE,
            progress: Some(batches.clone()),
            ..archive_config(b, &lib, arch)?
        }),
        None => None,
    };
    let sinp = nxb_archive::stream::StreamInputs {
        lib_dir: std::path::absolute(&lib).map_err(|e| e.to_string())?,
        chain: cfg.chain.clone(),
        pairs: nxb_archive::plan::read_pairs(&text),
        computes: compute_shaders(b),
        zero,
        group_target: GROUP_TARGET,
        limits: acfg.as_ref().map_or_else(
            || nxb_sched::Limits::new(MIN_RUNS, MIN_RUNS, 0),
            |c| run_limits(c, a.load_limit),
        ),
    };
    let stream = arch
        .as_ref()
        .map(|_| Arc::new(nxb_archive::stream::Stream::new(&sinp)));
    // the shaders are counted by one atomic add each: the progress reads it, no lock in the parallel loop
    let count = Arc::new(crate::events::Count::default());
    {
        let (st, count) = (stream.clone(), count.clone());
        cfg.on_done = Some(OnDone(Arc::new(move |key: &str, _status: &str| {
            count.add(1);
            if let Some(s) = &st {
                s.done(key);
            }
        })));
    }
    if let Some(s) = &stream {
        cfg.order = s.key_order();
    }
    {
        // the total is reported with or without an archive: the app binary shows a percent bar and an ETA from it
        let (s, count) = (stream.clone(), count.clone());
        cfg.on_start = Some(nxb_stages::build::OnStart(Arc::new(
            move |keys: &[String]| {
                if let Some(s) = &s {
                    s.building(&keys.iter().cloned().collect());
                }
                count.set_total(keys.len() as u64);
            },
        )));
    }
    p.set(Update {
        step: Some("translating and compiling"),
        unit: Some("shaders"),
        ..Default::default()
    });
    let (shaders, archive) = std::thread::scope(
        |scope| -> Result<(String, Option<Result<String, String>>), String> {
            let job = match (&stream, acfg) {
                (Some(s), Some(c)) => {
                    let (s, sinp) = (s.clone(), sinp.clone());
                    Some(scope.spawn(move || s.run(&c, &sinp).map(|r| (r, c))))
                }
                _ => None,
            };
            let r = p
                .counted(None, "shaders", &count, || nxb_stages::build::build(cfg))
                .map_err(|e| e.to_string());
            if let Some(s) = &stream {
                s.finish();
            }
            let r = r?;
            let ok = r.results.iter().filter(|x| x.1 == "ok").count();
            let mut shaders = format!(
                "{ok} of {} shaders, {}; {} translations from the store",
                r.results.len(),
                r.own_note,
                r.xlate_hits
            );
            let partial = nxb_sched::Partial::read(&lib);
            if let (true, Some(st)) = (a.partial, &partial) {
                shaders = format!("{shaders}; {}", st.note());
            }
            p.say_with(
                &format!("shaders done in {:.1} s ({shaders})", r.wall.as_secs_f64()),
                vec![
                    ("xlate_hits".into(), Value::Int(r.xlate_hits as i64)),
                    (
                        "xlate_refused".into(),
                        Value::Object(
                            r.xlate_refused
                                .iter()
                                .map(|(k, n)| (k.clone(), Value::Int(*n as i64)))
                                .collect(),
                        ),
                    ),
                ],
            );
            let archive = match job {
                Some(j) => {
                    p.set(Update {
                        step: Some("archive batches still running"),
                        ..Default::default()
                    });
                    let done = p
                        .tracked(
                            Some("archive batches still running"),
                            "batches",
                            || {
                                (
                                    batches.done.load(Ordering::Relaxed),
                                    batches.total.load(Ordering::Relaxed),
                                )
                            },
                            || j.join(),
                        )
                        .map_err(|_| "archive thread panicked".to_string())
                        .and_then(|r| r)
                        .and_then(|((rep, bases), c)| {
                            let arch = arch.clone().unwrap_or_default();
                            write_archive_stamps(b, &lib, &c, &arch, &text, zero, bases)?;
                            Ok((rep, bases))
                        });
                    match done {
                        Ok((rep, bases)) => {
                            let tail = partial
                                .filter(|_| a.partial)
                                .map_or(String::new(), |st| format!("; {}", st.note()));
                            Some(Ok(format!(
                                "{bases} pipelines, {} MB, {} batches ({} reused, up to {} compiler runs at once), streamed beside the shader step{tail}",
                                rep.size >> 20,
                                rep.batches.len(),
                                rep.batches.iter().filter(|x| x.reused).count(),
                                rep.peak_runs
                            )))
                        }
                        Err(e) => Some(Err(archive_failure(&e))),
                    }
                }
                None => None,
            };
            Ok((shaders, archive))
        },
    )?;
    let (archive, archive_error) = match archive {
        Some(Ok(s)) => (Some(s), None),
        Some(Err(e)) => (None, Some(e)),
        None => (None, None),
    };
    Ok(Outcome {
        shaders,
        archive,
        archive_error,
        lib,
    })
}

/// An archive failure in words: the common one, no pair with libraries of the chain, says what it means.
pub(crate) fn archive_failure(e: &str) -> String {
    if e.contains("not enough input files") {
        "the pipeline archive has no base to pack: no shader pair has libraries of this build's chain (their `_b.json` names another chain than the folder `b-<chain>`) or the profile has no usable pairs".into()
    } else {
        e.to_string()
    }
}

/// The minimum OS of the Mac archive: this Mac's major version (`sw_vers`), as `pso_offline.py`.
pub(crate) fn osmin() -> Result<String, String> {
    std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .split('.')
                .next()
                .unwrap_or("27")
                .to_string()
                + ".0"
        })
        .map_err(|e| e.to_string())
}

/// `prebuilt.tsv`, `archive.tsv` and the script `bases.mtl4-json` of a streamed archive, from the whole plan as
/// `pso_offline.py` (and the non-streamed [`stage_pso`]) writes them; the streamed groups must have compiled exactly the
/// plan's bases.
pub(crate) fn write_archive_stamps(
    b: &Build,
    lib: &Path,
    c: &nxb_archive::Config,
    arch: &str,
    text: &[u8],
    zero: bool,
    bases: usize,
) -> Result<(), String> {
    use nxb_archive::plan;
    let chain = lib
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("b-"))
        .ok_or("library folder is not b-<chain>")?;
    let p = plan::plan(lib, chain, &plan::read_pairs(text), zero);
    let (pipes, _) = nxb_archive::compute::plan_compute(lib, chain, &compute_shaders(b));
    if p.bases.len() + pipes.len() != bases {
        return Err(format!(
            "streamed archive: {bases} pipelines compiled, the plan has {} bases and {} compute pipelines",
            p.bases.len(),
            pipes.len()
        ));
    }
    let out = b.work.join("pso/mac");
    let js = b.work.join("pso/mac-work/bases.mtl4-json");
    std::fs::write(&js, plan::script_text_with(lib, &p.bases, &pipes))
        .map_err(|e| e.to_string())?;
    std::fs::write(out.join("prebuilt.tsv"), plan::stamp(chain, zero, text))
        .map_err(|e| e.to_string())?;
    let tsv = plan::archive_tsv(
        &[arch.to_string()],
        "macos",
        &c.osmin,
        &c.toolchain,
        p.bases.len(),
        p.rest.len(),
        pipes.len(),
    );
    std::fs::write(out.join("archive.tsv"), tsv).map_err(|e| e.to_string())
}

/// One architecture's pipeline archive: the plan of `pso_offline.py` (profile pairs, then the split's pso_pairs.tsv)
/// and `nxb_archive::build` (content-cut batches in the store). Output in `<work>/pso/mac`, as the NSP script's.
pub(crate) fn stage_pso(b: &Build, arch: &str, p: &Progress) -> Result<String, String> {
    use nxb_archive::plan;
    let pairs = [
        b.profile.0.join("shader_pairs.tsv"),
        b.work.join("shader/pso_pairs.tsv"),
    ];
    let text = plan::pair_text(&pairs);
    let chain = b
        .shaders
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_prefix("b-"))
        .ok_or("library folder is not b-<chain>")?;
    let zero = b.profile.1.get("depth_mode").map(String::as_str) == Some("zero");
    let plan = plan::plan(&b.shaders, chain, &plan::read_pairs(&text), zero);
    let (out, work) = (b.work.join("pso/mac"), b.work.join("pso/mac-work"));
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let (pipes, _) = nxb_archive::compute::plan_compute(&b.shaders, chain, &compute_shaders(b));
    let js = work.join("bases.mtl4-json");
    std::fs::write(&js, plan::script_text_with(&b.shaders, &plan.bases, &pipes))
        .map_err(|e| e.to_string())?;
    let osmin = osmin()?;
    let batches = Arc::new(nxb_archive::Batches::default());
    let c = nxb_archive::Config {
        progress: Some(batches.clone()),
        ..nxb_archive::Config::for_tools(
            "macos",
            arch,
            &osmin,
            &js,
            &out.join(format!("{arch}.mtl4a")),
            &work,
        )?
    };
    let r = p.tracked(
        None,
        "batches",
        || {
            (
                batches.done.load(Ordering::Relaxed),
                batches.total.load(Ordering::Relaxed),
            )
        },
        || nxb_archive::build(&c),
    )?;
    std::fs::write(out.join("prebuilt.tsv"), plan::stamp(chain, zero, &text))
        .map_err(|e| e.to_string())?;
    std::fs::write(
        out.join("archive.tsv"),
        plan::archive_tsv(
            &[arch.to_string()],
            "macos",
            &osmin,
            &c.toolchain,
            plan.bases.len(),
            plan.rest.len(),
            pipes.len(),
        ),
    )
    .map_err(|e| e.to_string())?;
    let tail =
        nxb_sched::Partial::read(&b.shaders).map_or(String::new(), |st| format!("; {}", st.note()));
    Ok(format!(
        "{} pipelines, {} MB, {} batches{tail}",
        plan.bases.len() + pipes.len(),
        r.size >> 20,
        r.batches.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::{archive_failure, run_limits};

    #[test]
    fn run_limits_from_cores_and_memory() {
        let gib = 1u64 << 30;
        let mut c = nxb_archive::Config {
            tokens: 12,
            cost: 1,
            mem_per_batch: gib * 19 / 10,
            mem_budget: 26 * gib,
            ..test_config()
        };
        let l = run_limits(&c, 90);
        // 26 GiB / (1.9 GiB × 0.8 + 0.5 GiB) = 12.8, 12 slots
        assert_eq!((l.floor, l.ceiling), (3, 12));
        assert!((l.cpu - 0.9).abs() < 1e-9 && (l.mem - 0.9).abs() < 1e-9);
        // 16 GB Mac: 10 GiB budget, 4 runs fit
        c.mem_budget = 10 * gib;
        let l = run_limits(&c, 50);
        assert_eq!((l.floor, l.ceiling), (3, 4));
        // 8 GB Mac: 2 GiB budget, one run, not three regardless of memory
        c.mem_budget = 2 * gib;
        let l = run_limits(&c, 90);
        assert_eq!((l.floor, l.ceiling), (1, 1));
        c.mem_budget = 0;
        assert_eq!(run_limits(&c, 90).ceiling, 1);
        // a 4-core Mac with metal-tt (two cores a run): 2 slots
        (c.tokens, c.cost, c.mem_budget) = (4, 2, 26 * gib);
        let l = run_limits(&c, 90);
        assert_eq!((l.floor, l.ceiling), (2, 2));
    }

    fn test_config() -> nxb_archive::Config {
        let p = std::path::PathBuf::new;
        nxb_archive::Config {
            script: p(),
            arch: String::new(),
            platform: String::new(),
            osmin: String::new(),
            sdkroot: p(),
            metal_tt: p(),
            metal_pack: p(),
            toolchain: String::new(),
            store: p(),
            work: p(),
            out: p(),
            target: 2000,
            tokens: 1,
            cost: 1,
            mem_per_batch: 0,
            mem_budget: 0,
            pack_threads: 1,
            direct: None,
            nice: 0,
            progress: None,
        }
    }

    #[test]
    fn archive_failures_in_words() {
        let raw = "metal-pack: ToolError { stderr_head: \"metal-pack: error: not enough input files\\n\" }";
        assert!(archive_failure(raw).starts_with("the pipeline archive has no base to pack"));
        assert_eq!(archive_failure("disk full"), "disk full");
    }
}
