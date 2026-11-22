//! Scripted input for a run that nobody attends: `NXKIT_INPUT_FILE=<path>` names a file with buttons that player 1
//! holds or presses at an interval (a diagnostic).
//!
//! The file is read again at most twice a second, at a call of the game, the way `NXKIT_SKIP_FILE` is (R39: no
//! thread). A missing or empty file means no scripted input. Entries are separated by commas or lines:
//!
//! - `hold <action>`: held while the line is in the file.
//! - `pulse <action> <on_ms> <period_ms>`: held for `on_ms` at the start of every `period_ms`, counted from the
//!   moment the content with the line was first read.
//!
//! An `<action>` is a name of `bind::action` (`a`, `zr`, `plus`, `dpad_up`, `ls_up`, `rs_left` and so on). A line that
//! starts with `#` is a comment. The actions join those of the keyboard before the sticks are worked out.
//!
//! A file with a timeline command (`wait`, `press`, `release`, `stick`, or a `hold` of several buttons) is a timeline.
//! Its lines run in order, and `hold` holds until a `release` (see `timeline.rs` for the commands). A recording of the
//! `record` verb is a timeline. A `pulse` line of a timeline still runs from the start. When the text of the file
//! changes, the timeline starts again.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::bind::{Action, action};
use crate::npad::{PLAYERS, RawKeys};
use crate::timeline::{Runner, Step, parse_step};

/// The file is read again when the last read is at least this old.
const REREAD: Duration = Duration::from_millis(500);

/// One entry of the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Held all the time.
    Hold(Action),
    /// Held for `on_ms` at the start of each `period_ms`.
    Pulse {
        action: Action,
        on_ms: u64,
        period_ms: u64,
    },
}

impl Entry {
    /// The held action at `elapsed_ms` after the content was first read: a pure function of the time.
    pub(crate) fn held(&self, elapsed_ms: u64) -> Option<Action> {
        match *self {
            Entry::Hold(a) => Some(a),
            Entry::Pulse {
                action,
                on_ms,
                period_ms,
            } => (elapsed_ms % period_ms < on_ms).then_some(action),
        }
    }
}

/// The action of a name, for a button or a stick direction only.
fn scripted_action(name: &str) -> Option<Action> {
    match action(name) {
        Ok(Some(Action::Walk)) | Ok(None) | Err(()) => None,
        Ok(Some(a)) => Some(a),
    }
}

/// An entry of one line, or `None` when the line is not valid.
fn parse_line(line: &str) -> Option<Entry> {
    let mut words = line.split_whitespace();
    let kind = words.next()?.to_ascii_lowercase();
    let a = scripted_action(words.next()?)?;
    let entry = match kind.as_str() {
        "hold" => Entry::Hold(a),
        "pulse" => {
            let on_ms: u64 = words.next()?.parse().ok()?;
            let period_ms: u64 = words.next()?.parse().ok()?;
            if period_ms == 0 {
                return None;
            }
            Entry::Pulse {
                action: a,
                on_ms,
                period_ms,
            }
        }
        _ => return None,
    };
    words.next().is_none().then_some(entry)
}

/// What a file text holds.
pub(crate) struct Parsed {
    /// The `hold` and `pulse` lines of a file that is not a timeline, or the `pulse` lines of a timeline. With their text
    /// (words joined by one space).
    pub(crate) entries: Vec<(String, Entry)>,
    /// The commands of a timeline, in order. Empty when the file is not a timeline.
    pub(crate) steps: Vec<Step>,
    /// The lines that do not parse.
    pub(crate) bad: Vec<String>,
}

/// One line before it is known whether the file is a timeline.
enum Line {
    Legacy(String, Entry),
    /// A `hold`: one action is a legacy entry, and every hold is a step.
    Hold(String, Option<Entry>, Step),
    Step(Step),
}

/// The lines of a file text, separated by commas or lines.
pub(crate) fn parse(text: &str) -> Parsed {
    let mut lines = Vec::new();
    let mut bad = Vec::new();
    for line in text.split([',', '\n']) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let norm = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let kind = norm.split(' ').next().unwrap_or("").to_ascii_lowercase();
        let parsed = match kind.as_str() {
            "pulse" => parse_line(line).map(|e| Line::Legacy(norm.clone(), e)),
            "hold" => parse_step(line)
                .ok()
                .map(|s| Line::Hold(norm.clone(), parse_line(line), s)),
            _ => parse_step(line).ok().map(Line::Step),
        };
        match parsed {
            Some(l) => lines.push(l),
            None => bad.push(norm),
        }
    }
    let timeline = lines
        .iter()
        .any(|l| matches!(l, Line::Step(_) | Line::Hold(_, None, _)));
    let (mut entries, mut steps) = (Vec::new(), Vec::new());
    for l in lines {
        match l {
            Line::Legacy(t, e) => entries.push((t, e)),
            Line::Hold(_, _, s) if timeline => steps.push(s),
            Line::Hold(t, Some(e), _) => entries.push((t, e)),
            Line::Hold(..) => {}
            Line::Step(s) => steps.push(s),
        }
    }
    Parsed {
        entries,
        steps,
        bad,
    }
}

/// What was read last.
struct Cache {
    /// When the file was read last.
    checked: Option<Instant>,
    /// The text that gave the entries.
    text: String,
    /// When this text was first read. The pulses count from it.
    since: Instant,
    entries: Vec<(String, Entry)>,
    /// The timeline of the text, when it is one.
    runner: Option<Runner>,
    /// The message that the timeline ended was said.
    ended: bool,
    /// What the last message named.
    shown: String,
    /// The bad lines that were reported.
    reported: HashSet<String>,
}

/// The scripted input of a process: the file and the cache of its content.
pub(crate) struct ScriptFile {
    path: Option<PathBuf>,
    cache: Mutex<Option<Cache>>,
}

impl ScriptFile {
    pub(crate) fn new(path: Option<PathBuf>) -> ScriptFile {
        ScriptFile {
            path,
            cache: Mutex::new(None),
        }
    }

    /// True when a file is named.
    pub(crate) fn active(&self) -> bool {
        self.path.is_some()
    }

    /// Reads the file when it is due, then runs `f` on the cache. `read` gives the text of the file (empty when it
    /// cannot be read) and is called at most every 500 ms, and not at all when no file is named. `say` takes the
    /// messages. A new text starts its timeline at `now` and `frame`.
    fn with_cache<R>(
        &self,
        now: Instant,
        frame: u64,
        read: impl FnOnce(&Path) -> String,
        mut say: impl FnMut(&str),
        f: impl FnOnce(&mut Cache, &mut dyn FnMut(&str)) -> R,
    ) -> Option<R> {
        let path = self.path.as_ref()?;
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let cache = guard.get_or_insert_with(|| Cache {
            checked: None,
            text: String::new(),
            since: now,
            entries: Vec::new(),
            runner: None,
            ended: false,
            shown: "none".to_owned(),
            reported: HashSet::new(),
        });
        let stale = cache
            .checked
            .is_none_or(|t| now.saturating_duration_since(t) >= REREAD);
        if stale {
            cache.checked = Some(now);
            let text = read(path);
            if text != cache.text {
                let parsed = parse(&text);
                for line in parsed.bad {
                    if cache.reported.insert(line.clone()) {
                        say(&format!("nxkit-input: scripted input: skipped \"{line}\""));
                    }
                }
                let mut names: Vec<String> =
                    parsed.entries.iter().map(|(t, _)| t.clone()).collect();
                if !parsed.steps.is_empty() {
                    names.push(format!("timeline of {} steps", parsed.steps.len()));
                }
                let shown = if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                };
                if shown != cache.shown {
                    say(&format!("nxkit-input: scripted input: {shown}"));
                    cache.shown = shown;
                }
                cache.text = text;
                cache.entries = parsed.entries;
                cache.runner =
                    (!parsed.steps.is_empty()).then(|| Runner::new(parsed.steps, now, frame));
                cache.ended = false;
                cache.since = now;
            }
        }
        Some(f(cache, &mut say))
    }

    /// The actions held at `now`: the `hold` and `pulse` lines of a file that is not a timeline, and the `pulse` lines
    /// of a timeline.
    pub(crate) fn held_at(
        &self,
        now: Instant,
        frame: u64,
        read: impl FnOnce(&Path) -> String,
        say: impl FnMut(&str),
    ) -> Vec<Action> {
        self.with_cache(now, frame, read, say, |cache, _| {
            let elapsed = now.saturating_duration_since(cache.since).as_millis() as u64;
            cache
                .entries
                .iter()
                .filter_map(|(_, e)| e.held(elapsed))
                .collect()
        })
        .unwrap_or_default()
    }

    /// What the timeline of the file gives each player at `now`, with `frame` frames presented.
    pub(crate) fn overlays_at(
        &self,
        now: Instant,
        frame: u64,
        read: impl FnOnce(&Path) -> String,
        say: impl FnMut(&str),
    ) -> [RawKeys; PLAYERS] {
        self.with_cache(now, frame, read, say, |cache, say| {
            let Some(runner) = cache.runner.as_mut() else {
                return [RawKeys::default(); PLAYERS];
            };
            let out = runner.poll(now, frame);
            if runner.done() && !cache.ended {
                cache.ended = true;
                say(&format!(
                    "nxkit-input: scripted input: the timeline ended at frame {frame}"
                ));
            }
            out
        })
        .unwrap_or([RawKeys::default(); PLAYERS])
    }
}

fn global() -> &'static ScriptFile {
    static FILE: OnceLock<ScriptFile> = OnceLock::new();
    FILE.get_or_init(|| ScriptFile::new(crate::settings::settings().script_file.clone()))
}

/// True when `NXKIT_INPUT_FILE` is set. Player 1 then has scripted input even without a controller or a keyboard.
pub(crate) fn active() -> bool {
    global().active()
}

/// The actions that the input file holds at `now`. Empty, without a read, when the variable is not set.
pub(crate) fn held(now: Instant) -> Vec<Action> {
    global().held_at(
        now,
        crate::frame::frames(),
        |p| std::fs::read_to_string(p).unwrap_or_default(),
        |m| eprintln!("{m}"),
    )
}

/// What the timeline of the input file gives each player at `now`. Nothing without the variable.
pub(crate) fn overlays(now: Instant) -> [RawKeys; PLAYERS] {
    global().overlays_at(
        now,
        crate::frame::frames(),
        |p| std::fs::read_to_string(p).unwrap_or_default(),
        |m| eprintln!("{m}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bind::{Bindings, Dir, MouseButtons, Source, raw_from, sources};
    use crate::keys::KeySet;
    use crate::npad::btn;

    const A: Action = Action::Button(btn::A);

    fn legacy(text: &str) -> (Vec<(String, Entry)>, Vec<String>) {
        let p = parse(text);
        (p.entries, p.bad)
    }

    fn keys(name: &str) -> KeySet {
        let mut keys = KeySet::default();
        for s in sources(name).unwrap_or_default() {
            if let Source::Key(u) = s {
                keys.set(u);
            }
        }
        keys
    }

    #[test]
    fn hold_and_pulse_lines_legacy() {
        let (e, bad) = legacy("hold a");
        assert_eq!(e[0].1, Entry::Hold(A));
        assert!(bad.is_empty());
        let (e, _) = legacy("pulse a 100 1500");
        assert_eq!(
            e[0].1,
            Entry::Pulse {
                action: A,
                on_ms: 100,
                period_ms: 1500
            }
        );
    }

    #[test]
    fn several_entries_by_comma_or_line() {
        let (e, bad) = legacy("hold  zr, pulse b 50 400\nhold ls_up\n# a comment\n");
        let texts: Vec<&str> = e.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["hold zr", "pulse b 50 400", "hold ls_up"]);
        assert_eq!(e[2].1, Entry::Hold(Action::LeftStick(Dir::Up)));
        assert!(bad.is_empty());
    }

    #[test]
    fn bad_lines_are_skipped() {
        let (e, bad) = legacy(
            "hold a, hold jump, pulse a 100, pulse a x 5, pulse a 1 0, hold walk, press, wait x, hold",
        );
        assert_eq!(e.len(), 1);
        assert_eq!(bad.len(), 8);
    }

    #[test]
    fn the_pulse_is_a_function_of_the_elapsed_time() {
        let p = Entry::Pulse {
            action: A,
            on_ms: 100,
            period_ms: 1500,
        };
        assert_eq!(p.held(0), Some(A));
        assert_eq!(p.held(99), Some(A));
        assert_eq!(p.held(100), None);
        assert_eq!(p.held(1499), None);
        assert_eq!(p.held(1500), Some(A));
        assert_eq!(p.held(1599), Some(A));
        assert_eq!(p.held(1600), None);
        assert_eq!(Entry::Hold(A).held(123_456), Some(A));
    }

    #[test]
    fn a_scripted_a_sets_the_bit_of_the_key_of_a() {
        let by_key = Bindings::pc().read(&keys("e"), MouseButtons::default());
        assert_eq!(by_key.buttons, btn::A);
        assert_eq!(raw_from(&[A]).buttons, by_key.buttons);
    }

    #[test]
    fn a_scripted_direction_and_the_opposite_key_cancel() {
        let mut held = vec![Action::LeftStick(Dir::Up)];
        held.extend(Bindings::pc().held(&keys("s"), MouseButtons::default()));
        assert_eq!(raw_from(&held).sticks, [0; 4]);
        held.pop();
        assert_eq!(raw_from(&held).sticks[1], 32767);
    }

    fn file() -> ScriptFile {
        ScriptFile::new(Some(PathBuf::from("script")))
    }

    #[test]
    fn nothing_is_read_without_the_variable() {
        let f = ScriptFile::new(None);
        let mut read = false;
        let held = f.held_at(
            Instant::now(),
            0,
            |_| {
                read = true;
                "hold a".to_owned()
            },
            |_| {},
        );
        assert!(held.is_empty());
        assert!(!read);
        assert!(!f.active());
    }

    #[test]
    fn the_file_is_read_twice_a_second_and_pulses_count_from_the_first_read() {
        let f = file();
        let t0 = Instant::now();
        let mut reads = 0;
        let mut said = Vec::new();
        let mut at = |ms: u64| {
            f.held_at(
                t0 + Duration::from_millis(ms),
                0,
                |_| {
                    reads += 1;
                    "pulse a 100 1500".to_owned()
                },
                |m| said.push(m.to_owned()),
            )
        };
        assert_eq!(at(0), [A]);
        assert!(at(300).is_empty());
        assert!(at(600).is_empty());
        assert_eq!(at(1500), [A]);
        // The same text again does not restart the pulse nor say anything.
        assert_eq!(reads, 3);
        assert_eq!(said, ["nxkit-input: scripted input: pulse a 100 1500"]);
    }

    #[test]
    fn a_changed_file_says_so_once_and_bad_lines_once() {
        let f = file();
        let t0 = Instant::now();
        let mut said = Vec::new();
        let mut run = |ms: u64, text: &str| {
            f.held_at(
                t0 + Duration::from_millis(ms),
                0,
                |_| text.to_owned(),
                |m| said.push(m.to_owned()),
            )
        };
        assert!(run(0, "").is_empty());
        assert_eq!(run(500, "hold a, hold jump"), [A]);
        assert_eq!(run(1000, "hold a, hold jump\n"), [A]);
        assert_eq!(run(1500, "hold jump, hold a, hold a").len(), 2);
        assert!(run(2000, "").is_empty());
        assert_eq!(
            said,
            [
                "nxkit-input: scripted input: skipped \"hold jump\"",
                "nxkit-input: scripted input: hold a",
                "nxkit-input: scripted input: hold a, hold a",
                "nxkit-input: scripted input: none",
            ]
        );
    }

    #[test]
    fn a_file_with_a_timeline_command_is_a_timeline() {
        let p = parse("hold a\nwait 10\nrelease a\npulse b 5 50\nhold zr plus");
        assert!(p.bad.is_empty());
        assert_eq!(p.steps.len(), 4);
        assert_eq!(p.entries.len(), 1);
        // Without a timeline command a hold stays a legacy entry.
        let p = parse("hold a, hold b, pulse x 1 9");
        assert_eq!((p.steps.len(), p.entries.len()), (0, 3));
        // A hold of two buttons has no legacy meaning.
        assert_eq!(parse("hold a b").steps.len(), 1);
    }

    #[test]
    fn the_timeline_of_a_file_runs_by_frames_and_restarts_when_the_text_changes() {
        let f = file();
        let t0 = Instant::now();
        let text = "wait frames 2\nhold a\nwait frames 2\nrelease a";
        let mut said = Vec::new();
        let mut at = |frame: u64, text: &str| {
            f.overlays_at(
                t0 + Duration::from_millis(frame * 600),
                frame,
                |_| text.to_owned(),
                |m| said.push(m.to_owned()),
            )[0]
            .buttons
        };
        // The timeline starts at frame 10, when the file is first read.
        assert_eq!(at(10, text), 0);
        assert_eq!(at(11, text), 0);
        assert_eq!(at(12, text), btn::A);
        assert_eq!(at(13, text), btn::A);
        assert_eq!(at(14, text), 0);
        assert_eq!(at(20, "hold a\nrelease zr"), btn::A);
        assert_eq!(
            said.last().map(String::as_str),
            Some("nxkit-input: scripted input: the timeline ended at frame 20")
        );
    }
}
