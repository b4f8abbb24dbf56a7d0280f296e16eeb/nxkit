//! Opt-in Mac producer built on the same title-independent analysis used by other targets.
use super::{Say, analysis, auto_shaders, bundle, extract, io, link, stage};
use crate::Args;
use std::path::Path;

fn validate(a: &Args) -> Result<(), String> {
    if !matches!(a.own_front.as_str(), "" | "on") {
        return Err(
            "--auto-analyze requires --own-front on (no verdict-file or Apple compiler fallback)"
                .into(),
        );
    }
    if a.partial
        || a.pipeline_list.is_some()
        || a.manifest.is_some()
        || a.allow_damaged
        || a.gpu.as_deref().is_some_and(|s| s != "none")
    {
        return Err("--auto-analyze does not accept damaged data, shader subsets, pipeline lists or GPU archives".into());
    }
    Ok(())
}

fn put(run: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let rel = nxb_extract::safe_path::rel("run folder", name, false).map_err(|e| e.to_string())?;
    let path = run.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    std::fs::write(&path, bytes).map_err(|e| io(&path, e))
}

// symlink_metadata also sees dangling symlinks, which must never be followed for output.
fn vacant(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(path, e)),
        Ok(_) => Err(format!("{} exists: choose a new output", path.display())),
    }
}

fn publish(partial: &Path, out: &Path) -> Result<(), String> {
    // Reserve an empty destination atomically. A concurrent build cannot replace another app.
    std::fs::create_dir(out).map_err(|e| io(out, e))?;
    if let Err(e) = std::fs::rename(partial, out) {
        let _ = std::fs::remove_dir(out);
        return Err(io(out, e));
    }
    Ok(())
}

pub(super) fn run(a: &Args, p: &Say) -> Result<(), String> {
    validate(a).map_err(|e| crate::exit::fail(2, e))?;
    if a.own_front.is_empty() && std::env::var("NXB_OWN_FRONT").is_ok_and(|v| v == "0") {
        return Err(crate::exit::fail(
            2,
            "--auto-analyze requires the own shader front end. Use --own-front on to override NXB_OWN_FRONT=0",
        ));
    }
    if a.dry_run {
        p.say("auto-analysis: discover and extract packages or NUP, analyze modules/NPDM/NVN, rewrite without title patches, translate shaders, link and bundle for Mac. Runtime compatibility is not certified.");
        return Ok(());
    }
    if let Some(out) = &a.out {
        vacant(out)?;
    }
    p.plan(5);
    let game = stage(p, "extract", "validate application metadata", || {
        let game = extract::extract(a, p)?;
        let note = format!("{} [{:016X}] v{}", game.name, game.title_id, game.version);
        Ok((game, note))
    })?;
    let name = a.app_name.as_deref().unwrap_or(&game.name);
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| super::home(a).join(format!("{}.app", bundle::file_name(name))));
    // Never replace an app or its saves in the experimental generic path.
    vacant(&out)?;
    let mut partial = out.clone().into_os_string();
    partial.push(".partial");
    let partial = std::path::PathBuf::from(partial);
    vacant(&partial)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let host = bundle::host(std::env::var("NXB_NXKIT_HOST").ok().as_deref(), &exe)?;
    let id = a
        .bundle_id
        .clone()
        .unwrap_or_else(|| bundle::default_bundle_id(game.title_id));
    let analyzed = stage(
        p,
        "analysis",
        "derive and rewrite modules without executable title patches",
        || {
            let analyzed = analysis::analyze(&game)?;
            let note = format!(
                "{} modules, NPDM main thread, NVN name candidates",
                analyzed.modules.len()
            );
            Ok((analyzed, note))
        },
    )?;
    let shaders = stage(
        p,
        "shaders",
        "discover and translate shaders for macOS",
        || {
            let files = auto_shaders::discover(&game.modules, &game.files, false)?;
            let count = files
                .iter()
                .filter(|(n, _)| n.ends_with(".metallib"))
                .count();
            Ok((
                files,
                format!("{count} Metal libraries (zero does not prove no shaders are needed)"),
            ))
        },
    )?;
    // Claim the staging folder instead of reusing a folder or following a symlink.
    if let Some(parent) = partial.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    }
    std::fs::create_dir(&partial).map_err(|e| io(&partial, e))?;
    stage(p, "link", "make Mac app and sign guest dylib", || {
        let run = bundle::skeleton(
            &partial,
            &host,
            &bundle::info_plist(&bundle::file_name(name), &id, &game.version.to_string()),
        )?;
        put(&run, "game.json", analyzed.manifest(&game).as_bytes())?;
        put(&run, "main.npdm", &game.npdm)?;
        for dir in &game.directories {
            let rel =
                nxb_extract::safe_path::rel("run folder", dir, false).map_err(|e| e.to_string())?;
            std::fs::create_dir_all(run.join(&rel)).map_err(|e| io(&run.join(&rel), e))?;
        }
        for (name, bytes) in game.files.iter().chain(&shaders) {
            put(&run, name, bytes)?;
        }
        link::module_files(&run, &analyzed.modules, |_, _| {})?;
        link::write_dylib(&run, &analyzed.modules, |_, _| {})?;
        Ok(((), "Mac host and guest linked".into()))
    })?;
    stage(p, "bundle", "sign and publish new app", || {
        bundle::sign(&partial)?;
        publish(&partial, &out)?;
        Ok(((), out.display().to_string()))
    })?;
    p.say("Built without executable title patches. Launch and game compatibility are not verified by this build.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_paths_are_owned_and_stay_inside_the_run_folder() {
        let root = std::env::temp_dir().join(format!("nxb-generic-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        let out = root.join("Game.app");
        let partial = root.join("Game.app.partial");
        assert!(vacant(&out).is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("missing"), &out).unwrap();
            assert!(vacant(&out).is_err());
            std::fs::remove_file(&out).unwrap();
        }
        std::fs::create_dir(&partial).unwrap();
        put(&partial, "/nested/file", b"data").unwrap();
        assert_eq!(std::fs::read(partial.join("nested/file")).unwrap(), b"data");
        assert!(put(&partial, "../escape", b"data").is_err());
        std::fs::create_dir(&out).unwrap();
        assert!(publish(&partial, &out).is_err());
        assert!(partial.join("nested/file").is_file());
        std::fs::remove_dir(&out).unwrap();
        publish(&partial, &out).unwrap();
        assert!(out.join("nested/file").is_file());
        assert!(!partial.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_unsupported_input_modes() {
        let mut a = Args {
            nsp_dir: "game.nsp".into(),
            keys: Some("prod.keys".into()),
            ..Args::default()
        };
        assert!(validate(&a).is_ok());
        a.own_front = "off".into();
        assert!(validate(&a).is_err());
        a.own_front = "verdicts.json".into();
        assert!(validate(&a).is_err());
        a.own_front = "on".into();
        assert!(validate(&a).is_ok());
        a.pipeline_list = Some("pipelines.seen".into());
        assert!(validate(&a).is_err());
        a.pipeline_list = None;
        a.resource_patches.push(("x".into(), "y".into()));
        assert!(validate(&a).is_ok());
        a.resource_patches.clear();
        a.nsp_dir = "packages".into();
        assert!(validate(&a).is_ok());
    }
}
