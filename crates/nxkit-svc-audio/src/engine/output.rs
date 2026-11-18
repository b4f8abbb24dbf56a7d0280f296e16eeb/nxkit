//! Desktop output selection and the clocked silent sink.
use super::{Mixer, mixer};
use crate::{host, sys::au};
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) enum OutState {
    #[default]
    Unstarted,
    Device(au::Output),
    Silent,
}

impl OutState {
    pub(super) fn join_status(&self) -> Option<i32> {
        match self {
            Self::Device(device) => device.join_status(),
            _ => None,
        }
    }
}

impl Mixer {
    pub(super) fn ensure_output(&self) {
        let Ok(mut out) = self.out.lock() else { return };
        if !matches!(*out, OutState::Unstarted) {
            return;
        }
        if self.force_silent.load(Ordering::Relaxed) {
            eprintln!(
                "nxkit-audio: the device is off by configuration, a silent sink paces the outputs"
            );
            self.spawn_silent();
            *out = OutState::Silent;
            return;
        }
        let m = Arc::clone(mixer());
        match au::Output::start(Box::new(move |buf| m.render(buf))) {
            Ok(o) => {
                eprintln!("nxkit-audio: the output unit runs at 48000 Hz stereo");
                *out = OutState::Device(o);
            }
            Err(e) => {
                eprintln!(
                    "nxkit-audio: warning: the output unit did not start ({e}), a silent sink paces the outputs"
                );
                self.spawn_silent();
                *out = OutState::Silent;
            }
        }
    }

    fn spawn_silent(&self) {
        let m = Arc::clone(mixer());
        let r = std::thread::Builder::new()
            .name("nxkit-audio-silent".into())
            .spawn::<_, ()>(move || {
                let mut buf = vec![0.0f32; 480 * 2];
                let step = Duration::from_millis(10);
                let mut next = Instant::now() + step;
                loop {
                    m.render(&mut buf);
                    let now = Instant::now();
                    if next > now {
                        std::thread::sleep(next - now);
                    }
                    next += step;
                }
            });
        if let Err(e) = r {
            host::fatal(&format!("the silent sink thread did not start: {e}"));
        }
    }
}
