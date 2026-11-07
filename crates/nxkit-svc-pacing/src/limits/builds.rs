//! The frame limits that are known for a build of a game.
//!
//! Each entry names the build by its build id and says where the engine keeps its limit. Every entry is a place that
//! nxnative found by reading the game. The comment of each entry has what is known, and the evidence for all of them is
//! `dm/host/hle_fpslock.c` of nxnative at `88c7e03f`, read on 2026-11-07. The entries for Monster Hunter Rise and Need for
//! Speed come from the configs of masagrator's FPSLocker-Warehouse. None of it was checked on nxkit, because no module of
//! these games is a title module here yet.

use super::rows::{Kind, Row};

struct Build {
    id: &'static str,
    rows: &'static [Spec],
}

struct Spec {
    what: &'static str,
    ptr_off: u64,
    field: u64,
    kind: Kind,
    copies: u32,
}

const fn spec(what: &'static str, ptr_off: u64, field: u64, kind: Kind, copies: u32) -> Spec {
    Spec {
        what,
        ptr_off,
        field,
        kind,
        copies,
    }
}

const BUILDS: &[Build] = &[
    // Pokemon Brilliant Diamond 1.3.0: Unity's `Application.targetFrameRate`, an int32 at main+0x4afc868. The game sets 30 when
    // it enters the field.
    Build {
        id: "94ceae325c205c4b9d6f7235552f28fd",
        rows: &[spec("Unity targetFrameRate", 0, 0x4afc868, Kind::FpsI32, 1)],
    },
    // Monster Hunter Rise 16.0.2: RE Engine's frame limit, a float at [main+0x1295b3f0]+0x434. The frame loop sleeps until
    // [+0x430] / [+0x434] sixtieths of a second have passed and steps the game by the time that really passed.
    Build {
        id: "c9a3dd7702075ecd90101f4555389582",
        rows: &[spec(
            "RE Engine frame limit",
            0x1295b3f0,
            0x434,
            Kind::FpsF32,
            1,
        )],
    },
    // Luigi's Mansion 3 1.4.0: Next Level Games' frame limiter, a float of milliseconds at main+0x13c94dc.
    Build {
        id: "79e5950ffa85acf63e28c9aec051eac2",
        rows: &[spec(
            "NLG frame limiter target",
            0,
            0x13c94dc,
            Kind::MsF32,
            1,
        )],
    },
    // Need for Speed: Hot Pursuit Remastered 1.0.3: Criterion's frame timing. A uint32 at main+0xf0043c selects the 60 fps
    // mode. In it the frame time comes from the timer object at [main+0xed96a8] (+0x30 the frame in ticks of 19.2 MHz, +0x28 a
    // tenth of it). The game's own step is a float of seconds at [main+0xeffc60] +0x30 (2D) and +0x50 (3D).
    Build {
        id: "799d1061182c1302e08e706b84ce76e5",
        rows: &[
            spec("Criterion 60 fps mode", 0, 0xf0043c, Kind::ModeU32, 1),
            spec(
                "Criterion frame ticks",
                0xed96a8,
                0x30,
                Kind::TicksU32 { scale: 19_200_000 },
                1,
            ),
            spec(
                "Criterion frame ticks / 10",
                0xed96a8,
                0x28,
                Kind::TicksU32 { scale: 1_920_000 },
                1,
            ),
            spec("Criterion 2D game step", 0xeffc60, 0x30, Kind::SecsF32, 1),
            spec("Criterion 3D game step", 0xeffc60, 0x50, Kind::SecsF32, 1),
        ],
    },
    // EA SPORTS FC 27 1.8d.310c: the game's frame regulator, an object at [main+0xda20e38]. +0x8 is the rate it waits for and
    // +0x10 the rate it reports, both doubles. The game sets both to 30.0 whenever GameRenderSettings.Enable is 1.
    Build {
        id: "a48c0de3bc353efa0e4d359bde05ef24",
        rows: &[spec(
            "FIFA frame regulator",
            0xda20e38,
            0x8,
            Kind::FpsF64,
            2,
        )],
    },
];

/// The rows of a build, none for a build without known limits.
pub fn rows_for(build_id: &str) -> Vec<Row> {
    BUILDS
        .iter()
        .filter(|b| b.id == build_id)
        .flat_map(|b| b.rows)
        .map(|s| Row::new(s.what, s.ptr_off, s.field, s.kind, s.copies))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_build_has_its_rows_and_an_unknown_one_none() {
        assert_eq!(rows_for("799d1061182c1302e08e706b84ce76e5").len(), 5);
        assert_eq!(rows_for("a48c0de3bc353efa0e4d359bde05ef24")[0].copies, 2);
        assert!(rows_for("00000000000000000000000000000000").is_empty());
    }

    #[test]
    fn no_build_id_repeats() {
        for (i, a) in BUILDS.iter().enumerate() {
            assert!(BUILDS[i + 1..].iter().all(|b| b.id != a.id));
        }
    }
}
