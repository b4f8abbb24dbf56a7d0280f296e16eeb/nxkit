//! The extract stage for the inputs of [`crate::inputs`]: packages (extract and shader split fused in one pass), a NUP
//! container (RomFS as a folder, the split in the same pass), and the DLC images (`<work>/aoc/<index>.img`,
//! index = DLC title ID - base title ID - 0x1000, the host mounts them by index).

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
            p.say(&format!("nxb: error: hash check failed: {f}"));
        }
        p.say(
            "nxb: error: the package is damaged (--allow-damaged writes the files as read and goes on)",
        );
    }
    crate::exit::fail(e.exit_code(), e.to_string())
}

/// One stat line per DLC package: what a later build compares to see that the DLC images are current.
fn dlc_key(b: &Build) -> String {
    let mut rows: Vec<String> = b
        .src
        .dlc
        .iter()
        .map(|(t, v, path)| {
            let m = std::fs::metadata(path);
            format!(
                "{t:016x} {v} {} {} {}",
                path.display(),
                m.as_ref().map_or(0, |m| m.len()),
                m.as_ref()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_nanos() as i64)
            )
        })
        .collect();
    rows.sort();
    format!("{}\n", rows.join("\n"))
}

/// DLC packages into `<work>/aoc/<index>.img`. Reused while the packages are as at the last extraction.
pub(crate) fn dlc(a: &Args, b: &Build, p: &Progress) -> Result<Option<String>, String> {
    let dlc: Vec<_> = b
        .src
        .dlc
        .iter()
        .filter(|(_, _, path)| !nxb_extract::nup::is_nup(path))
        .collect();
    if dlc.is_empty() {
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
        && dlc
            .iter()
            .all(|d| dst.join(format!("{}.img", idx(d.0))).is_file())
    {
        return Ok(Some(format!("{} DLC reused", dlc.len())));
    }
    let _ = std::fs::remove_file(&kf);
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let n = dlc.len() as u64;
    for (i, (tid, _, pkg)) in dlc.into_iter().enumerate() {
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
        let id = if a.auto_analyze || b.src.source == Source::Nup {
            Some(*tid)
        } else {
            b.src.app_flag.map(|_| *tid)
        };
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
        program: None,
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
