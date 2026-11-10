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

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::bind::{Action, action};

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

/// The lines of a file text: the entries with their text (words joined by one space), and the lines that do not
/// parse.
pub(crate) fn parse(text: &str) -> (Vec<(String, Entry)>, Vec<String>) {
    let mut entries = Vec::new();
    let mut bad = Vec::new();
    for line in text.split([',', '\n']) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let norm = line.split_whitespace().collect::<Vec<_>>().join(" ");
        match parse_line(line) {
            Some(e) => entries.push((norm, e)),
            None => bad.push(norm),
        }
    }
    (entries, bad)
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

    /// The actions held at `now`. `read` gives the text of the file (empty when it cannot be read) and is called at
    /// most every 500 ms, and not at all when no file is named. `say` takes the messages.
    pub(crate) fn held_at(
        &self,
        now: Instant,
        read: impl FnOnce(&Path) -> String,
        mut say: impl FnMut(&str),
    ) -> Vec<Action> {
        let Some(path) = &self.path else {
            return Vec::new();
        };
        let mut guard = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let cache = guard.get_or_insert_with(|| Cache {
            checked: None,
            text: String::new(),
            since: now,
            entries: Vec::new(),
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
                let (entries, bad) = parse(&text);
                for line in bad {
                    if cache.reported.insert(line.clone()) {
                        say(&format!("nxkit-input: scripted input: skipped \"{line}\""));
                    }
                }
                let names: Vec<&str> = entries.iter().map(|(t, _)| t.as_str()).collect();
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
                cache.entries = entries;
                cache.since = now;
            }
        }
        let elapsed = now.saturating_duration_since(cache.since).as_millis() as u64;
        cache
            .entries
            .iter()
            .filter_map(|(_, e)| e.held(elapsed))
            .collect()
    }
}

fn global() -> &'static ScriptFile {
    static FILE: OnceLock<ScriptFile> = OnceLock::new();
    FILE.get_or_init(|| {
        ScriptFile::new(
            std::env::var_os("NXKIT_INPUT_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        )
    })
}

/// True when `NXKIT_INPUT_FILE` is set. Player 1 then has scripted input even without a controller or a keyboard.
pub(crate) fn active() -> bool {
    global().active()
}

/// The actions that the input file holds at `now`. Empty, without a read, when the variable is not set.
pub(crate) fn held(now: Instant) -> Vec<Action> {
    global().held_at(
        now,
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
    fn hold_and_pulse_lines_parse() {
        let (e, bad) = parse("hold a");
        assert_eq!(e[0].1, Entry::Hold(A));
        assert!(bad.is_empty());
        let (e, _) = parse("pulse a 100 1500");
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
        let (e, bad) = parse("hold  zr, pulse b 50 400\nhold ls_up\n# a comment\n");
        let texts: Vec<&str> = e.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(texts, ["hold zr", "pulse b 50 400", "hold ls_up"]);
        assert_eq!(e[2].1, Entry::Hold(Action::LeftStick(Dir::Up)));
        assert!(bad.is_empty());
    }

    #[test]
    fn bad_lines_are_skipped() {
        let (e, bad) = parse(
            "hold a, hold jump, pulse a 100, pulse a x 5, pulse a 1 0, hold walk, press a, hold a b, hold",
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
}
