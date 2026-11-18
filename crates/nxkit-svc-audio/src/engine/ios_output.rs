//! The iOS output may temporarily lose its audio session while the game keeps running.
#[cfg(target_os = "ios")]
use super::{Mixer, RATE, STARTED, mixer};
#[cfg(target_os = "ios")]
use crate::sys::au;
#[cfg(target_os = "ios")]
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

const SILENT_STEP: Duration = Duration::from_millis(20);

// Recover promptly from a short interruption, then probe infrequently while the game needs audio.
const FAST_ATTEMPTS: u8 = 3;
const SLOW_RETRY: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Playback {
    requested: bool,
    active: bool,
    available: bool,
    running: bool,
    stop_uncertain: bool,
    failures: u8,
    retry_at: Option<Instant>,
    next_silent: Option<Instant>,
}

impl Playback {
    fn set_demand(&mut self, requested: bool) {
        if self.requested != requested {
            self.requested = requested;
            self.reset_retry();
        }
    }

    fn reset_retry(&mut self) {
        self.failures = 0;
        self.retry_at = None;
        self.next_silent = None;
    }

    fn set_activity(&mut self, active: bool, available: bool) {
        if (self.active, self.available) != (active, available) {
            self.active = active;
            self.available = available;
            self.reset_retry();
        }
    }

    fn update_device(
        &mut self,
        now: Instant,
        force_silent: bool,
        set_running: impl FnOnce(bool) -> Result<(), String>,
    ) -> Result<(), String> {
        let wanted = self.requested && self.active && self.available && !force_silent;
        if (wanted == self.running && !self.stop_uncertain)
            || (!(self.active && self.requested) && self.failures == FAST_ATTEMPTS)
            || self.retry_at.is_some_and(|at| now < at)
        {
            return Ok(());
        }
        match set_running(wanted) {
            Ok(()) => {
                self.running = wanted;
                self.stop_uncertain = false;
                self.failures = 0;
                self.retry_at = None;
                self.next_silent = None;
                Ok(())
            }
            Err(error) => {
                // If a stop fails, keep silent rendering disabled until the unit confirms a
                // new start or stop. The system may already have stopped it during interruption.
                self.stop_uncertain |= !wanted;
                let report = self.failures == 0;
                self.failures = (self.failures + 1).min(FAST_ATTEMPTS);
                let delay = if self.failures == FAST_ATTEMPTS {
                    SLOW_RETRY
                } else {
                    RETRY_DELAY
                };
                self.retry_at = Some(now + delay);
                if report { Err(error) } else { Ok(()) }
            }
        }
    }

    // A failed stop leaves the hardware as sole mixer consumer until it has actually stopped.
    fn silent_due(&mut self, now: Instant) -> bool {
        if !self.requested || !self.active || self.running {
            return false;
        }
        let next = self.next_silent.get_or_insert(now);
        if now < *next {
            return false;
        }
        *next += SILENT_STEP;
        // Never replay a backlog of buffers after the process or device was suspended.
        if *next <= now {
            *next = now + SILENT_STEP;
        }
        true
    }
}

#[cfg(target_os = "ios")]
#[derive(Default)]
pub(super) struct OutState {
    playback: Playback,
    device: Option<au::Output>,
}

#[cfg(target_os = "ios")]
impl OutState {
    pub(super) fn join_status(&self) -> Option<i32> {
        self.device.as_ref()?.join_status()
    }

    fn update_device(&mut self, now: Instant, force_silent: bool) -> Result<(), String> {
        let device = &mut self.device;
        self.playback.update_device(now, force_silent, |running| {
            if device.is_none() && running {
                let m = Arc::clone(mixer());
                *device = Some(au::Output::start(Box::new(move |buf| m.render(buf)))?);
            }
            if let Some(unit) = device {
                unit.set_running(running)?;
            }
            Ok(())
        })
    }
}

#[cfg(target_os = "ios")]
impl Mixer {
    pub(super) fn ensure_output(&self) {
        self.sem.signal();
    }

    pub(crate) fn set_activity(&self, active: bool, available: bool) -> Result<(), String> {
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        out.playback.set_activity(active, available);
        self.sem.signal();
        // Stop before UIKit deactivates its audio session. Opening and retrying run on the notifier.
        if !active || !available {
            out.update_device(Instant::now(), self.force_silent.load(Ordering::Relaxed))?;
        }
        Ok(())
    }

    pub(super) fn poll_output(&self) {
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = self
            .slots
            .iter()
            .any(|s| s.open.load(Ordering::Acquire) && s.state.load(Ordering::Acquire) == STARTED);
        out.playback.set_demand(requested);
        let now = Instant::now();
        if let Err(e) = out.update_device(now, self.force_silent.load(Ordering::Relaxed)) {
            eprintln!("nxkit-audio: RemoteIO: {e}");
        }
        if out.playback.silent_due(now) {
            // The mutex excludes every hardware start until silent rendering is finished.
            const SAMPLES: usize = RATE as usize * SILENT_STEP.as_millis() as usize * 2 / 1000;
            self.render(&mut [0.0; SAMPLES]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_retries_back_off_and_recover_without_changing_scenes() {
        let mut playback = Playback::default();
        playback.set_demand(true);
        let now = Instant::now();
        playback.set_activity(true, true);
        let mut calls = 0;
        let mut reports = 0;
        for n in 0..60 {
            let at = now + RETRY_DELAY * n;
            let attempt = playback.update_device(at, false, |_| {
                calls += 1;
                Err("busy".into())
            });
            reports += u8::from(attempt.is_err());
            assert!(playback.silent_due(at));
            assert!(!playback.silent_due(at));
        }
        assert_eq!((calls, reports), (FAST_ATTEMPTS, 1));
        playback.set_activity(true, true);
        let due = now + RETRY_DELAY * 2 + SLOW_RETRY;
        assert!(
            playback
                .update_device(due, false, |on| {
                    calls += 1;
                    assert!(on);
                    Ok(())
                })
                .is_ok()
        );
        assert_eq!(calls, FAST_ATTEMPTS + 1);
        assert!(!playback.silent_due(due));
    }

    #[test]
    fn a_failed_stop_never_hands_the_same_buffers_to_two_consumers() {
        let mut playback = Playback::default();
        playback.set_demand(true);
        let now = Instant::now();
        playback.set_activity(true, true);
        assert!(playback.update_device(now, false, |_| Ok(())).is_ok());
        playback.set_activity(true, false);
        assert!(
            playback
                .update_device(now, false, |_| Err("stop failed".into()))
                .is_err()
        );
        assert!(!playback.silent_due(now));
        playback.set_activity(false, false);
        assert!(!playback.silent_due(now));
        // If UIKit stopped RemoteIO after the error, the next activation must start it again.
        playback.set_activity(true, true);
        let mut restarted = false;
        assert!(
            playback
                .update_device(now, false, |on| {
                    restarted = on;
                    Ok(())
                })
                .is_ok()
        );
        assert!(restarted);
        assert!(!playback.silent_due(now));
        playback.set_activity(true, false);
        assert!(
            playback
                .update_device(now, false, |on| {
                    assert!(!on);
                    Ok(())
                })
                .is_ok()
        );
        assert!(playback.silent_due(now));
    }
}
