//! The input timeline: the commands of one line and the runner that plays a list of them.
//!
//! One grammar serves the input script (`NXKIT_INPUT_FILE`), a recording and the live verbs of the control socket.
//! A command is one line. Buttons are the names of `bind::action`. Several buttons are written as `a b` or `a+b`.
//! A trailing `player N` (1 to 8) picks the player. The default is player 1.
//!
//! - `press BUTTONS [MS]`: held for MS milliseconds (100), then released.
//! - `hold BUTTONS`: held until a `release`.
//! - `release [BUTTONS]`: lets the buttons go. Without buttons it lets everything go.
//! - `stick l|r X Y [MS]`: a stick at X and Y (-1 to 1, Y up). Without MS it stays until the next `stick`.
//! - `wait MS`, `wait 500ms`, `wait 2.5s`: waits on the wall clock.
//! - `wait frames N`: waits for N presented frames.
//! - `wait frame N`: waits until N frames have been presented since the start of the process.
//!
//! The [`Runner`] plays the commands in order. A `press` and a `hold` do not wait. Only a `wait` stops the cursor.
//! The cursor does not drift: a wait on the wall clock ends at the time it was due, not at the time of the poll. A wait
//! for frames ends at the frame it was due.

use std::time::{Duration, Instant};

use crate::bind::{Action, action, raw_from};
use crate::npad::{PLAYERS, RawKeys, pick, to_stick};

/// The longest time of a press or a wait on the wall clock.
const MAX_MS: u64 = 3_600_000;
/// The time of a `press` without a time.
const PRESS_MS: u64 = 100;

/// What a `wait` waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wait {
    /// Milliseconds on the wall clock.
    Ms(u64),
    /// This many presented frames from the cursor.
    Frames(u64),
    /// Until the count of presented frames since the start is this number.
    UntilFrame(u64),
}

/// One command.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Step {
    Wait(Wait),
    /// Buttons and sticks of a player. `ms` is the time of a press. `None` holds until a release.
    Set {
        player: usize,
        buttons: u64,
        sticks: [Option<i32>; 4],
        ms: Option<u64>,
    },
    /// Lets buttons and sticks go. Every stick that is `true` returns to the centre.
    Release {
        player: usize,
        buttons: u64,
        sticks: [bool; 4],
    },
}

/// Takes a trailing `player N` off the words. Returns the player index.
fn take_player(words: &mut Vec<&str>) -> Result<usize, String> {
    let n = words.len();
    if n >= 2 && words[n - 2].eq_ignore_ascii_case("player") {
        let id: usize = words[n - 1]
            .parse()
            .ok()
            .filter(|p| (1..=PLAYERS).contains(p))
            .ok_or_else(|| format!("player must be 1 to {PLAYERS}, not {:?}", words[n - 1]))?;
        words.truncate(n - 2);
        return Ok(id - 1);
    }
    Ok(0)
}

fn parse_ms(word: &str) -> Result<u64, String> {
    let (num, unit) = match word.strip_suffix("ms") {
        Some(n) => (n, 1.0),
        None => match word.strip_suffix('s') {
            Some(n) => (n, 1000.0),
            None => (word, 1.0),
        },
    };
    let v: f64 = num
        .parse()
        .map_err(|_| format!("{word:?} is not a time (MS, 500ms or 2.5s)"))?;
    let ms = v * unit;
    if !ms.is_finite() || !(0.0..=MAX_MS as f64).contains(&ms) {
        return Err(format!("{word:?} is out of range (0 to {MAX_MS} ms)"));
    }
    Ok(ms.round() as u64)
}

fn parse_count(word: &str) -> Result<u64, String> {
    word.parse()
        .map_err(|_| format!("{word:?} is not a frame number"))
}

fn parse_wait(args: &[&str]) -> Result<Wait, String> {
    match args {
        [kind, n] if kind.eq_ignore_ascii_case("frames") => Ok(Wait::Frames(parse_count(n)?)),
        [kind, n] if kind.eq_ignore_ascii_case("frame") => Ok(Wait::UntilFrame(parse_count(n)?)),
        [t] => Ok(Wait::Ms(parse_ms(t)?)),
        _ => Err("wait needs MS, SECONDSs, frames N or frame N".to_owned()),
    }
}

/// The actions of the button words. A button is a name, or names joined with `+`.
fn parse_buttons(words: &[&str]) -> Result<Vec<Action>, String> {
    let mut out = Vec::new();
    for word in words {
        let one = |name: &str| match action(name) {
            Ok(Some(Action::Walk)) | Ok(None) | Err(()) => Err(format!("{name:?} is not a button")),
            Ok(Some(a)) => Ok(a),
        };
        match one(word) {
            Ok(a) => out.push(a),
            Err(e) if !word.contains('+') || *word == "+" => return Err(e),
            Err(_) => {
                for part in word.split('+') {
                    out.push(one(part)?);
                }
            }
        }
    }
    if out.is_empty() {
        return Err("no button named".to_owned());
    }
    Ok(out)
}

/// The sticks that the stick directions of `keys` push. The other axes stay `None`.
fn axes_of(keys: &RawKeys, actions: &[Action]) -> [Option<i32>; 4] {
    let touched = actions
        .iter()
        .any(|a| matches!(a, Action::LeftStick(_) | Action::RightStick(_)));
    let mut out = [None; 4];
    if touched {
        for (o, v) in out.iter_mut().zip(keys.sticks) {
            *o = (v != 0).then_some(v);
        }
    }
    out
}

fn parse_stick(args: &[&str]) -> Result<([Option<i32>; 4], Option<u64>), String> {
    let (side, rest) = args.split_first().ok_or("stick needs l|r X Y")?;
    let at = match side.to_ascii_lowercase().as_str() {
        "l" | "ls" | "left" => 0,
        "r" | "rs" | "right" => 2,
        other => return Err(format!("{other:?} is not a stick (l or r)")),
    };
    let axis = |w: &str| {
        w.parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| to_stick(v.clamp(-1.0, 1.0)))
            .ok_or_else(|| format!("{w:?} is not a stick value (-1 to 1)"))
    };
    let (x, y, ms) = match rest {
        [x, y] => (axis(x)?, axis(y)?, None),
        [x, y, t] => (axis(x)?, axis(y)?, Some(parse_ms(t)?)),
        _ => return Err("stick needs l|r X Y [MS]".to_owned()),
    };
    let mut sticks = [None; 4];
    sticks[at] = Some(x);
    sticks[at + 1] = Some(y);
    Ok((sticks, ms))
}

/// The step of one line. `Err` says what is wrong.
pub(crate) fn parse_step(line: &str) -> Result<Step, String> {
    let mut words: Vec<&str> = line.split_whitespace().collect();
    let player = take_player(&mut words)?;
    let Some((verb, args)) = words.split_first() else {
        return Err("empty command".to_owned());
    };
    match verb.to_ascii_lowercase().as_str() {
        "wait" => parse_wait(args).map(Step::Wait),
        "press" => {
            let (names, ms) = match args.split_last() {
                Some((t, rest)) if !rest.is_empty() && t.parse::<u64>().is_ok() => {
                    (rest, parse_ms(t)?)
                }
                _ => (args, PRESS_MS),
            };
            let actions = parse_buttons(names)?;
            let keys = raw_from(&actions);
            Ok(Step::Set {
                player,
                buttons: keys.buttons,
                sticks: axes_of(&keys, &actions),
                ms: Some(ms),
            })
        }
        "hold" => {
            let actions = parse_buttons(args)?;
            let keys = raw_from(&actions);
            Ok(Step::Set {
                player,
                buttons: keys.buttons,
                sticks: axes_of(&keys, &actions),
                ms: None,
            })
        }
        "release" => {
            if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("all")) {
                return Ok(Step::Release {
                    player,
                    buttons: u64::MAX,
                    sticks: [true; 4],
                });
            }
            let actions = parse_buttons(args)?;
            let keys = raw_from(&actions);
            Ok(Step::Release {
                player,
                buttons: keys.buttons,
                sticks: axes_of(&keys, &actions).map(|s| s.is_some()),
            })
        }
        "stick" => {
            let (sticks, ms) = parse_stick(args)?;
            Ok(Step::Set {
                player,
                buttons: 0,
                sticks,
                ms,
            })
        }
        other => Err(format!("{other:?} is not an input command")),
    }
}

/// The two readings of one player joined: buttons are ORed and a stick takes the larger deflection.
pub(crate) fn merge_keys(a: RawKeys, b: RawKeys) -> RawKeys {
    let mut sticks = a.sticks;
    for (s, o) in sticks.iter_mut().zip(b.sticks) {
        *s = pick(*s, o);
    }
    RawKeys {
        buttons: a.buttons | b.buttons,
        sticks,
    }
}

/// A press that ends at a time.
struct Timed {
    player: usize,
    buttons: u64,
    sticks: [Option<i32>; 4],
    until: Instant,
    /// A poll has shown it. A press shorter than the gap between two polls is still shown once.
    seen: bool,
}

/// Plays a list of steps. It also takes steps at once ([`Runner::apply`]), for the live verbs.
pub(crate) struct Runner {
    steps: Vec<Step>,
    next: usize,
    cursor: Instant,
    cursor_frame: u64,
    held: [RawKeys; PLAYERS],
    timed: Vec<Timed>,
}

impl Runner {
    /// A runner that starts at `now`, when `frame` frames were presented.
    pub(crate) fn new(steps: Vec<Step>, now: Instant, frame: u64) -> Runner {
        Runner {
            steps,
            next: 0,
            cursor: now,
            cursor_frame: frame,
            held: [RawKeys::default(); PLAYERS],
            timed: Vec::new(),
        }
    }

    /// True when every step has run.
    pub(crate) fn done(&self) -> bool {
        self.next >= self.steps.len()
    }

    /// The steps that have run and the steps in all.
    pub(crate) fn progress(&self) -> (usize, usize) {
        (self.next, self.steps.len())
    }

    /// Runs one step that is not a wait, as of `at`. A wait does nothing.
    pub(crate) fn apply(&mut self, step: &Step, at: Instant) {
        match *step {
            Step::Wait(_) => {}
            Step::Set {
                player,
                buttons,
                sticks,
                ms,
            } => match ms {
                Some(ms) => self.timed.push(Timed {
                    player,
                    buttons,
                    sticks,
                    until: at + Duration::from_millis(ms),
                    seen: false,
                }),
                None => {
                    if let Some(h) = self.held.get_mut(player) {
                        h.buttons |= buttons;
                        for (s, v) in h.sticks.iter_mut().zip(sticks) {
                            *s = v.unwrap_or(*s);
                        }
                    }
                }
            },
            Step::Release {
                player,
                buttons,
                sticks,
            } => {
                if let Some(h) = self.held.get_mut(player) {
                    h.buttons &= !buttons;
                    for (s, clear) in h.sticks.iter_mut().zip(sticks) {
                        if clear {
                            *s = 0;
                        }
                    }
                }
                for t in self.timed.iter_mut().filter(|t| t.player == player) {
                    t.buttons &= !buttons;
                    for (s, clear) in t.sticks.iter_mut().zip(sticks) {
                        if clear {
                            *s = None;
                        }
                    }
                }
            }
        }
    }

    /// Runs the steps that are due at `now` with `frame` frames presented, and returns what each player holds.
    pub(crate) fn poll(&mut self, now: Instant, frame: u64) -> [RawKeys; PLAYERS] {
        while let Some(step) = self.steps.get(self.next).copied() {
            match step {
                Step::Wait(Wait::Ms(ms)) => {
                    let due = self.cursor + Duration::from_millis(ms);
                    if now < due {
                        break;
                    }
                    self.cursor = due;
                    self.cursor_frame = frame;
                }
                Step::Wait(Wait::Frames(n)) => {
                    let due = self.cursor_frame.saturating_add(n);
                    if frame < due {
                        break;
                    }
                    self.cursor_frame = due;
                    self.cursor = now;
                }
                Step::Wait(Wait::UntilFrame(n)) => {
                    if frame < n {
                        break;
                    }
                    self.cursor_frame = frame;
                    self.cursor = now;
                }
                other => self.apply(&other, self.cursor),
            }
            self.next += 1;
        }
        self.output(now)
    }

    /// What every player holds at `now`. A finished press leaves after one poll showed it.
    fn output(&mut self, now: Instant) -> [RawKeys; PLAYERS] {
        let mut out = self.held;
        for t in &mut self.timed {
            if (now < t.until || !t.seen)
                && let Some(o) = out.get_mut(t.player)
            {
                o.buttons |= t.buttons;
                for (s, v) in o.sticks.iter_mut().zip(t.sticks) {
                    *s = v.unwrap_or(*s);
                }
            }
            t.seen = true;
        }
        self.timed.retain(|t| now < t.until);
        out
    }
}

#[cfg(test)]
#[path = "timeline_tests.rs"]
mod tests;
