//! Compiling one batch of bases (`compile`, `compile_resilient`), its content key and the outcome types.

use super::*;

/// A batch's outcome slot (filled by its task).
pub(super) type Slot = Option<Result<Compiled, String>>;

/// A recorded state that the compiler refused and the archive lacks, with the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    /// The functions of the pipeline.
    pub what: String,
    /// The first line of what the tool said.
    pub reason: String,
}

/// One compiled piece of a batch: its key, its archive, its report and the bases in it.
pub(crate) type PieceOf = (ContentKey, PathBuf, BatchReport, Vec<Base>);

/// What [`compile_resilient`] made of a batch: one piece, or several when entries had to be left out.
pub(crate) struct Compiled {
    pub(crate) pieces: Vec<PieceOf>,
    pub(crate) dropped: Vec<Dropped>,
}

/// The part of a tool error to show: the text after the last `error:` up to the end of the line or the string, else the
/// whole message cut to one line.
pub(crate) fn short_reason(e: &str) -> String {
    let t = e.rfind("error:").map_or(e, |i| &e[i + "error:".len()..]);
    let t = t.split(['\n', '"']).next().unwrap_or(t);
    // a debug-printed message carries `\n` as two characters
    let t = t.split("\\n").next().unwrap_or(t).trim();
    t.chars().take(300).collect()
}

/// [`compile`] for a batch that may hold recorded states. When the compile fails and the batch holds states, the plain
/// bases are compiled alone and the states in groups that are cut in halves until each group compiles, so that only
/// the states the compiler refuses are left out (reported in `dropped`); the rest of the batch is kept. A batch of
/// plain bases only, or whose plain bases fail, is an error as before.
pub(crate) fn compile_resilient(
    c: &Config,
    script: &Script,
    batch: &[Base],
    store: &Store,
    reg: &ProcessRegistry,
    headroom: Option<&nxb_sched::Headroom>,
) -> Result<Compiled, String> {
    let one = |b: &[Base]| -> Result<PieceOf, String> {
        let key = batch_key(c, b);
        let (p, r) = compile(c, script, b, &key, store, reg, headroom)?;
        Ok((key, p, r, b.to_vec()))
    };
    let first = one(batch);
    let err = match first {
        Ok(p) => {
            return Ok(Compiled {
                pieces: vec![p],
                dropped: Vec::new(),
            });
        }
        Err(e) => e,
    };
    let (extras, plain): (Vec<Base>, Vec<Base>) =
        batch.iter().copied().partition(|b| script.is_extra(b));
    if extras.is_empty() {
        return Err(err);
    }
    let mut out = Compiled {
        pieces: Vec::new(),
        dropped: Vec::new(),
    };
    if !plain.is_empty() {
        out.pieces.push(one(&plain)?);
    }
    let mut work = vec![extras];
    while let Some(group) = work.pop() {
        match one(&group) {
            Ok(p) => out.pieces.push(p),
            Err(e) if group.len() == 1 => out.dropped.push(Dropped {
                what: script.describe(&group[0]),
                reason: short_reason(&e),
            }),
            Err(_) => {
                let (a, b) = group.split_at(group.len() / 2);
                work.push(b.to_vec());
                work.push(a.to_vec());
            }
        }
    }
    Ok(out)
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
