//! `nxkit-host <run folder> [--fullscreen]` runs a game. `nxkit-host probe <run folder>` prints the R18 measurements.
//! With no arguments, the host uses the run folder in its application bundle.

#[cfg(any(target_os = "macos", test))]
fn bundled_run_folder(executable: &std::path::Path) -> Result<std::path::PathBuf, String> {
    use std::ffi::OsStr;

    let macos = executable.parent();
    let contents = macos.and_then(std::path::Path::parent);
    let app = contents.and_then(std::path::Path::parent);
    if !executable.is_absolute()
        || executable.file_name() != Some(OsStr::new("nxkit-host"))
        || macos.and_then(std::path::Path::file_name) != Some(OsStr::new("MacOS"))
        || contents.and_then(std::path::Path::file_name) != Some(OsStr::new("Contents"))
        || app.and_then(std::path::Path::extension) != Some(OsStr::new("app"))
    {
        return Err(format!(
            "{} is not in a generated application bundle. Supply a run folder explicitly.",
            executable.display()
        ));
    }
    let Some(contents) = contents else {
        return Err("The application bundle has no Contents folder.".into());
    };
    let root = contents.join("Resources/nxkit");
    if !root.join("game.json").is_file() {
        return Err(format!(
            "The application bundle has no run manifest at {}.",
            root.join("game.json").display()
        ));
    }
    Ok(root)
}

/// The allocator with the spy of `NXKIT_MEM_ALLOC` (see `docs/diagnostics.md`).
#[cfg(target_os = "macos")]
#[global_allocator]
static ALLOC: nxkit_host::sys::allocspy::Spy = nxkit_host::sys::allocspy::Spy;

#[cfg(target_os = "macos")]
fn main() {
    use std::path::PathBuf;

    use nxkit_host::run::Options;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage =
        "usage: nxkit-host [--fullscreen] <run folder>\n       nxkit-host probe <run folder>";
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [] => {
            let root = std::env::current_exe()
                .map_err(|e| format!("Cannot locate nxkit-host: {e}"))
                .and_then(|executable| bundled_run_folder(&executable));
            let root = match root {
                Ok(root) => root,
                Err(e) => {
                    eprintln!("nxkit-host: {e}\n{usage}");
                    std::process::exit(64);
                }
            };
            nxkit_host::run::run(&Options {
                root,
                fullscreen: false,
            })
        }
        ["probe", root] => nxkit_host::probe::run(&PathBuf::from(*root)),
        ["--fullscreen", root] | [root, "--fullscreen"] => nxkit_host::run::run(&Options {
            root: PathBuf::from(*root),
            fullscreen: true,
        }),
        [root] if *root != "probe" && !root.starts_with('-') => nxkit_host::run::run(&Options {
            root: PathBuf::from(*root),
            fullscreen: false,
        }),
        _ => {
            eprintln!("{usage}");
            std::process::exit(64);
        }
    };
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => nxkit_host::stop::fatal(&e.to_string()),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("nxkit-host runs on macOS only");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::bundled_run_folder;
    use std::path::Path;

    #[test]
    fn rejects_paths_outside_generated_bundles() {
        for path in [
            "/usr/local/bin/nxkit-host",
            "/tmp/Game/Contents/MacOS/nxkit-host",
            "/tmp/Game.app/Other/MacOS/nxkit-host",
            "/tmp/Game.app/Contents/bin/nxkit-host",
            "/tmp/Game.app/Contents/MacOS/other",
            "Game.app/Contents/MacOS/nxkit-host",
        ] {
            assert!(
                bundled_run_folder(Path::new(path))
                    .unwrap_err()
                    .contains("Supply a run folder explicitly")
            );
        }
    }

    #[test]
    fn resolves_bundle_resources_and_requires_a_manifest() {
        let temp = std::env::temp_dir().join(format!("nxkit-host-bundle-{}", std::process::id()));
        let app = temp.join("Game With Spaces.app");
        let executable = app.join("Contents/MacOS/nxkit-host");
        let root = app.join("Contents/Resources/nxkit");
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            bundled_run_folder(&executable)
                .unwrap_err()
                .contains("no run manifest")
        );
        std::fs::write(root.join("game.json"), "{}").unwrap();
        assert_eq!(bundled_run_folder(&executable).unwrap(), root);
        std::fs::remove_dir_all(temp).unwrap();
    }
}
