//! The recorder: turns what player 1 holds, sampled at the calls of the game, into lines of the input script.
//!
//! A line is written only when the buttons or a stick change. Between two changes the recorder writes a wait. In
//! frame timing (the default) the wait counts presented frames, so a replay gives the same frame to each change
//! whatever the load or the speed of the machine. In ms timing the wait counts wall-clock milliseconds. The waits
//! are totals, not sums of rounded gaps, so no drift builds up.
//!
//! A recording in frame timing starts with `wait frame N` when frames were presented before it started. A replay
//! then starts at the same count of presented frames.

use std::time::Instant;

use crate::npad::btn;

/// How the waits of a recording count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Timing {
    Frames,
    Ms,
}

impl Timing {
    pub(crate) fn parse(word: &str) -> Option<Timing> {
        match word.to_ascii_lowercase().as_str() {
            "frames" | "frame" => Some(Timing::Frames),
            "ms" | "time" => Some(Timing::Ms),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Timing::Frames => "frames",
            Timing::Ms => "ms",
        }
    }
}

/// The buttons that a script names, in the order of the bits. The stick direction bits are not here: a stick has
/// its own line.
pub(crate) const BUTTONS: [(&str, u64); 16] = [
    ("a", btn::A),
    ("b", btn::B),
    ("x", btn::X),
    ("y", btn::Y),
    ("lstick", btn::STICK_L),
    ("rstick", btn::STICK_R),
    ("l", btn::L),
    ("r", btn::R),
    ("zl", btn::ZL),
    ("zr", btn::ZR),
    ("plus", btn::PLUS),
    ("minus", btn::MINUS),
    ("dpad_left", btn::LEFT),
    ("dpad_up", btn::UP),
    ("dpad_right", btn::RIGHT),
    ("dpad_down", btn::DOWN),
];

/// The bits of [`BUTTONS`].
pub(crate) const PHYSICAL: u64 = 0xffff;

fn names(mask: u64) -> String {
    BUTTONS
        .iter()
        .filter(|(_, b)| mask & b != 0)
        .map(|(n, _)| *n)
        .collect::<Vec<_>>()
        .join(" ")
}

fn axis(v: i32) -> String {
    format!("{:.5}", f64::from(v) / 32767.0)
}

/// The state of a recording.
pub(crate) struct Recorder {
    timing: Timing,
    start: Instant,
    start_frame: u64,
    /// Frames and milliseconds that the written waits cover.
    wrote_frames: u64,
    wrote_ms: u64,
    buttons: u64,
    sticks: [i32; 4],
}

impl Recorder {
    /// A recorder that starts at `now` with `frame` frames presented, and the first lines of the file.
    pub(crate) fn new(timing: Timing, now: Instant, frame: u64) -> (Recorder, Vec<String>) {
        let mut lines = vec![format!(
            "# nxkit input recording of player 1, timing {}, started at frame {frame}",
            timing.name()
        )];
        if timing == Timing::Frames && frame > 0 {
            lines.push(format!("wait frame {frame}"));
        }
        let r = Recorder {
            timing,
            start: now,
            start_frame: frame,
            wrote_frames: 0,
            wrote_ms: 0,
            buttons: 0,
            sticks: [0; 4],
        };
        (r, lines)
    }

    /// The wait before a change at `now` and `frame`, or none when no time has passed.
    fn wait(&mut self, now: Instant, frame: u64) -> Option<String> {
        // Frame timing falls back to ms while no frame was ever presented, so a host without a frame count records.
        if self.timing == Timing::Frames && frame > 0 {
            let total = frame.saturating_sub(self.start_frame);
            let n = total.saturating_sub(self.wrote_frames);
            self.wrote_frames = total.max(self.wrote_frames);
            return (n > 0).then(|| format!("wait frames {n}"));
        }
        let total = u64::try_from(now.saturating_duration_since(self.start).as_millis())
            .unwrap_or(u64::MAX);
        let n = total.saturating_sub(self.wrote_ms);
        self.wrote_ms = total.max(self.wrote_ms);
        (n > 0).then(|| format!("wait {n}"))
    }

    /// Looks at the state of player 1. Returns the lines for a change. A change of the stick is written for the stick
    /// whose x or y changed.
    pub(crate) fn observe(
        &mut self,
        buttons: u64,
        sticks: [i32; 4],
        now: Instant,
        frame: u64,
    ) -> Vec<String> {
        let buttons = buttons & PHYSICAL;
        if buttons == self.buttons && sticks == self.sticks {
            return Vec::new();
        }
        let mut lines: Vec<String> = self.wait(now, frame).into_iter().collect();
        let released = self.buttons & !buttons;
        let pressed = buttons & !self.buttons;
        if released != 0 {
            lines.push(format!("release {}", names(released)));
        }
        if pressed != 0 {
            lines.push(format!("hold {}", names(pressed)));
        }
        for (side, at) in [("l", 0), ("r", 2)] {
            if sticks[at] != self.sticks[at] || sticks[at + 1] != self.sticks[at + 1] {
                lines.push(format!(
                    "stick {side} {} {}",
                    axis(sticks[at]),
                    axis(sticks[at + 1])
                ));
            }
        }
        self.buttons = buttons;
        self.sticks = sticks;
        lines
    }

    /// The last lines, at `now` and `frame`: the wait up to the stop and a release when something is held, so a replay
    /// keeps the last hold as long as it lasted and ends with the controller at rest.
    pub(crate) fn finish(&mut self, now: Instant, frame: u64) -> Vec<String> {
        if self.buttons == 0 && self.sticks == [0; 4] {
            return Vec::new();
        }
        let mut lines: Vec<String> = self.wait(now, frame).into_iter().collect();
        lines.push("release".to_owned());
        lines
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod tests;
