//! The nxkit runtime in a Mac run (`--platform mac`). The host loads `libnxkit_rt.dylib` at its first lookup with a
//! service in its mask (`dm/host/hle.c` of the engine). The host build holds neither the library nor the mask of the
//! module, so the run command carries both: `DM_NXKIT_SERVICES` and `DM_NXKIT_LIB`. A changed library or module needs a
//! new run command and no new stage.

use std::path::{Path, PathBuf};

const LIB: &str = "libnxkit_rt.dylib";

/// The services nxkit runs for a title, by the module that the catalog holds for it (none for a title without one).
fn module_services(title: &str) -> Result<Vec<&'static str>, String> {
    // a title that is not 16 hex digits has no module
    let Ok(id) = u64::from_str_radix(title, 16) else {
        return Ok(Vec::new());
    };
    let catalog = nxtitles::catalog()?;
    let facts = nxtitle::TitleFacts::new(nxtitle::TitleId(id));
    Ok(catalog
        .resolve(&facts)
        .nxkit_services()
        .into_iter()
        .map(nxtitle::Service::id)
        .collect())
}

/// The library: `NXB_NXKIT_RT` or the file beside `exe`. `cargo build -p nxkit-rt` puts it next to `nxb` in
/// `target/<profile>/`.
fn library(env: Option<&str>, exe: &Path) -> Result<PathBuf, String> {
    let path = match env.filter(|e| !e.is_empty()) {
        Some(e) => PathBuf::from(e),
        None => exe.parent().unwrap_or(Path::new(".")).join(LIB),
    };
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "{}: the title runs services on nxkit and the library is missing (build it with `cargo build -p nxkit-rt`, or set NXB_NXKIT_RT)",
        path.display()
    ))
}

/// The services of the run: those of the profile's `nxkit_services` and those of the module, each once, in the order of
/// `nxtitle::Service::ALL`.
fn merged(profile: Option<&str>, module: &[&str]) -> Vec<&'static str> {
    let named: Vec<&str> = profile
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .collect();
    nxtitle::Service::ALL
        .into_iter()
        .map(nxtitle::Service::id)
        .filter(|id| named.contains(id) || module.contains(id))
        .collect()
}

/// The environment of the run command for a service list: nothing for none.
fn env_of(
    services: &[&str],
    lib_env: Option<&str>,
    exe: &Path,
) -> Result<Vec<(&'static str, String)>, String> {
    if services.is_empty() {
        return Ok(Vec::new());
    }
    let lib = library(lib_env, exe)?;
    Ok(vec![
        ("DM_NXKIT_SERVICES", services.join(",")),
        ("DM_NXKIT_LIB", lib.to_string_lossy().into_owned()),
    ])
}

/// What a run of `title` adds to the host's environment. `profile` is the `nxkit_services` key of its `game.tsv`.
pub(crate) fn run_env(
    title: &str,
    profile: Option<&str>,
) -> Result<Vec<(&'static str, String)>, String> {
    let module = module_services(title)?;
    let services = merged(profile, &module);
    let exe = std::env::current_exe().map_err(|e| format!("current exe: {e}"))?;
    env_of(
        &services,
        std::env::var("NXB_NXKIT_RT").ok().as_deref(),
        &exe,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn services_of_both_sources_come_once_in_service_order() {
        assert_eq!(merged(None, &[]), Vec::<&str>::new());
        assert_eq!(
            merged(Some("gpu, fs"), &["offline", "fs"]),
            ["fs", "offline", "gpu"]
        );
    }

    #[test]
    fn a_title_without_a_service_needs_no_library() {
        // not even a library that does not exist
        assert!(
            env_of(&[], None, Path::new("/nope/nxb"))
                .unwrap()
                .is_empty()
        );
        // BotW runs the offline service on nxkit
        assert_eq!(module_services("01007EF00011E000").unwrap(), ["offline"]);
        assert!(run_env("0100000000000000", None).unwrap().is_empty());
    }

    #[test]
    fn the_library_is_found_beside_the_executable_or_by_the_variable() {
        let d = std::env::temp_dir().join(format!("nxb-nxkit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let exe = d.join("nxb");
        let err = library(None, &exe).unwrap_err();
        assert!(err.contains("cargo build -p nxkit-rt"), "{err}");
        std::fs::write(d.join(LIB), "").unwrap();
        assert_eq!(library(None, &exe).unwrap(), d.join(LIB));
        std::fs::write(d.join("other.dylib"), "").unwrap();
        let other = d.join("other.dylib");
        assert_eq!(library(other.to_str(), &exe).unwrap(), other);
        assert!(library(Some("/nope.dylib"), &exe).is_err());
        let env = env_of(&["fs", "offline"], None, &exe).unwrap();
        assert_eq!(env[0], ("DM_NXKIT_SERVICES", "fs,offline".to_string()));
        assert_eq!(env[1], ("DM_NXKIT_LIB", d.join(LIB).display().to_string()));
        let _ = std::fs::remove_dir_all(&d);
    }
}
