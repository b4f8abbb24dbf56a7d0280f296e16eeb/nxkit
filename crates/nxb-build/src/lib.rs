//! `nxb build`: the NSP build script (`scripts/build_ipa_from_nsp.py`) as one stage graph. Same command line (the
//! options it does not implement yet are refused by name, exit code 1 with a `[RED]` line), same console events
//! (`--progress json`: the front ends' NDJSON, [`events`]), same exit codes (0 done, 1 error, 2 bad arguments, 130
//! interrupted).
//!
//! The graph (`--platform mac-app`): extract + split (fused, one pass for all versions) -> shaders -> pipeline archive
//! (when `--gpu` names an architecture.`nxb-archive`, not run on a full game from here yet) -> app (stage + sign, an in-place update of the previous app).
//! Stages overlap where the graph allows ([`order`]): the Mac host from the start, the app's companion app, icon and
//! settings beside the shader step, the first archive's batches as their libraries appear (widening to every core when
//! the shader step ends), the console events stay those of a run one after another. Not overlapped yet: the shader
//! step with the extract (it reads the split's packages when they are all written) and the staging of the app's
//! non-shader files with the shader step. `--dry-run` prints the graph and the keys.
//!
//! `--platform mac` has the same nodes without the app and ends with the command that runs the game (`mac_run`).
//! `--program N` builds one program of a multi-program application as a game of its own. For `mac-app` and `mac`, the
//! profile's other programs are built that way first (`programs`).
//!
//! The rewrite (`patch_nso.py`) is made by [`rewrite`] when the work folder has no `patched_<package>`, with the engine's own
//! files (a case outside the port's contract stops the build at the rewrite with the message of `nxb rewrite`, which exits with code 4: there is no fallback to a later stage without the patched modules). Not ported yet,
//! taken from the work folder (the Python build's outputs) and named in the plan: the survey and profile steps. The
//! shader step is an existing library folder (`--shaders`, or the work folder's `shader/cache/b-<chain>`), running the
//! lead's `shaderbuild build` as a process from here is not wired yet.

mod app_stage;
pub mod archive_layout;
mod args;
mod boot;
pub mod events;
mod extract_stage;
pub mod graph;
pub mod host;
pub mod inputs;
pub mod ios;
pub mod ios_cli;
mod iosbuild;
pub mod layer;
mod mac_run;
mod nxkit;
mod options;
pub mod order;
pub mod pipeline;
mod programs;
mod provisional;
mod resource_patch;
pub mod rewrite;
mod stages;
mod standalone;
mod versions;

/// The exit code a failing stage asks for (the first one wins). Stage closures return `Result<String, String>`, so the
/// class of an error (damaged data 3, not supported yet 4, bad arguments 2, missing key 6) travels beside the text.
pub(crate) mod exit {
    use std::sync::atomic::{AtomicI32, Ordering};

    static CODE: AtomicI32 = AtomicI32::new(0);

    /// Records `code` unless an earlier failure recorded one.
    pub(crate) fn set(code: i32) {
        let _ = CODE.compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
    }

    /// The recorded code, 0 when none.
    pub(crate) fn get() -> i32 {
        CODE.load(Ordering::SeqCst)
    }

    /// `msg` as an error of class `code`.
    pub(crate) fn fail(code: i32, msg: impl Into<String>) -> String {
        set(code);
        msg.into()
    }
}

/// Stops the run on `SIGINT` or `SIGTERM`: every child process group is killed, the stage is closed as failed, the
/// exit code is 130. Waits for the signal on a thread of its own (the handler only records the number).
fn watch_interrupts(prog: std::sync::Arc<events::Progress>) {
    if nxb_sys::stdio::catch_interrupts().is_err() {
        return;
    }
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if let Some(sig) = nxb_sys::stdio::interrupted() {
                let n = nxb_tool::kill_everything();
                prog.end("interrupted", false);
                prog.say(&format!(
                    "[RED] interrupted (signal {sig}), {n} child process groups stopped"
                ));
                prog.finish();
                std::process::exit(130);
            }
        }
    });
}

pub use args::{Args, parse};

/// Whether the command line asks for `--progress json`.
fn wants_json(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--progress=json")
        || argv
            .windows(2)
            .any(|w| w[0] == "--progress" && w[1] == "json")
}

/// `nxb build <args>`: returns the exit code. With `--progress json` standard output holds the NDJSON events only:
/// the process's descriptor 1 is moved to standard error and the events go to the original standard output, so the
/// lines the extract and split steps print (and any other library or child output) never mix into the events.
pub fn main(argv: &[String]) -> i32 {
    let json = wants_json(argv);
    let mut sink: Box<dyn std::io::Write + Send> = Box::new(std::io::stdout());
    if json && let Ok(f) = nxb_sys::stdio::stdout_to_stderr() {
        sink = Box::new(f);
    }
    let a = match parse(argv) {
        Ok(a) => a,
        Err((code, msg)) => {
            if json {
                events::Progress::new(events::Mode::Json, sink).say(&format!("[RED] {msg}"));
            } else {
                println!("[RED] {msg}");
            }
            return code;
        }
    };
    let mode = match a.progress.as_str() {
        "json" => events::Mode::Json,
        "off" => events::Mode::Off,
        // the live bar is opt-in: `auto` and `lines` stay plain lines, so a log never holds escape codes
        "bar" => events::Mode::Bar,
        _ => events::Mode::Lines,
    };
    let prog = std::sync::Arc::new(events::Progress::new(mode, sink));
    watch_interrupts(prog.clone());
    let result = graph::run(&a, &prog);
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            prog.end("", false);
            prog.say(&format!("[RED] {e}"));
            // 3 damaged data, 4 not supported yet (the text starts with "unsupported" or the stage said so), 6 missing key
            match exit::get() {
                0 if e.starts_with("unsupported") => 4,
                0 => 1,
                c => c,
            }
        }
    };
    prog.finish();
    code
}
