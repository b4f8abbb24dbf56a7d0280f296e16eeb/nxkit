//! The extract stage for every kind of input ([`crate::inputs`]): packages (extract and shader split fused in one
//! pass), a NUP container or a dump folder (one image per version, the split after them), and the DLC images
//! (`<work>/aoc/<index>.img`, index = DLC title ID - base title ID - 0x1000, the host mounts them by index).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;
use crate::inputs::Source;

fn split_outputs(b: &Build) -> nxb_extract::split::SplitOutputs {
    nxb_extract::split::SplitOutputs {
        pkg: b.work.join("shader/pkg"),
        map: b.work.join("shader/shader_map.tsv"),
        facts: b.work.join("shader/shader_facts.tsv"),
        pairs: b.work.join("shader/shader_pairs.tsv"),
        pso_pairs: b.work.join("shader/pso_pairs.tsv"),
    }
}

/// The keys file (`-k`): required and present for packages.
pub(crate) fn keys(a: &Args) -> Result<PathBuf, String> {
    let k = a
        .keys
        .clone()
        .ok_or_else(|| crate::exit::fail(2, "-k/--keys is required to extract packages"))?;
    if !k.is_file() {
        return Err(crate::exit::fail(
            6,
            format!("the keys file {} does not exist (-k/--keys)", k.display()),
        ));
    }
    Ok(k)
}

pub(crate) fn damaged(p: &Progress, e: &nxb_extract::ExtractError) -> String {
    if let nxb_extract::ExtractError::Damaged(files) = e {
        for f in files.iter().take(20) {
            p.say(&format!("[RED] hash check failed: {f}"));
        }
        p.say(
            "[RED] the package is damaged (--allow-damaged writes the files as read and goes on)",
        );
    }
    crate::exit::fail(e.exit_code(), e.to_string())
}

/// `v327680` / `v0` as a number.
fn number(v: &str) -> Option<u32> {
    v.strip_prefix('v').and_then(|n| n.parse().ok())
}

/// The versions a NUP or a dump gives images for: the packed one, those the split reads, and the base game.
fn needed(b: &Build) -> Vec<u32> {
    let mut v: Vec<u32> = b
        .split_versions
        .iter()
        .chain([&b.package])
        .filter_map(|s| number(s))
        .collect();
    if b.src.has_base {
        v.push(0);
    }
    v.sort_unstable();
    v.dedup();
    v
}

/// One stat line per DLC package: what a later build compares to see that the DLC images are current.
fn dlc_key(b: &Build) -> String {
    let mut rows: Vec<String> = b
        .src
        .dlc
        .iter()
        .map(|(t, v, path)| {
            let m = std::fs::metadata(path);
            use std::os::unix::fs::MetadataExt;
            format!(
                "{t:016x} {v} {} {} {}",
                path.display(),
                m.as_ref().map_or(0, |m| m.len()),
                m.as_ref()
                    .map_or(0, |m| m.mtime_nsec() + m.mtime() * 1_000_000_000)
            )
        })
        .collect();
    rows.sort();
    format!("{}\n", rows.join("\n"))
}

/// DLC packages into `<work>/aoc/<index>.img`. Reused while the packages are as at the last extraction.
pub(crate) fn dlc(a: &Args, b: &Build, p: &Progress) -> Result<Option<String>, String> {
    if b.src.dlc.is_empty() || b.src.source != Source::Packages {
        return Ok(None);
    }
    let keys = keys(a)?;
    let (dst, tmp, kf) = (
        b.work.join("aoc"),
        b.work.join("aoc.partial"),
        b.work.join("aoc.key"),
    );
    let key = dlc_key(b);
    let idx = |t: u64| t.wrapping_sub(b.src.app).wrapping_sub(0x1000);
    if std::fs::read_to_string(&kf).is_ok_and(|k| k == key)
        && b.src
            .dlc
            .iter()
            .all(|d| dst.join(format!("{}.img", idx(d.0))).is_file())
    {
        return Ok(Some(format!("{} DLC reused", b.src.dlc.len())));
    }
    let _ = std::fs::remove_file(&kf);
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let n = b.src.dlc.len() as u64;
    for (i, (tid, _, pkg)) in b.src.dlc.iter().enumerate() {
        p.set(Update {
            step: Some("extracting DLC"),
            done: Some(i as u64),
            total: Some(n),
            unit: Some("DLC"),
            ..Default::default()
        });
        p.say(&format!(
            "DLC index {}: {}",
            idx(*tid),
            pkg.file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        ));
        let part = tmp.join(format!("{}.d", idx(*tid)));
        let req = nxb_extract::Request {
            keys: keys.clone(),
            base: pkg.clone(),
            out_base: part.clone(),
            threads: a.threads,
            allow_damaged: a.allow_damaged,
            ..nxb_extract::Request::default()
        };
        // a merged package or a card image holds more than one title: name the DLC
        let id = b.src.app_flag.map(|_| *tid);
        let s = nxb_extract::aoc::aoc(&req, id).map_err(|e| damaged(p, &e))?;
        s.closing.wait();
        std::fs::rename(
            part.join("RomFS.img"),
            tmp.join(format!("{}.img", idx(*tid))),
        )
        .map_err(|e| format!("DLC {tid:016X}: {e}"))?;
        let _ = std::fs::remove_dir_all(&part);
    }
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::rename(&tmp, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    std::fs::write(&kf, key).map_err(|e| format!("{}: {e}", kf.display()))?;
    Ok(Some(format!("{n} DLC")))
}

/// Runs `f` while a thread reports `counter`'s bytes as the stage's progress, as the script's poll of the extract
/// folder reports them: a determinate bar with an ETA.
fn with_bytes<T>(
    p: &Progress,
    step: &str,
    counter: &nxb_extract::Counter,
    f: impl FnOnce() -> T,
) -> T {
    p.tracked(
        Some(step),
        "B",
        || {
            (
                counter.done.load(Ordering::Relaxed),
                counter.total.load(Ordering::Relaxed),
            )
        },
        f,
    )
}

/// Extracts the packages of the input (and the split in the same pass). Returns the image count and, when the fused
/// split needs the game's own decoder (Oodle, LZ5), why: the split then runs again once the Mac host can decode.
pub(crate) fn packages(
    a: &Args,
    b: &Build,
    p: &Progress,
) -> Result<(usize, Option<String>), String> {
    let out = |v: &str| b.work.join("extract").join(v);
    let counter = Arc::new(nxb_extract::Counter::default());
    let req = nxb_extract::Request {
        keys: keys(a)?,
        base: b.base.clone(),
        out_base: out("v0"),
        updates: b
            .updates
            .iter()
            .map(|(v, path)| (path.clone(), out(&format!("v{v}"))))
            .collect(),
        threads: a.threads,
        mem_limit: 1 << 30,
        cache_chunks: 512,
        digests: None,
        html: true,
        base_outputs: true,
        app: b.src.app_flag,
        program: (b.program != 0).then_some(b.src.app),
        discard_images: false,
        allow_damaged: a.allow_damaged,
        manifest: false,
        split: Some((b.profile.0.join("game.tsv"), split_outputs(b))),
        progress: Some(counter.clone()),
    };
    p.say("extract and shader split in one pass (nxb-extract)");
    let s = with_bytes(p, "extracting", &counter, || nxb_extract::extract(&req))
        .map_err(|e| damaged(p, &e))?;
    Ok((s.images.len(), s.split_unsupported.clone()))
}

/// A NUP for a build without the engine: `extract/v<version>/` with the ExeFS, the control data, the RomFS as a folder
/// (`romfs/`, no image) and each DLC's RomFS as `aoc/<index>/`, and the shader split in the same pass. Returns the
/// summary and, when the split needs the game's own decoder, why.
pub(crate) fn nup_loose(
    a: &Args,
    b: &Build,
    p: &Progress,
    version: u32,
) -> Result<(String, Option<String>), String> {
    let dir = b.work.join("extract").join(format!("v{version}"));
    let counter = Arc::new(nxb_extract::Counter::default());
    let req = nxb_extract::nup::UnpackRequest {
        nup: b.base.clone(),
        version: Some(version),
        out: dir.clone(),
        romfs: dir.join("romfs"),
        aoc: dir.join("aoc"),
        threads: a.threads,
        mem_limit: 2 << 30,
        verify: true,
        split: Some((b.profile.0.join("game.tsv"), split_outputs(b))),
        progress: Some(counter.clone()),
    };
    p.say("unpack and shader split in one pass (nxb-extract, RomFS as a folder)");
    let u = with_bytes(p, "unpacking", &counter, || {
        nxb_extract::nup::unpack_nup(&req)
    })
    .map_err(|e| damaged(p, &e))?;
    for l in &u.notes {
        p.say(l);
    }
    let text = format!(
        "{} files, {:.1} GB{}",
        u.files,
        u.bytes as f64 / 1e9,
        if u.dlc.is_empty() {
            String::new()
        } else {
            format!(", {} DLC", u.dlc.len())
        }
    );
    Ok((text, u.split_unsupported))
}

/// A NUP or a dump: one `extract/v<version>` per needed version.
pub(crate) fn container(a: &Args, b: &Build, p: &Progress) -> Result<usize, String> {
    let versions = needed(b);
    let n = versions.len() as u64;
    for (i, v) in versions.iter().enumerate() {
        // the survey of a provisional build wrote the images already
        if b.prov
            && b.work
                .join("extract")
                .join(format!("v{v}/RomFS.img"))
                .is_file()
        {
            continue;
        }
        p.set(Update {
            step: Some(&format!("unpacking v{v}")),
            done: Some(i as u64),
            total: Some(n),
            unit: Some("versions"),
            ..Default::default()
        });
        let counter = Arc::new(nxb_extract::Counter::default());
        let step = if n > 1 {
            format!("unpacking v{v} ({}/{n})", i + 1)
        } else {
            format!("unpacking v{v}")
        };
        let r = with_bytes(p, &step, &counter, || match b.src.source {
            Source::Nup => nxb_extract::nup::extract_nup(&nxb_extract::nup::NupRequest {
                nup: b.base.clone(),
                version: Some(*v),
                work: b.work.clone(),
                threads: a.threads,
                mem_limit: 1 << 30,
                discard_images: false,
                digests: None,
                skip_images: false,
                progress: Some(counter.clone()),
            })
            .map(|_| ()),
            _ => nxb_extract::dump::extract_dump(&nxb_extract::dump::DumpRequest {
                dump: b.base.clone(),
                version: Some(*v),
                work: b.work.clone(),
                threads: a.threads,
                mem_limit: 1 << 30,
                discard_images: false,
                digests: None,
                progress: Some(counter.clone()),
            })
            .map(|_| ()),
        });
        r.map_err(|e| damaged(p, &e))?;
    }
    Ok(versions.len())
}

/// The shader split of a NUP or a dump (the packages' split runs inside the extract's pass).
pub(crate) fn split(a: &Args, b: &Build, p: &Progress) -> Result<String, String> {
    let inputs = crate::boot::inputs::HostInputs::load(&b.work);
    // the game's own decoder: the Mac host the boot steps built, serving with the rewritten modules
    let oodle = match &inputs.oodle {
        Some((host, modules)) => {
            let c = crate::boot::Ctx::of(b, p)?;
            let (cwd, env) = c.run_dir("oodle_serve", false)?;
            Some(nxb_extract::oodle_host::HostSpec {
                host: host.clone(),
                modules: modules.clone(),
                env,
                cwd,
                logs: c.logs(),
            })
        }
        None => None,
    };
    let scanned = Arc::new(nxb_extract::Counter::default());
    let req = nxb_extract::split::SplitRequest {
        versions: b
            .split_versions
            .iter()
            .map(|v| b.work.join("extract").join(v))
            .collect::<Vec<_>>(),
        game_tsv: b.profile.0.join("game.tsv"),
        outputs: split_outputs(b),
        threads: a.threads,
        nro_dir: inputs.nro_dir.clone(),
        oodle,
        progress: Some(scanned.clone()),
    };
    p.say("shader split (nxb-extract)");
    let (_, bad) = p
        .tracked(
            None,
            "B",
            || {
                (
                    scanned.done.load(Ordering::Relaxed),
                    scanned.total.load(Ordering::Relaxed),
                )
            },
            || nxb_extract::split::split(&req),
        )
        .map_err(|e| damaged(p, &e))?;
    for l in bad {
        p.say(&l);
    }
    if !inputs.glslc_observed.is_empty() {
        let map = std::fs::read_to_string(&req.outputs.map)
            .map_err(|e| format!("{}: {e}", req.outputs.map.display()))?;
        let said = crate::boot::captures::check_glslc_coverage(&inputs.glslc_observed, &map)
            .map_err(|e| crate::exit::fail(1, e))?;
        p.say(&said);
    }
    crate::provisional::take_tables(b)?;
    Ok(format!("{} versions", req.versions.len()))
}

/// The extract stage.
pub(crate) fn run(a: &Args, b: &Build, p: &Progress) -> Result<String, String> {
    for n in &b.src.notes {
        p.say(n);
    }
    if b.program != 0 {
        p.say(&format!(
            "program {} of the application: title ID {}",
            b.program, b.title
        ));
    }
    let (images, later) = match b.src.source {
        Source::Packages => packages(a, b, p)?,
        _ => (container(a, b, p)?, None),
    };
    let aoc = dlc(a, b, p)?;
    let note = crate::rewrite::ensure_with(&b.engine, &b.profile.0, &b.work, &b.package, Some(p))?;
    // the rewrite is skipped when this engine's rules are outside the port's contract: without rewritten modules
    // the app cannot be made, and saying so here is clearer than the app stage's missing file
    let patched = b.work.join(format!("patched_{}", b.package));
    let has_modules = std::fs::read_dir(&patched).is_ok_and(|d| {
        d.flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".patched.bin"))
    });
    if !has_modules {
        return Err(crate::exit::fail(
            4,
            format!(
                "unsupported: no rewritten modules in {} and the Rust rewrite does not apply to this engine (its patch files differ from the snapshot of the rules, or the game has files the port cannot collect)",
                patched.display()
            ),
        ));
    }
    if b.prov {
        crate::provisional::refine(b, p)?;
    }
    crate::boot::run::after_rewrite(a, b, p)?;
    if b.src.source == Source::Packages {
        // the fused split ran before the boots: what they made (or the decoder it lacked) needs a split of its own
        if let Some(why) = &later {
            p.say(&format!(
                "the split runs again with the game's own decoder: {why}"
            ));
        }
        let mut inputs = crate::boot::inputs::HostInputs::load(&b.work);
        if later.is_some() && inputs.oodle.is_none() && crate::boot::run::apple_silicon() {
            let c = crate::boot::Ctx::of(b, p)?;
            inputs.oodle = Some((c.host()?, c.patched()));
            inputs.save(&b.work)?;
        }
        if later.is_some() || inputs.split_again {
            split(a, b, p)?;
        } else if b.prov {
            crate::provisional::take_tables(b)?;
        }
    }
    let what = if b.src.source == Source::Packages {
        "images"
    } else {
        "versions"
    };
    Ok(format!(
        "{images} {what}{}{}",
        aoc.map(|n| format!(", {n}")).unwrap_or_default(),
        note.map(|n| format!(", {n}")).unwrap_or_default()
    ))
}
