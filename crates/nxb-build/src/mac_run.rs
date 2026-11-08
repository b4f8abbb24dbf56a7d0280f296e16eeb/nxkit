//! The end of a Mac-only build (`--platform mac`, the development build): the command that runs the game on the Mac
//! (`mac_run` of the NSP script) and, for a multi-program application, the `programs.tsv` that `DM_PROGRAMS` names.
//! The command is printed and not started. The pipeline prebuild (`DM_PSO_PREBUILD=tool`), which starts the host, is
//! not run: the host builds the pipeline bases at its first start.

use std::path::{Path, PathBuf};

use crate::Args;
use crate::events::Progress;
use crate::graph::Build;
use crate::ios::shlex_quote;

/// What it takes to start the game of one program.
#[derive(Debug)]
pub(crate) struct Run {
    /// The environment of the command, in the order the script sets it.
    pub env: Vec<(String, String)>,
    /// The Mac host.
    pub host: PathBuf,
    /// The rewritten modules, the argument of the host.
    pub arg: PathBuf,
    /// The run folder, the working folder of the command.
    pub cwd: PathBuf,
}

fn set(env: &mut Vec<(String, String)>, k: &str, v: String) {
    match env.iter_mut().find(|(n, _)| n == k) {
        Some(slot) => slot.1 = v,
        None => env.push((k.to_string(), v)),
    }
}

fn text(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The folder the host reads its tables from, beside `DM_NVN_SHMAP`: the profile, or `<run>/tables` when the shader
/// step wrote `c1_full.tsv` or `cb_extent.tsv`. That folder holds the profile's tables with the step's in their place.
fn run_tables(a: &Args, b: &Build, cwd: &Path) -> Result<PathBuf, String> {
    let guard = crate::pipeline::guard_tables(a, b);
    let made = guard.as_deref().is_some_and(|g| {
        nxb_pack::macapp::GUARD_TABLES
            .iter()
            .any(|n| g.join(n).is_file())
    });
    if !made {
        return Ok(b.profile.0.clone());
    }
    let dir = cwd.join("tables");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for t in nxb_pack::macapp::table_files(&b.profile.0, guard.as_deref()) {
        let to = dir.join(t.file_name().unwrap_or_default());
        std::fs::copy(&t, &to).map_err(|e| format!("{}: {e}", t.display()))?;
    }
    Ok(dir)
}

/// The run of the build `b` with the host and the font folder of the application's first program: its folders
/// `<work>/run/{save,temp}` are made, `partial` (`--seen-shaders`) gives the pipeline bases an archive of their own.
pub(crate) fn run_of(
    a: &Args,
    b: &Build,
    host: &Path,
    font: &Path,
    partial: bool,
) -> Result<Run, String> {
    let work = std::fs::canonicalize(&b.work).map_err(|e| format!("{}: {e}", b.work.display()))?;
    let cwd = work.join("run");
    for d in ["save", "temp"] {
        std::fs::create_dir_all(cwd.join(d))
            .map_err(|e| format!("{}: {e}", cwd.join(d).display()))?;
    }
    let gfx = crate::layer::gfx(b)?.name();
    let ex = work.join("extract").join(&b.package);
    let romfs =
        crate::resource_patch::package_dir(a, b, &|m| eprintln!("{m}"), &ex)?.join("RomFS.img");
    let lib = crate::layer::msl_dir(b)?;
    let lib = lib
        .strip_prefix(&b.work)
        .map_or_else(|_| lib.clone(), |r| work.join(r));
    let msl = lib.parent().map_or_else(|| lib.clone(), Path::to_path_buf);
    let mut env: Vec<(String, String)> = Vec::new();
    set(&mut env, "DM_ROOT", text(&work));
    set(&mut env, "DM_ROMFS", text(&romfs));
    set(&mut env, "DM_SAVE", text(&cwd.join("save")));
    set(&mut env, "DM_TEMP", text(&cwd.join("temp")));
    set(&mut env, "DM_MSL_DIR", text(&msl));
    set(&mut env, "DM_FONT_DIR", text(font));
    if work.join("aoc").is_dir() {
        set(&mut env, "DM_AOC_DIR", text(&work.join("aoc")));
    }
    if gfx == "nvn" {
        let tables = run_tables(a, b, &cwd)?;
        set(
            &mut env,
            "DM_NVN_SHMAP",
            text(&tables.join("shader_map.tsv")),
        );
    }
    if partial {
        let archive = cwd.join("pso_archive_partial");
        // the keys may have grown: the prebuild walks the pairs again and skips what the archive holds
        let _ = std::fs::remove_file(archive.join("prebuilt.tsv"));
        set(&mut env, "DM_PSO_ARCHIVE", text(&archive));
    }
    let game = work.join("extract").join(&b.package);
    if crate::boot::jit::uses_jit(&game.join("ExeFS"))? {
        // what the game's nn::jit plugin makes in play goes into the iPhone's JIT image at the next build
        let play = game.join("jit_capture").join("play");
        std::fs::create_dir_all(&play).map_err(|e| format!("{}: {e}", play.display()))?;
        set(&mut env, "DM_JIT_CAPTURE", text(&play));
    }
    if b.program != 0 && b.profile.1.contains_key("program_of") {
        set(&mut env, "DM_PROGRAM", b.program.to_string());
    }
    let (_, lang) = crate::host::app::language(a, &work, &b.package)?;
    set(&mut env, "DM_LANG", lang);
    set(&mut env, "DM_WINDOW", "1".into());
    set(&mut env, "DM_AUDIO", "1".into());
    let nxkit = crate::nxkit::run_env(
        &b.title,
        b.profile.1.get("nxkit_services").map(String::as_str),
    )?;
    for (k, v) in nxkit {
        set(&mut env, k, v);
    }
    if let Some(f) = crate::options::env_file(a, &work)? {
        for (k, v) in crate::options::env_pairs(&f)? {
            set(&mut env, &k, v);
        }
    }
    let pairs = work.join("shader/pso_pairs.tsv");
    if gfx == "nvn"
        && b.profile.0.join("shader_pairs.tsv").is_file()
        && !b.profile.0.join("pso_pairs.tsv").is_file()
        && pairs.is_file()
    {
        set(&mut env, "DM_PSO_PAIRS", text(&pairs));
    }
    Ok(Run {
        env,
        host: host.to_path_buf(),
        arg: work.join(format!("patched_{}", b.package)),
        cwd,
    })
}

/// The shell command of a run: `cd <run> && K=V... <host> <patched>`, with the host's output kept in `host.log` for a
/// partial library.
pub(crate) fn command(r: &Run, partial: bool) -> String {
    let env: Vec<String> = r
        .env
        .iter()
        .map(|(k, v)| format!("{k}={}", shlex_quote(v)))
        .collect();
    format!(
        "cd {} && {} {} {}{}",
        shlex_quote(&text(&r.cwd)),
        env.join(" "),
        shlex_quote(&text(&r.host)),
        shlex_quote(&text(&r.arg)),
        if partial {
            format!(
                " 2>&1 | tee {}",
                shlex_quote(&text(&r.cwd.join("host.log")))
            )
        } else {
            String::new()
        }
    )
}

/// The text of `programs.tsv`: the programs in the order of their indexes, each with the host, the rewritten
/// modules, the run folder and the environment (without the window and sound switches of the run command). Every
/// program shares the save of the first, as on Horizon.
pub(crate) fn programs_tsv(runs: &[(u64, &Run)]) -> String {
    let mut rows = vec!["# The programs of this multi-program application on the Mac (build_ipa_from_nsp.py): index, the host (exe), rewritten modules (arg), run folder (cwd) and environment; DM_PROGRAMS names this file (hle_oe.c)".to_string()];
    let save = runs
        .first()
        .and_then(|(_, r)| r.env.iter().find(|(k, _)| k == "DM_SAVE"))
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    for (n, r) in runs {
        let mut cols = vec![
            n.to_string(),
            format!("exe={}", r.host.display()),
            format!("arg={}", r.arg.display()),
            format!("cwd={}", r.cwd.display()),
        ];
        for (k, v) in &r.env {
            match k.as_str() {
                "DM_WINDOW" | "DM_AUDIO" => {}
                "DM_SAVE" => cols.push(format!("{k}={save}")),
                _ => cols.push(format!("{k}={v}")),
            }
        }
        rows.push(cols.join("\t"));
    }
    format!("{}\n", rows.join("\n"))
}

/// The last step of a Mac-only build: the run command of the game, with `DM_PROGRAMS` when the application has more
/// programs (they are built as sub-builds, [`crate::programs`]).
pub(crate) fn finish(a: &Args, b: &Build, p: &std::sync::Arc<Progress>) -> Result<(), String> {
    let work = std::fs::canonicalize(&b.work).map_err(|e| format!("{}: {e}", b.work.display()))?;
    let host = work.join("tools/dmhost");
    let font = work.join("font");
    let partial = a.seen_shaders;
    let first = run_of(a, b, &host, &font, partial)?;
    let mut cmd = command(&first, partial);
    if b.program == 0 {
        let others = crate::programs::mac_runs(a, b, &host, &font, p)?;
        if !others.is_empty() {
            let mut runs: Vec<(u64, &Run)> = vec![(0, &first)];
            runs.extend(others.iter().map(|(n, r)| (*n, r)));
            let tsv = work.join("programs.tsv");
            std::fs::write(&tsv, programs_tsv(&runs))
                .map_err(|e| format!("{}: {e}", tsv.display()))?;
            cmd = cmd.replacen(
                " && ",
                &format!(" && DM_PROGRAMS={} ", shlex_quote(&text(&tsv))),
                1,
            );
        }
    }
    if b.profile.0.join("shader_pairs.tsv").is_file() {
        p.say("the pipeline prebuild is not run here (it starts the host): the game builds its pipeline bases at its first start");
    }
    p.say(&format!(
        "done: the game runs on the Mac with this command (its save in {}, kept from run to run. DM_WINDOW=1 opens a window, DM_AUDIO=1 plays sound):",
        work.join("run/save").display()
    ));
    p.say(&cmd);
    if partial {
        p.say(&format!(
            "a partial shader library: where a shader is missing, the log says so in red and the draw goes without it. Run this again with --seen-shaders to add what {} names, or without it for the whole map",
            work.join("run/host.log").display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(save: &str, program: Option<&str>) -> Run {
        let mut env = vec![
            ("DM_ROOT".to_string(), "/w".to_string()),
            ("DM_SAVE".to_string(), save.to_string()),
        ];
        if let Some(p) = program {
            env.push(("DM_PROGRAM".into(), p.into()));
        }
        env.push(("DM_WINDOW".into(), "1".into()));
        env.push(("DM_AUDIO".into(), "1".into()));
        Run {
            env,
            host: "/w/tools/dmhost".into(),
            arg: "/w/patched_v0".into(),
            cwd: "/w/run".into(),
        }
    }

    #[test]
    fn the_command_quotes_as_the_script_does() {
        let mut r = run("/w/run/save", None);
        r.env.push(("DM_X".into(), "a b'c".into()));
        assert_eq!(
            command(&r, false),
            "cd /w/run && DM_ROOT=/w DM_SAVE=/w/run/save DM_WINDOW=1 DM_AUDIO=1 DM_X='a b'\"'\"'c' /w/tools/dmhost /w/patched_v0"
        );
        assert!(command(&r, true).ends_with(" 2>&1 | tee /w/run/host.log"));
    }

    #[test]
    fn the_programs_file_shares_the_first_save() {
        let (first, second) = (run("/w/run/save", None), run("/w1/run/save", Some("1")));
        let tsv = programs_tsv(&[(0, &first), (1, &second)]);
        let lines: Vec<&str> = tsv.lines().collect();
        assert!(lines[0].starts_with("# The programs of this multi-program application"));
        assert_eq!(
            lines[1],
            "0\texe=/w/tools/dmhost\targ=/w/patched_v0\tcwd=/w/run\tDM_ROOT=/w\tDM_SAVE=/w/run/save"
        );
        assert_eq!(
            lines[2],
            "1\texe=/w/tools/dmhost\targ=/w/patched_v0\tcwd=/w/run\tDM_ROOT=/w\tDM_SAVE=/w/run/save\tDM_PROGRAM=1"
        );
    }

    #[test]
    fn a_run_sets_the_environment_in_the_scripts_order() {
        let d = std::env::temp_dir().join(format!("nxb-macrun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (work, prof) = (d.join("work"), d.join("prof"));
        std::fs::create_dir_all(work.join("aoc")).unwrap();
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("shader_pairs.tsv"), "").unwrap();
        std::fs::create_dir_all(work.join("shader")).unwrap();
        std::fs::write(work.join("shader/pso_pairs.tsv"), "").unwrap();
        let exefs = work.join("extract/v0/ExeFS");
        std::fs::create_dir_all(&exefs).unwrap();
        let jit =
            "_ZN2nn3jit20CreateJitEnvironmentEPNS0_14JitEnvironmentERKNS0_16JitConfigurationE";
        std::fs::write(exefs.join("main"), crate::boot::jit::test_main(jit, false)).unwrap();
        let mut b = Build::for_test(work.clone(), prof.clone());
        b.package = "v0".into();
        b.program = 2;
        b.shaders = work.join("shader/cache/b-1");
        b.profile
            .1
            .insert("program_of".into(), "0400000000000000".into());
        let r = run_of(
            &Args::default(),
            &b,
            Path::new("/h/dmhost"),
            Path::new("/f"),
            true,
        )
        .unwrap();
        let keys: Vec<&str> = r.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "DM_ROOT",
                "DM_ROMFS",
                "DM_SAVE",
                "DM_TEMP",
                "DM_MSL_DIR",
                "DM_FONT_DIR",
                "DM_AOC_DIR",
                "DM_NVN_SHMAP",
                "DM_PSO_ARCHIVE",
                "DM_JIT_CAPTURE",
                "DM_PROGRAM",
                "DM_LANG",
                "DM_WINDOW",
                "DM_AUDIO",
                "DM_PSO_PAIRS"
            ]
        );
        let get = |k: &str| r.env.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(get("DM_PROGRAM"), "2");
        assert!(get("DM_MSL_DIR").ends_with("work/shader/cache"));
        assert!(get("DM_PSO_ARCHIVE").ends_with("work/run/pso_archive_partial"));
        assert!(work.join("run/save").is_dir() && work.join("run/temp").is_dir());
        assert!(get("DM_JIT_CAPTURE").ends_with("work/extract/v0/jit_capture/play"));
        assert!(work.join("extract/v0/jit_capture/play").is_dir());
        assert_eq!(get("DM_NVN_SHMAP"), text(&prof.join("shader_map.tsv")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_run_reads_the_guard_tables_of_its_shader_step() {
        let d = std::env::temp_dir().join(format!("nxb-macrun-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (work, prof) = (d.join("work"), d.join("prof"));
        std::fs::create_dir_all(work.join("shader/work/tables")).unwrap();
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("shader_map.tsv"), "map").unwrap();
        std::fs::write(prof.join("c1_full.tsv"), "old").unwrap();
        std::fs::write(prof.join("cb_extent.tsv"), "old").unwrap();
        std::fs::write(work.join("shader/work/tables/c1_full.tsv"), "new").unwrap();
        let mut b = Build::for_test(work.clone(), prof.clone());
        b.shaders = work.join("shader/cache/b-1");
        let r = run_of(
            &Args::default(),
            &b,
            Path::new("/h/dmhost"),
            Path::new("/f"),
            false,
        )
        .unwrap();
        let map = r.env.iter().find(|(n, _)| n == "DM_NVN_SHMAP").unwrap();
        let dir = PathBuf::from(&map.1).parent().unwrap().to_path_buf();
        assert!(dir.ends_with("run/tables"));
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).unwrap();
        assert_eq!(
            [
                read("shader_map.tsv"),
                read("c1_full.tsv"),
                read("cb_extent.tsv")
            ],
            ["map", "new", "old"]
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}
