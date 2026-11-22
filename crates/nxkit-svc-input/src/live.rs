//! Live input, recording and replay of player 1 (and the other players), for an agent or a person at the control
//! socket (W36).
//!
//! The host adds the verbs `press`, `hold`, `release`, `stick`, `input`, `record` and `replay` to the control socket.
//! Each one comes here ([`command`]). The grammar is the one of the input script (`timeline.rs`). What the verbs
//! inject and what a replay plays join the controllers, the keyboard and the input file: buttons are ORed and a stick
//! takes the larger deflection. The Home menu still takes the controllers ([`crate::set_blocked`]).
//!
//! Time. The game calls the service at its own pace, so the service has no clock of its own. A wait on the wall clock
//! uses `Instant`. A wait for frames uses the count of presented frames ([`frame_presented`], called by the layer
//! chain of the runtime for each present). The count starts at 0 with the process, so loading screens count too. A
//! recording in frame timing gives each change to the frame of the game that saw it.
//!
//! Without a verb the service does the work that it did before: `add` and `observe` return at once.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

pub use crate::frame::{frame_presented, frames};
use crate::npad::{NpadState, PLAYERS, RawKeys};
use crate::record::{Recorder, Timing};
use crate::script;
use crate::timeline::{Runner, Step, merge_keys, parse_step};

// Process-wide (W26): the take-overs of `nn::hid` have no context, and the control socket has its own thread. The
// verbs and the service share the state through the one mutex. The flag keeps the common case, no verb ever used,
// free of the lock.
static LIVE: Mutex<Option<Live>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// The state that the verbs make.
struct Live {
    /// What `press`, `hold`, `release`, `stick` and `input` set.
    inject: Runner,
    replay: Option<Runner>,
    rec: Option<Rec>,
    /// What the last replay did, for `replay` without arguments.
    last_replay: String,
}

struct Rec {
    recorder: Recorder,
    out: File,
    path: String,
    timing: Timing,
    lines: usize,
}

fn with_live<R>(f: impl FnOnce(&mut Live) -> R) -> R {
    let mut guard = LIVE.lock().unwrap_or_else(PoisonError::into_inner);
    let live = guard.get_or_insert_with(|| Live {
        inject: Runner::new(Vec::new(), Instant::now(), 0),
        replay: None,
        rec: None,
        last_replay: "no replay yet".to_owned(),
    });
    f(live)
}

/// What the file timeline, the injected input and a replay hold at `now`. `None` when none of them can hold anything.
fn overlays(now: Instant) -> Option<[RawKeys; PLAYERS]> {
    let file = script::active();
    let used = ACTIVE.load(Ordering::Relaxed);
    if !file && !used {
        return None;
    }
    let mut out = if file {
        script::overlays(now)
    } else {
        [RawKeys::default(); PLAYERS]
    };
    if used {
        let frame = frames();
        with_live(|l| {
            let mut join = |keys: [RawKeys; PLAYERS]| {
                for (o, k) in out.iter_mut().zip(keys) {
                    *o = merge_keys(*o, k);
                }
            };
            join(l.inject.poll(now, frame));
            if let Some(r) = l.replay.as_mut() {
                join(r.poll(now, frame));
                if r.done() {
                    l.last_replay = format!("the replay ended at frame {frame}");
                    eprintln!("nxkit-input: {}", l.last_replay);
                    l.replay = None;
                }
            }
        });
    }
    Some(out)
}

/// What `player` holds from the timeline of the file, the verbs and a replay, joined with `own` (the keyboard and the
/// scripted actions).
pub(crate) fn add(player: usize, own: Option<RawKeys>) -> Option<RawKeys> {
    #[cfg(test)]
    if !tests::RECORDER_THREAD.with(std::cell::Cell::get) {
        return own;
    }
    let Some(all) = overlays(Instant::now()) else {
        return own;
    };
    match all.get(player) {
        Some(o) if *o != RawKeys::default() => Some(merge_keys(own.unwrap_or_default(), *o)),
        _ => own,
    }
}

/// The reading of player 1 that goes to the game. A recording takes it.
pub(crate) fn observe(state: &NpadState) {
    if !ACTIVE.load(Ordering::Relaxed) || crate::system::blocked() {
        return;
    }
    // The tests of the crate run on parallel threads. Only the thread that records takes part.
    #[cfg(test)]
    if !tests::RECORDER_THREAD.with(std::cell::Cell::get) {
        return;
    }
    let (now, frame) = (Instant::now(), frames());
    let sticks = [state.lx, state.ly, state.rx, state.ry];
    with_live(|l| {
        let Some(rec) = l.rec.as_mut() else {
            return;
        };
        let lines = rec.recorder.observe(state.buttons, sticks, now, frame);
        if let Err(e) = write_lines(rec, &lines) {
            eprintln!("nxkit-input: the recording stops: {e}");
            l.rec = None;
        }
    });
}

fn write_lines(rec: &mut Rec, lines: &[String]) -> std::io::Result<()> {
    for line in lines {
        writeln!(rec.out, "{line}")?;
    }
    rec.lines += lines.len();
    Ok(())
}

fn exec(line: &str) -> Result<Vec<String>, String> {
    let step = parse_step(line)?;
    let player = match step {
        Step::Wait(_) => {
            return Err("wait is not a live command, a script file and a replay use it".to_owned());
        }
        Step::Set { player, .. } | Step::Release { player, .. } => player,
    };
    ACTIVE.store(true, Ordering::Relaxed);
    with_live(|l| l.inject.apply(&step, Instant::now()));
    Ok(vec![format!("player {} ok", player + 1)])
}

fn record(rest: &str) -> Result<Vec<String>, String> {
    let (sub, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let arg = arg.trim();
    match sub {
        "" | "status" => Ok(vec![with_live(|l| match &l.rec {
            Some(r) => format!(
                "recording to {} (timing {}, {} lines)",
                r.path,
                r.timing.name(),
                r.lines
            ),
            None => "not recording".to_owned(),
        })]),
        "start" => {
            // The timing is the last word, when it is one.
            let (path, timing) = match arg.rsplit_once(char::is_whitespace) {
                Some((p, t)) => match Timing::parse(t) {
                    Some(t) => (p.trim(), t),
                    None => (arg, Timing::Frames),
                },
                None => (arg, Timing::Frames),
            };
            if path.is_empty() {
                return Err("record start needs FILE [frames|ms]".to_owned());
            }
            let out = File::create(path).map_err(|e| format!("cannot write {path}: {e}"))?;
            let (recorder, head) = Recorder::new(timing, Instant::now(), frames());
            let mut rec = Rec {
                recorder,
                out,
                path: path.to_owned(),
                timing,
                lines: 0,
            };
            write_lines(&mut rec, &head).map_err(|e| format!("cannot write {path}: {e}"))?;
            ACTIVE.store(true, Ordering::Relaxed);
            let said = format!("recording player 1 to {path} (timing {})", timing.name());
            with_live(|l| l.rec = Some(rec));
            Ok(vec![said])
        }
        "stop" => with_live(|l| {
            let mut rec = l.rec.take().ok_or("not recording")?;
            let end = rec.recorder.finish(Instant::now(), frames());
            write_lines(&mut rec, &end).map_err(|e| format!("cannot write {}: {e}", rec.path))?;
            Ok(vec![format!("saved {} ({} lines)", rec.path, rec.lines)])
        }),
        other => Err(format!("record {other:?} is not start, stop or status")),
    }
}

fn replay(rest: &str) -> Result<Vec<String>, String> {
    match rest {
        "" | "status" => Ok(vec![with_live(|l| match &l.replay {
            Some(r) => {
                let (at, all) = r.progress();
                format!("replaying: {at} of {all} commands, at frame {}", frames())
            }
            None => l.last_replay.clone(),
        })]),
        "stop" => with_live(|l| {
            l.replay.take().ok_or("no replay runs")?;
            l.last_replay = "the replay was stopped".to_owned();
            Ok(vec!["stopped".to_owned()])
        }),
        path => {
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
            let parsed = script::parse(&text);
            if parsed.steps.is_empty() {
                return Err(format!(
                    "{path} has no timeline commands (wait, press, ...)"
                ));
            }
            let n = parsed.steps.len();
            let mut said = vec![format!("replaying {n} commands from {path}")];
            said.extend(parsed.bad.iter().map(|b| format!("skipped {b:?}")));
            ACTIVE.store(true, Ordering::Relaxed);
            let runner = Runner::new(parsed.steps, Instant::now(), frames());
            with_live(|l| l.replay = Some(runner));
            Ok(said)
        }
    }
}

/// Runs a verb of the control socket: `press`, `hold`, `release`, `stick`, `input`, `record` or `replay`. `rest` is
/// the line after the verb. The answer is its lines, or why it failed.
pub fn command(verb: &str, rest: &str) -> Result<Vec<String>, String> {
    let rest = rest.trim();
    match verb {
        "press" | "hold" | "release" | "stick" => exec(&format!("{verb} {rest}")),
        "input" => exec(rest),
        "record" => record(rest),
        "replay" => replay(rest),
        other => Err(format!("{other:?} is not an input verb")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::npad::btn;

    thread_local! {
        pub(super) static RECORDER_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    fn state(buttons: u64) -> NpadState {
        NpadState {
            buttons,
            lx: 0,
            ly: 0,
            rx: 0,
            ry: 0,
            attributes: 0,
        }
    }

    fn at_frame(n: u64) {
        while frames() < n {
            frame_presented();
        }
    }

    // One test, because the state of the verbs is process-wide.
    #[test]
    fn verbs_record_and_replay_to_the_same_input() {
        RECORDER_THREAD.with(|c| c.set(true));
        // Bad verbs fail with a reason.
        assert!(command("press", "").is_err());
        assert!(command("input", "wait 100").is_err());
        assert!(command("input", "jump").is_err());
        assert!(command("record", "start").is_err());
        assert!(command("record", "stop").is_err());
        assert!(command("replay", "stop").is_err());
        assert!(command("replay", "/nonexistent/file").is_err());
        assert!(command("frob", "").is_err());
        // A held button joins the player.
        assert_eq!(
            command("hold", "b player 2"),
            Ok(vec!["player 2 ok".to_owned()])
        );
        assert_eq!(add(1, None).map(|k| k.buttons), Some(btn::B));
        assert_eq!(add(0, None), None);
        assert!(command("release", "player 2").is_ok());
        assert_eq!(add(1, None), None);
        // Record what the game sees for a few frames.
        let dir = std::env::temp_dir().join(format!("nxkit-live-{}", std::process::id()));
        assert!(std::fs::create_dir_all(&dir).is_ok());
        let file = dir.join("rec.txt");
        let path = file.display().to_string();
        let base = frames() + 10;
        at_frame(base);
        assert!(command("record", &format!("start {path}")).is_ok());
        for (f, b) in [(2, btn::A), (3, btn::A), (4, 0), (6, btn::ZR)] {
            at_frame(base + f);
            observe(&state(b));
        }
        at_frame(base + 8);
        let stopped = command("record", "stop");
        assert!(
            matches!(&stopped, Ok(l) if l[0].contains("saved")),
            "{stopped:?}"
        );
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        let body: Vec<&str> = text.lines().skip(1).collect();
        assert_eq!(
            body,
            [
                format!("wait frame {base}").as_str(),
                "wait frames 2",
                "hold a",
                "wait frames 2",
                "release a",
                "wait frames 2",
                "hold zr",
                "wait frames 2",
                "release"
            ]
        );
        // Play it back. The `wait frame` of the file is past, so the replay starts at once.
        let first = frames();
        let started = command("replay", &path);
        assert!(started.is_ok(), "{started:?}");
        let seen: Vec<u64> = (0..8)
            .map(|i| {
                at_frame(first + i);
                add(0, None).map_or(0, |k| k.buttons)
            })
            .collect();
        assert_eq!(seen, [0, 0, btn::A, btn::A, 0, 0, btn::ZR, btn::ZR]);
        assert!(matches!(command("replay", ""), Ok(l) if l[0].starts_with("replaying")));
        // The other tests of the crate must see no input.
        assert!(command("replay", "stop").is_ok());
        assert_eq!(add(0, None), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
