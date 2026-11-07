//! `nxkit-host <run folder> [--fullscreen]` runs a game. `nxkit-host probe <run folder>` prints the R18 measurements.

#[cfg(target_os = "macos")]
fn main() {
    use std::path::PathBuf;

    use nxkit_host::run::Options;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: nxkit-host [--fullscreen] <run folder>\n       nxkit-host probe <run folder>";
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
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
