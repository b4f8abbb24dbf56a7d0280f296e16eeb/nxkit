//! The settings of the pacing service from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The crate has live keys only (W35). The host's board fills them from its snapshot at the start and
//! changes them while the game runs ([`LIVE`]). The code of the crate reads them through [`live`].

use nxkit_abi::env::Env;
use nxkit_abi::live::{Live, LiveKey, Section};

/// The live settings of the pacing service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PacingLive {
    /// `NXKIT_PACE_DIAG=1`: the cost and step lines of the load-driven frame step.
    pub pace_diag: bool,
}

impl PacingLive {
    /// The settings from a snapshot.
    pub fn from_env(env: &Env) -> Self {
        PacingLive {
            pace_diag: env.var("NXKIT_PACE_DIAG").is_ok_and(|v| v == "1"),
        }
    }
}

// Process-wide (W33 (2)): the frame step stubs of the game call the present path with no context.
static LIVE_CELL: Live<PacingLive> = Live::new();

/// The live settings now (the defaults in a unit test).
pub fn live() -> &'static PacingLive {
    LIVE_CELL.get()
}

/// The live keys of the crate, for the board of the host.
pub const LIVE: Section = Section {
    owner: "nxkit-svc-pacing",
    keys: &[LiveKey {
        name: "NXKIT_PACE_DIAG",
        help: "1: the cost and step lines of the frame step",
    }],
    apply: |env| LIVE_CELL.set(PacingLive::from_env(env)),
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        assert!(!PacingLive::from_env(&Env::default()).pace_diag);
        assert!(PacingLive::from_env(&Env::from_pairs([("NXKIT_PACE_DIAG", "1")])).pace_diag);
        assert!(!PacingLive::from_env(&Env::from_pairs([("NXKIT_PACE_DIAG", "2")])).pace_diag);
    }
}
