//! The command line of the `nxb` binary (layer L3).
//!
//! `nxb` is the one binary: the build of an nxkit app and the stages that work without more than their arguments, as
//! subcommands. There are no other program names and no dispatch by `argv[0]`. Each subcommand calls a library main:
//!
//! | `nxb` command | calls |
//! |---|---|
//! | `build` | `nxb_build::main` |
//! | `extract` | `nxb_extract::cli::main` |
//! | `split` | `nxb_extract::split_cli::main` |
//! | `shaders guards\|own-verdicts\|build` | this crate (the shader-step command line) |
//! | `sign` | `nxb_pack::cli::sign::main` |
//! | `pso` | `nxb_archive::cli::pso::main` |
//! | `archive` | `nxb_archive::cli::archive::main` |
//! | `airc` | `airc::main` (the own front end compiler) |
//! | `cache`, `prune` | `cache` (store export, import, prune, list) |
//! | `regressions` | `regressions` (`shader_regressions.py`) |
//! | `self-check` | `selfcheck` (external tools and versions) |
//!
//! `nxb help` lists the commands by group and `nxb <command> --help` prints the usage. An unknown command exits with 2.

use std::io::Write;

mod build;
mod cache;
pub mod help;
mod regressions;
mod selfcheck;

/// Which binary is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Binary {
    /// `nxb`
    Nxb,
    /// `nxb shaders`
    Shaderbuild,
}

impl Binary {
    /// The program name.
    pub fn name(self) -> &'static str {
        match self {
            Binary::Nxb => "nxb",
            Binary::Shaderbuild => "nxb shaders",
        }
    }

    /// The commands, in help order.
    pub fn commands(self) -> Vec<&'static str> {
        match self {
            Binary::Nxb => help::names(help::NXB),
            Binary::Shaderbuild => help::SHADERBUILD.iter().map(|(n, _)| *n).collect(),
        }
    }
}

/// The usage text of a command, if the binary has it.
fn usage(bin: Binary, cmd: &str) -> Option<&'static str> {
    match bin {
        Binary::Nxb => help::usage_of(cmd),
        Binary::Shaderbuild => help::shaderbuild_usage(cmd),
    }
}

/// `nxb shaders <sub> ...`: the shader-step command line.
fn shaders(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    match args.first().map(String::as_str) {
        None | Some("--help" | "-h" | "help") => {
            let u = help::usage_of("shaders").unwrap_or_default();
            writeln!(out, "{u}").map_or(1, |_| 0)
        }
        Some(_) => run(Binary::Shaderbuild, args, out, err),
    }
}

/// Run a binary with its arguments (without the program name) and return the exit code.
pub fn run(bin: Binary, args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    // `<command> --help`: the usage, exit 0 (no stage takes these as arguments)
    if args.len() > 1
        && matches!(args[1].as_str(), "--help" | "-h")
        && !(bin == Binary::Nxb && cmd == "shaders")
        && let Some(u) = usage(bin, cmd)
    {
        return writeln!(out, "{u}").map_or(1, |_| 0);
    }
    let rest = &args.get(1..).unwrap_or_default();
    let res = match cmd {
        "version" | "--version" | "-V" => {
            writeln!(out, "{} {}", bin.name(), env!("CARGO_PKG_VERSION")).map(|_| 0)
        }
        "help" | "--help" | "-h" => match (bin, rest.first()) {
            (Binary::Nxb, None) => help::write_nxb_help(out).map(|_| 0),
            (_, Some(c)) if usage(bin, c).is_some() => {
                writeln!(out, "{}", usage(bin, c).unwrap_or_default()).map(|_| 0)
            }
            _ => {
                let list = bin.commands().join(", ");
                writeln!(
                    out,
                    "usage: {} <command> [args]\ncommands: {list}",
                    bin.name()
                )
                .map(|_| 0)
            }
        },
        // commands of both binaries
        "prune" => {
            return cache::main(
                &[vec!["prune".to_string()], rest.to_vec()].concat(),
                out,
                err,
            );
        }
        "regressions" => return regressions::main(rest, out, err),
        "self-check" => return selfcheck::main(rest, out, err),
        "guards" if bin == Binary::Shaderbuild => return guards(rest, out, err),
        "own-verdicts" if bin == Binary::Shaderbuild => return own_verdicts(rest, out, err),
        "build" if bin == Binary::Shaderbuild => return build::main(rest, out, err),
        // the whole build (ADR W19: nxb-build is the orchestrator crate)
        "build" if bin == Binary::Nxb => return nxb_build::main(rest),
        // the shader step under its own name
        "shaders" if bin == Binary::Nxb => return shaders(rest, out, err),
        // a title's shader and pipeline cache as a store export
        "cache" if bin == Binary::Nxb => return cache::main(rest, out, err),
        // the extract stage: NSP and NUP packages (ADR W19)
        "extract" if bin == Binary::Nxb => return nxb_extract::cli::main(rest),
        "split" if bin == Binary::Nxb => return nxb_extract::split_cli::main(rest),
        // pack and sign stages
        "sign" if bin == Binary::Nxb => return nxb_pack::cli::sign::main(rest),
        // pipeline archive stages
        "pso" if bin == Binary::Nxb => return nxb_archive::cli::pso::main(rest),
        "archive" if bin == Binary::Nxb => return nxb_archive::cli::archive::main(rest),
        // the own front end compiler
        "airc" if bin == Binary::Nxb => return airc_main(rest),
        c => writeln!(err, "{}: unknown command {c:?} (try help)", bin.name()).map(|_| 2),
    };
    res.unwrap_or(1)
}

/// `nxb airc ...`: the front end compiler (`airc::main`), exit code 3 (unsupported) when built without the front end.
#[cfg(feature = "air-front")]
fn airc_main(args: &[String]) -> i32 {
    airc::main(args)
}

/// Without the `air-front` feature there is no front end in the binary.
#[cfg(not(feature = "air-front"))]
fn airc_main(_args: &[String]) -> i32 {
    eprintln!("nxb airc: this build has no own front end (feature air-front)");
    3
}

/// `shaderbuild own-verdicts <validation.jsonl> [data-dir]`: write the shipped verdict file of this front end and this
/// Mac toolchain (own-identical shaders only).
fn own_verdicts(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some(jsonl) = args.first() else {
        let _ = writeln!(
            err,
            "usage: nxb shaders own-verdicts <validation.jsonl> [data-dir]"
        );
        return 2;
    };
    let data = args.get(1).map_or_else(
        || nxb_stages::own_front::default_config().verdicts,
        std::path::PathBuf::from,
    );
    let tmp = std::env::temp_dir().join(format!("nxb-own-verdicts-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let r = nxb_stages::own_front::mac_compiler_id(&tmp)
        .and_then(|cid| nxb_stages::own_front::ship(std::path::Path::new(jsonl), &data, &cid));
    let _ = std::fs::remove_dir_all(&tmp);
    match r {
        Ok((f, n)) => writeln!(out, "{n} own-identical shaders: {}", f.display()).map_or(1, |_| 0),
        Err(e) => {
            let _ = writeln!(err, "[RED] {e}");
            1
        }
    }
}

/// `shaderbuild guards <b-dir> <out-dir> [jobs]`: write `c1_full.tsv` and `cb_extent.tsv`.
fn guards(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let (Some(b), Some(o)) = (args.first(), args.get(1)) else {
        let _ = writeln!(err, "usage: nxb shaders guards <b-dir> <out-dir> [jobs]");
        return 2;
    };
    let jobs = args.get(2).and_then(|j| j.parse().ok()).unwrap_or(6);
    match nxb_stages::guard_tables::write(std::path::Path::new(b), std::path::Path::new(o), jobs) {
        Ok((c1, cb)) => {
            let _ = writeln!(
                out,
                "not proven bounded: {c1} shaders; uniform extent rows: {cb}"
            );
            0
        }
        Err(e) => {
            let _ = writeln!(err, "[RED] guards: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(bin: Binary, args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let (mut o, mut e) = (Vec::new(), Vec::new());
        let code = run(bin, &args, &mut o, &mut e);
        (
            code,
            String::from_utf8_lossy(&o).into(),
            String::from_utf8_lossy(&e).into(),
        )
    }

    #[test]
    fn version_and_help() {
        let (c, o, _) = call(Binary::Shaderbuild, &["version"]);
        assert_eq!(c, 0);
        assert!(o.starts_with("nxb shaders "));
        let (c, o, _) = call(Binary::Nxb, &[]);
        assert_eq!(c, 0);
        assert!(o.contains("extract"));
    }

    #[test]
    fn unknown_help_and_usage() {
        assert_eq!(call(Binary::Nxb, &["frobnicate"]).0, 2);
        assert_eq!(call(Binary::Shaderbuild, &["parity"]).0, 2);
        let (c, o, _) = call(Binary::Nxb, &["help"]);
        assert_eq!(c, 0);
        for cmd in Binary::Nxb.commands() {
            assert!(o.contains(cmd), "help lacks {cmd}");
            let (c, o, _) = call(Binary::Nxb, &[cmd, "--help"]);
            assert_eq!(c, 0, "{cmd} --help");
            assert!(o.starts_with("usage: nxb"), "{cmd}: {o}");
        }
        for sub in ["guards", "own-verdicts", "build"] {
            let (c, o, _) = call(Binary::Nxb, &["shaders", sub]);
            assert!(c == 2 && o.is_empty() || c == 1, "{sub}");
            let (c, o, _) = call(Binary::Shaderbuild, &[sub, "--help"]);
            assert!(c == 0 && o.contains(sub));
        }
    }

    #[test]
    fn shaders_is_shaderbuild() {
        for args in [
            vec!["guards"],
            vec!["own-verdicts"],
            vec!["prune"],
            vec!["regressions", "/nonexistent-dir"],
            vec!["frobnicate"],
        ] {
            let sb = call(Binary::Shaderbuild, &args);
            let mut a = vec!["shaders"];
            a.extend(&args);
            assert_eq!(sb, call(Binary::Nxb, &a), "{args:?}");
        }
    }
}
