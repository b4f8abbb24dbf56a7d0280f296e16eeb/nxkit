//! When a rumble report goes to a Switch Pro Controller, a port of the pacing of `src/backend/nintendo_hd.rs` of
//! RenHDrum (<https://github.com/trzz1214/RenHDrum>, MIT OR Apache-2.0, used under MIT), without its 5 ms planner, its
//! click packets and its token bucket.
//!
//! Over USB a report goes out at most every 15 ms and carries the strongest value since the last one, so a short hit
//! between two reports is not lost. Over Bluetooth a report goes out at most every 32 ms and carries the newest value,
//! as RenHDrum's level 4 "Compatible": a radio that gets reports faster loses input reports or the connection
//! (`docs/research/03-bluetooth-congestion-and-pacing.md`). A report that still rumbles is sent again after 40 ms
//! without a change, so the controller keeps playing it and a lost report does not stop it. A report equal to the
//! last one is not sent.

use super::encoding::{encode_side, is_neutral, rumble_report};
use crate::rumble::VibrationValue;

/// How the controller is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    Usb,
    Bluetooth,
}

impl Link {
    /// The least time between two reports in microseconds.
    fn spacing(self) -> u64 {
        match self {
            Link::Usb => 15_000,
            Link::Bluetooth => 32_000,
        }
    }
}

/// A report that still rumbles is sent again after this many microseconds (RenHDrum level 4 `resend_ms`).
const KEEP_ALIVE: u64 = 40_000;

/// What the writer does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Poll {
    /// Write this report now.
    Send([u8; 10]),
    /// Nothing to write before this time in microseconds, or before a new value when `None`.
    Wait(Option<u64>),
}

/// The pacing state of one controller.
#[derive(Clone, Debug)]
pub(crate) struct Pacer {
    link: Link,
    latest: [VibrationValue; 2],
    peak: [VibrationValue; 2],
    dirty: bool,
    last: Option<[[u8; 4]; 2]>,
    last_send: Option<u64>,
    counter: u8,
}

/// The stronger of two values for each band, with its frequency.
fn peak(acc: &mut VibrationValue, v: &VibrationValue) {
    if v.amp_low > acc.amp_low {
        acc.amp_low = v.amp_low;
        acc.freq_low = v.freq_low;
    }
    if v.amp_high > acc.amp_high {
        acc.amp_high = v.amp_high;
        acc.freq_high = v.freq_high;
    }
}

impl Pacer {
    pub(crate) fn new(link: Link) -> Pacer {
        Pacer {
            link,
            latest: [VibrationValue::IDLE; 2],
            peak: [VibrationValue::IDLE; 2],
            dirty: false,
            last: None,
            last_send: None,
            counter: 0,
        }
    }

    /// Takes the values of the left and the right side.
    pub(crate) fn push(&mut self, sides: [VibrationValue; 2]) {
        for (p, v) in self.peak.iter_mut().zip(&sides) {
            peak(p, v);
        }
        self.latest = sides;
        self.dirty = true;
    }

    /// The next counter of a report, 0 to 15. Every report to the controller takes one.
    pub(crate) fn next_counter(&mut self) -> u8 {
        let c = self.counter;
        self.counter = (c + 1) & 0x0F;
        c
    }

    fn rumbling(&self) -> bool {
        self.last.is_some_and(|l| !l.iter().all(is_neutral))
    }

    fn send(&mut self, now: u64, sides: [[u8; 4]; 2]) -> Poll {
        self.last = Some(sides);
        self.last_send = Some(now);
        let counter = self.next_counter();
        Poll::Send(rumble_report(counter, &sides))
    }

    /// The time of the next report while nothing new comes.
    fn next_due(&self) -> Option<u64> {
        let since = self.last_send?;
        if self.dirty {
            Some(since + self.link.spacing())
        } else if self.rumbling() {
            Some(since + KEEP_ALIVE)
        } else {
            None
        }
    }

    /// What to do at `now`, in microseconds of a steady clock.
    pub(crate) fn poll(&mut self, now: u64) -> Poll {
        let since = self.last_send.map(|t| now.saturating_sub(t));
        if since.is_some_and(|s| s < self.link.spacing()) {
            return Poll::Wait(self.next_due());
        }
        if self.dirty {
            let values = match self.link {
                Link::Usb => self.peak,
                Link::Bluetooth => self.latest,
            };
            // A peak that was stronger than the newest value is followed by the newest value.
            self.dirty = values != self.latest;
            self.peak = if self.dirty {
                self.latest
            } else {
                [VibrationValue::IDLE; 2]
            };
            let sides = [encode_side(&values[0]), encode_side(&values[1])];
            if self.last != Some(sides) {
                return self.send(now, sides);
            }
        }
        if let Some(sides) = self.last
            && self.rumbling()
            && since.is_some_and(|s| s >= KEEP_ALIVE)
        {
            return self.send(now, sides);
        }
        Poll::Wait(self.next_due())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hd::encoding::NEUTRAL;

    const MS: u64 = 1000;

    fn v(amp: f32) -> VibrationValue {
        VibrationValue {
            amp_low: amp,
            ..VibrationValue::IDLE
        }
    }

    fn sent(p: Poll) -> Option<[u8; 10]> {
        match p {
            Poll::Send(r) => Some(r),
            Poll::Wait(_) => None,
        }
    }

    #[test]
    fn a_value_goes_out_at_once_then_waits_for_the_spacing() {
        let mut p = Pacer::new(Link::Usb);
        p.push([v(0.5), VibrationValue::IDLE]);
        let r = sent(p.poll(0)).unwrap();
        assert_eq!((r[0], r[1]), (0x10, 0));
        assert_eq!(&r[2..6], &encode_side(&v(0.5)));
        assert_eq!(&r[6..10], &NEUTRAL);
        p.push([v(0.6), VibrationValue::IDLE]);
        assert_eq!(p.poll(10 * MS), Poll::Wait(Some(15 * MS)));
        let r = sent(p.poll(15 * MS)).unwrap();
        assert_eq!((r[1], &r[2..6]), (1, &encode_side(&v(0.6))[..]));
    }

    #[test]
    fn usb_keeps_the_peak_and_bluetooth_the_newest() {
        for (link, first) in [(Link::Usb, 0.9), (Link::Bluetooth, 0.2)] {
            let mut p = Pacer::new(link);
            p.push([v(0.1), v(0.1)]);
            p.poll(0);
            p.push([v(0.9), v(0.1)]);
            p.push([v(0.2), v(0.1)]);
            let r = sent(p.poll(40 * MS)).unwrap();
            assert_eq!(&r[2..6], &encode_side(&v(first)), "{link:?}");
            if link == Link::Usb {
                let r = sent(p.poll(55 * MS)).unwrap();
                assert_eq!(
                    &r[2..6],
                    &encode_side(&v(0.2)),
                    "the newest value follows the peak"
                );
            }
        }
    }

    #[test]
    fn bluetooth_waits_32_ms() {
        let mut p = Pacer::new(Link::Bluetooth);
        p.push([v(0.3), v(0.3)]);
        p.poll(0);
        p.push([v(0.6), v(0.6)]);
        assert_eq!(p.poll(31 * MS), Poll::Wait(Some(32 * MS)));
        assert!(sent(p.poll(32 * MS)).is_some());
    }

    #[test]
    fn a_rumble_is_sent_again_after_40_ms_and_silence_is_not() {
        let mut p = Pacer::new(Link::Usb);
        p.push([v(0.4), v(0.0)]);
        let first = sent(p.poll(0)).unwrap();
        assert_eq!(p.poll(20 * MS), Poll::Wait(Some(40 * MS)));
        let again = sent(p.poll(40 * MS)).unwrap();
        assert_eq!((&again[2..], again[1]), (&first[2..], 1));
        p.push([VibrationValue::IDLE; 2]);
        let stop = sent(p.poll(55 * MS)).unwrap();
        assert_eq!((&stop[2..6], &stop[6..10]), (&NEUTRAL[..], &NEUTRAL[..]));
        assert_eq!(p.poll(200 * MS), Poll::Wait(None));
    }

    #[test]
    fn an_unchanged_value_is_not_sent_again_before_the_keep_alive() {
        let mut p = Pacer::new(Link::Usb);
        p.push([v(0.4), v(0.0)]);
        p.poll(0);
        p.push([v(0.4), v(0.0)]);
        assert_eq!(p.poll(20 * MS), Poll::Wait(Some(40 * MS)));
    }

    #[test]
    fn the_counter_wraps_at_16() {
        let mut p = Pacer::new(Link::Usb);
        let counters: Vec<u8> = (0..18).map(|_| p.next_counter()).collect();
        assert_eq!((counters[15], counters[16], counters[17]), (15, 0, 1));
    }
}
