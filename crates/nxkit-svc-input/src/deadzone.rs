//! The radial dead zone of the sticks of a controller, a port of the engine `stick_deadzone.h` (MIT).
//!
//! Inside the dead zone a stick reads centred. Outside it the rest of the travel is spread over the full range, so a
//! stick pushed all the way still reads full. A diagonal of a square range beyond a full radius is left alone. The
//! service applies it to the readings of the controllers only, not to the sticks of the keyboard and the mouse.

use crate::npad::RawPad;

/// The largest dead zone in percent of the travel.
const MAX_PERCENT: u8 = 50;

/// A stick at `(x, y)`, each -1 to 1, with a dead zone of `percent` (0 for none, at most 50).
fn apply(x: f32, y: f32, percent: u8) -> (f32, f32) {
    if percent == 0 {
        return (x, y);
    }
    let dz = f32::from(percent.min(MAX_PERCENT)) / 100.0;
    let radius = x.hypot(y);
    if radius <= dz {
        return (0.0, 0.0);
    }
    if radius >= 1.0 {
        return (x, y);
    }
    let scale = (radius - dz) / ((1.0 - dz) * radius);
    ((x * scale).clamp(-1.0, 1.0), (y * scale).clamp(-1.0, 1.0))
}

/// The dead zones of the left and the right stick in percent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Deadzones {
    left: u8,
    right: u8,
}

impl Deadzones {
    pub(crate) fn new(left: u8, right: u8) -> Self {
        Deadzones { left, right }
    }

    /// The reading `pad` with the dead zone of each stick applied.
    pub(crate) fn apply(&self, pad: RawPad) -> RawPad {
        let [lx, ly, rx, ry] = pad.axes;
        let (lx, ly) = apply(lx, ly, self.left);
        let (rx, ry) = apply(rx, ry, self.right);
        RawPad {
            axes: [lx, ly, rx, ry],
            ..pad
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn no_dead_zone_leaves_the_stick() {
        assert_eq!(apply(0.03, -0.13, 0), (0.03, -0.13));
    }

    #[test]
    fn inside_reads_centred() {
        assert_eq!(apply(0.1, 0.1, 20), (0.0, 0.0));
    }

    #[test]
    fn outside_is_rescaled_and_radial() {
        let (x, y) = apply(0.5, 0.0, 20);
        assert!(near(x, 0.375) && y == 0.0, "(0.5 - 0.2) / 0.8");
        let (x, y) = apply(0.65, 0.0, 30);
        assert!(near(x, 0.5) && y == 0.0, "(0.65 - 0.3) / 0.7");
        let (dx, dy) = apply(0.6, 0.6, 20);
        assert!(dx == dy && dx > 0.0, "a diagonal keeps its angle");
    }

    #[test]
    fn full_travel_stays_full() {
        assert_eq!(apply(1.0, 0.0, 50), (1.0, 0.0));
        assert_eq!(
            apply(1.0, 1.0, 30),
            (1.0, 1.0),
            "a square corner is not turned"
        );
        assert_eq!(apply(-1.0, 0.0, 10), (-1.0, 0.0));
    }

    #[test]
    fn the_output_grows_with_the_stick() {
        let mut last = -1.0;
        for i in 0..=100 {
            let (x, _) = apply(i as f32 / 100.0, 0.0, 35);
            assert!(x >= last);
            last = x;
        }
    }

    #[test]
    fn each_stick_has_its_own_dead_zone() {
        let pad = RawPad {
            buttons: 1,
            axes: [0.25, 0.0, 0.25, 0.0],
        };
        let out = Deadzones::new(30, 20).apply(pad);
        assert_eq!(out.buttons, 1);
        assert_eq!(out.axes[0], 0.0, "inside 30 %");
        assert!(near(out.axes[2], 0.0625), "(0.25 - 0.2) / 0.8");
    }
}
