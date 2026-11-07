//! The app stage of the Mac build: stage and sign the app (or update the previous one in place).

use std::path::{Path, PathBuf};

use crate::Args;
use crate::events::{Progress, Update};
use crate::graph::Build;

/// A pipeline archive folder with an archive in it (`pso_offline`: none when no base could be planned).
pub(crate) fn has_archive(d: &Path) -> bool {
    std::fs::read_dir(d).is_ok_and(|r| {
        r.flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".mtl4a"))
    })
}

/// The app stage: stage and sign (an in-place update of `previous`, the app as it was when the build started) with the
/// parts made ahead (`host::app::prepare`).
pub(crate) fn stage_app(
    a: &Args,
    b: &Build,
    p: &Progress,
    previous: Option<PathBuf>,
    parts: crate::host::app::AppParts,
    no_archive: bool,
    programs: &[nxb_pack::macapp::Program],
) -> Result<String, String> {
    let w = &b.work;
    let prof = &b.profile.1;
    let ex = w.join("extract").join(&b.package);
    let (companion, icns, lang) = (parts.companion, parts.icns, parts.lang);
    let nacp = std::fs::read(ex.join("control.nacp")).unwrap_or_default();
    let version = nxb_extract::version::display_version(&nacp);
    let name = a
        .app_name
        .clone()
        .or_else(|| nxb_extract::version::title_name(&nacp).ok())
        .or_else(|| prof.get("name").cloned())
        .unwrap_or_else(|| b.title.clone());
    let patched = w.join(format!("patched_{}", b.package));
    let fork = std::fs::read_to_string(b.engine.join("dm/fork_patches.txt")).unwrap_or_default();
    let tables = b.profile.0.clone();
    let pso_pairs = w.join("shader/pso_pairs.tsv");
    let inputs = nxb_pack::macapp::MacAppInputs {
        host: w.join("tools/dmhost"),
        companion: companion.clone(),
        patched: patched.clone(),
        romfs: ex.join("RomFS.img"),
        msl: crate::layer::msl_dir(b)?,
        msl_name: None,
        tables,
        guard_tables: crate::pipeline::guard_tables(a, b),
        font: w.join("font"),
        pso: (!b.gpus.is_empty() && !no_archive)
            .then(|| w.join("pso/mac"))
            .filter(|d| has_archive(d)),
        pso_pairs: pso_pairs.is_file().then_some(pso_pairs),
        html: Some(ex.join("html")),
        env_file: crate::options::env_file(a, w)?,
        settings: Some(parts.settings),
        icns,
        title_id: b.title.clone(),
        name,
        version: (!version.is_empty()).then_some(version),
        bundle_id: a.bundle_id.clone(),
        category: prof.get("app_category").cloned(),
        lang: Some(lang.clone()),
        marker: prof.get("romfs_marker").cloned(),
        fork_patches: fork
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(","),
        commit: None,
        aoc: b.work.join("aoc").is_dir().then(|| b.work.join("aoc")),
        programs: programs.to_vec(),
    };
    let plan = nxb_pack::macapp::plan(&inputs).map_err(|e| e.to_string())?;
    let done = match previous {
        Some(prev) => {
            // the companion app comes from the previous app: keep a copy, the update rewrites it in place
            let keep = w.join("tmp/nxnative");
            if companion.starts_with(&prev) {
                std::fs::create_dir_all(w.join("tmp")).map_err(|e| e.to_string())?;
                std::fs::copy(&companion, &keep).map_err(|e| e.to_string())?;
            }
            let mut plan = plan;
            for e in &mut plan.entries {
                if e.dest == "Contents/MacOS/nxnative" && companion.starts_with(&prev) {
                    e.content = nxb_pack::appstage::Content::File(keep.clone());
                }
            }
            nxb_pack::incremental::update(&plan, &prev, &b.out, a.threads)
                .map(|r| r.to_string())
                .map_err(|e| e.to_string())
        }
        None => {
            let staged = std::sync::Arc::new(nxb_pack::appstage::Staged::default());
            let o = nxb_pack::appstage::StageOptions {
                threads: a.threads,
                progress: Some(staged.clone()),
                ..Default::default()
            };
            p.set(Update {
                note: Some("staging and hashing, then signing"),
                ..Default::default()
            });
            p.tracked(
                None,
                "B",
                || {
                    (
                        staged.done.load(std::sync::atomic::Ordering::Relaxed),
                        staged.total.load(std::sync::atomic::Ordering::Relaxed),
                    )
                },
                || nxb_pack::appstage::stage_and_sign(&plan, &b.out, &o),
            )
            .map(|r| format!("{} files staged and signed", r.files))
                .map_err(|e| e.to_string())
        }
    }?;
    // `--mac-sign <identity>`: signed again with it (the staged app carries the ad-hoc signature)
    let done = if a.mac_sign == "-" {
        done
    } else {
        crate::options::sign_with_identity(&b.out, &a.mac_sign)?;
        format!("{done}; signed with {}", a.mac_sign)
    };
    // a partial library folder travels into the app as `msl/<b-V>/_b_partial.json`, the summary says so too
    if crate::layer::gfx(b)? != crate::layer::Gfx::Nvn {
        return Ok(done);
    }
    Ok(match nxb_sched::Partial::read(&b.shaders) {
        Some(st) => format!("{done}; {}", st.note()),
        None => done,
    })
}
