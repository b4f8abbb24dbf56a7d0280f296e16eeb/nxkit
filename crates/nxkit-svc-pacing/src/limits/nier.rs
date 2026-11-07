//! NieR:Automata: the frame interval and the dynamic resolution.
//!
//! The 1.0.3 builds expose a preset minimum for the scale but ship fixed fallbacks for each mode. The host selects the
//! verified lower bound and leaves the game's controller of the scale and the texture coordinates alone. The choice of the
//! resolution is separate from the frame rate: 30 or 60 fps does not turn the controller on or off.
//!
//! The addresses are the 1.0.3 builds that the rewrite cuts. The Asian release (`0100B8E016F76000`) shifts the function by
//! 0x13c0, and its preset minimum is the target of the relocation of the slot that the function loads it through
//! (main+0x12c8500). The display object holds a pointer to the NVN device at its first word. The present interval is at
//! +0x9c160 of the device (1 is 60 fps, 2 is 30 fps), and the frame interval mode is at +0x504 of the display object (0 is
//! 60 fps, 1 is 30 fps), which the setter of the game writes.
//!
//! Source: `k_nier_dr`, `dynamic_resolution_tick` and the NieR part of `dm_fps_lock_tick` in `dm/host/hle_fpslock.c` of
//! nxnative, read on 2026-11-07 at `88c7e03f`.

use nxkit_abi::guest::{self, Fault};

use crate::notice::Notice;

#[derive(Debug)]
struct Build {
    id: &'static str,
    clamp: u64,
    publish: u64,
    floor0: u64,
    floor1: u64,
    minimum: u64,
    display: u64,
}

const BUILDS: [Build; 2] = [
    Build {
        id: "3c10ec35c5ef4c3686b4053afb0e58e7",
        clamp: 0x9bf568,
        publish: 0x9bf578,
        floor0: 0x9bf528,
        floor1: 0x9bf534,
        minimum: 0x128e068,
        display: 0x192be00,
    },
    Build {
        id: "98397a10419894652f372d4dc1b6fc2c",
        clamp: 0x9c0928,
        publish: 0x9c0938,
        floor0: 0x9c08e8,
        floor1: 0x9c08f4,
        minimum: 0x128f130,
        display: 0x192d600,
    },
];

/// The instruction words that the rewrite leaves at the scale clamp, the publish branch and the two floors.
const CLAMP: u32 = 0x1e25_4c80;
const PUBLISH: u32 = 0x5400_0489;
const FLOOR: u64 = 0x72a7_f00c_5280_000c;
/// The preset minimum of the scale: 0.7 for the dynamic resolution, else 1.0.
const MINIMUM_DYNAMIC: u32 = 0x3f33_3333;
const MINIMUM_FULL: u32 = 0x3f80_0000;
const LOG_LIMIT: u32 = 8;

/// NieR's state in a main module of a known build.
#[derive(Clone, Debug)]
pub struct Nier {
    build: &'static Build,
    dynamic: bool,
    logged: u32,
}

impl Nier {
    /// The state for a build id, `None` for a build that is not NieR 1.0.3. `dynamic_resolution` is the profile's choice, with
    /// its notice. A choice that the module's code does not verify keeps the full resolution.
    pub fn new(build_id: &str, base: u64, dynamic_resolution: bool) -> Option<(Self, Vec<Notice>)> {
        let build = BUILDS.iter().find(|b| b.id == build_id)?;
        let mut notices = Vec::new();
        let mut dynamic = dynamic_resolution;
        if dynamic {
            let ok = guest::read_u32(base + build.clamp) == Ok(CLAMP)
                && guest::read_u32(base + build.publish) == Ok(PUBLISH)
                && guest::read_u64(base + build.floor0) == Ok(FLOOR)
                && guest::read_u64(base + build.floor1) == Ok(FLOOR);
            if !ok {
                notices.push(Notice::red(
                    "dynamic resolution needs a verified native scale and UV rewrite, so the full resolution stays",
                ));
                dynamic = false;
            }
        }
        notices.push(Notice::info(if dynamic {
            "main: NieR dynamic resolution on (native controller, 70 to 100 percent scale)"
        } else {
            "main: NieR dynamic resolution off (full resolution)"
        }));
        Some((
            Self {
                build,
                dynamic,
                logged: 0,
            },
            notices,
        ))
    }

    /// Whether the dynamic resolution stays on after the checks.
    pub fn dynamic(&self) -> bool {
        self.dynamic
    }

    /// Writes the lower bound of the scale. It runs every frame, whatever the target is.
    pub fn resolution_tick(&self, base: u64) -> Result<(), Fault> {
        let a = base + self.build.minimum;
        let want = if self.dynamic {
            MINIMUM_DYNAMIC
        } else {
            MINIMUM_FULL
        };
        let Ok(cur) = guest::read_u32(a) else {
            return Ok(());
        };
        if cur != want {
            guest::write_u32(a, want)?;
        }
        if !self.dynamic && guest::read_u32(a + 4) == Ok(MINIMUM_DYNAMIC) {
            guest::write_u32(a + 4, MINIMUM_FULL)?;
        }
        Ok(())
    }

    /// Writes the frame interval for the target. Returns the change of the present interval, `(before, after)`, that is
    /// worth a line in the log.
    pub fn interval_tick(&mut self, base: u64, target: u32) -> Result<Option<(u32, u32)>, Fault> {
        let want = if target > 60 { 1 } else { 60 / target.max(1) };
        let mode = base + self.build.display + 0x504;
        if let Ok(cur) = guest::read_u32(mode)
            && cur != want - 1
            && cur <= 1
        {
            guest::write_u32(mode, want - 1)?;
        }
        let Ok(device) = guest::read_u64(base + self.build.display) else {
            return Ok(None);
        };
        if device == 0 {
            return Ok(None);
        }
        let a = device + 0x9c160;
        let Ok(cur) = guest::read_u32(a) else {
            return Ok(None);
        };
        if cur != want && (cur == 1 || cur == 2) {
            guest::write_u32(a, want)?;
            if self.logged < LOG_LIMIT {
                self.logged += 1;
                return Ok(Some((cur, want)));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "3c10ec35c5ef4c3686b4053afb0e58e7";

    #[test]
    fn only_the_two_known_builds_are_nier() {
        assert!(Nier::new("00000000000000000000000000000000", 0x10000, false).is_none());
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(Nier::new(ID, 0x10000, false).is_some());
        assert!(Nier::new("98397a10419894652f372d4dc1b6fc2c", 0x10000, false).is_some());
    }

    #[test]
    fn a_dynamic_resolution_that_the_code_does_not_confirm_is_refused() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = vec![0u64; 0x40];
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let r = Nier::new(ID, base, true);
        assert!(
            r.as_ref()
                .is_some_and(|(n, c)| !n.dynamic() && c.iter().any(|n| n.red))
        );
    }

    /// A module big enough for the addresses of a build, so that the reads of the guest stay in range.
    fn big(len: usize) -> Vec<u64> {
        vec![0u64; len / 8]
    }

    #[test]
    fn the_minimum_scale_is_full_unless_the_dynamic_resolution_is_confirmed() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = big(0x200_0000);
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let b = &BUILDS[0];
        let Some((n, _)) = Nier::new(ID, base, false) else {
            return;
        };
        assert!(guest::write_u32(base + b.minimum, 0x3f00_0000).is_ok());
        assert!(guest::write_u32(base + b.minimum + 4, MINIMUM_DYNAMIC).is_ok());
        assert!(n.resolution_tick(base).is_ok());
        assert_eq!(guest::read_u32(base + b.minimum).ok(), Some(MINIMUM_FULL));
        assert_eq!(
            guest::read_u32(base + b.minimum + 4).ok(),
            Some(MINIMUM_FULL)
        );
        assert!(guest::write_u32(base + b.clamp, CLAMP).is_ok());
        assert!(guest::write_u32(base + b.publish, PUBLISH).is_ok());
        assert!(guest::write_u64(base + b.floor0, FLOOR).is_ok());
        assert!(guest::write_u64(base + b.floor1, FLOOR).is_ok());
        let Some((d, notes)) = Nier::new(ID, base, true) else {
            return;
        };
        assert!(d.dynamic() && notes.iter().all(|n| !n.red));
        assert!(d.resolution_tick(base).is_ok());
        assert_eq!(
            guest::read_u32(base + b.minimum).ok(),
            Some(MINIMUM_DYNAMIC)
        );
    }

    #[test]
    fn the_interval_follows_the_target_and_only_from_one_or_two() {
        let _g = crate::GUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut mem = big(0x200_0000);
        let base = mem.as_mut_ptr() as u64;
        guest::set_range(base, (mem.len() * 8) as u64);
        let b = &BUILDS[0];
        let Some((mut n, _)) = Nier::new(ID, base, false) else {
            return;
        };
        let device = base + 0x1000;
        assert!(guest::write_u64(base + b.display, device).is_ok());
        assert!(guest::write_u32(device + 0x9c160, 2).is_ok());
        assert!(guest::write_u32(base + b.display + 0x504, 1).is_ok());
        assert_eq!(n.interval_tick(base, 60).ok().flatten(), Some((2, 1)));
        assert_eq!(guest::read_u32(device + 0x9c160).ok(), Some(1));
        assert_eq!(guest::read_u32(base + b.display + 0x504).ok(), Some(0));
        assert_eq!(n.interval_tick(base, 60).ok().flatten(), None);
        assert!(guest::write_u32(device + 0x9c160, 7).is_ok());
        assert_eq!(
            n.interval_tick(base, 60).ok().flatten(),
            None,
            "7 is not an interval"
        );
        assert_eq!(guest::read_u32(device + 0x9c160).ok(), Some(7));
    }
}
