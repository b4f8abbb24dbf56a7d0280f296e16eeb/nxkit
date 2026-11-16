//! `nxb build`: the app of a game that has a title module, made for nxkit. One run: extract (packages or NUP, with the
//! shader split) -> romfs -> rewrite -> manifest -> link -> shaders and pipeline archive -> bundle, with
//! `nxkit-host` as the program of the app (`standalone`). Console events as `--progress json` NDJSON ([`events`]),
//! exit codes 0 done, 1 error, 2 bad arguments, 3 damaged data, 4 not supported, 6 missing key, 130 interrupted.
//! `--dry-run` prints the plan and writes nothing. `--auto-analyze` selects the generic path without a title module.

pub mod archive_layout;
mod args;
pub mod events;
mod extract_stage;
pub mod graph;
pub mod host;
pub mod inputs;
pub mod pipeline;
mod resource_patch;
mod standalone;

#[cfg(test)]
#[path = "../../nxb-extract/tests/common/mod.rs"]
mod extract_fixture;
#[cfg(test)]
#[path = "../../nxb-extract/tests/common/nup.rs"]
mod nup_fixture;

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
                    "nxb: error: interrupted (signal {sig}), {n} child process groups stopped"
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
                events::Progress::new(events::Mode::Json, sink).say(&format!("nxb: error: {msg}"));
            } else {
                println!("nxb: error: {msg}");
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
    let result = standalone::run(&a, &prog);
    let code = match result {
        Ok(()) => 0,
        Err(e) => {
            prog.end("", false);
            prog.say(&format!("nxb: error: {e}"));
            // 3 damaged data, 4 not supported (the text starts with "unsupported" or the stage said so), 6 missing key
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
