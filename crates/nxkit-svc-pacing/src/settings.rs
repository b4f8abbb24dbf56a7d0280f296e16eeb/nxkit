//! The settings of the pacing service from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The host builds them once from its snapshot ([`PacingSettings::from_env`]) and installs them ([`install`])
//! before it builds any part. The code of the crate reads them through [`settings`].

use nxkit_abi::env::{Env, Slot, SlotError};

/// The settings of the pacing service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PacingSettings {
    /// `NXKIT_PACE_DIAG=1`: the cost and step lines of the load-driven frame step.
    pub pace_diag: bool,
}

impl PacingSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        PacingSettings {
            pace_diag: env.var("NXKIT_PACE_DIAG").is_ok_and(|v| v == "1"),
        }
    }
}

// Process-wide (W26): the frame step stubs of the game call the present path with no context. The host installs them once at start.
static SETTINGS: Slot<PacingSettings> = Slot::new("nxkit-svc-pacing");

/// Installs the settings. The host calls it once, before it builds any part.
pub fn install(settings: PacingSettings) -> Result<(), SlotError> {
    SETTINGS.install(settings)
}

/// The installed settings (the defaults in a unit test).
pub fn settings() -> &'static PacingSettings {
    SETTINGS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        assert!(!PacingSettings::from_env(&Env::default()).pace_diag);
        assert!(PacingSettings::from_env(&Env::from_pairs([("NXKIT_PACE_DIAG", "1")])).pace_diag);
        assert!(!PacingSettings::from_env(&Env::from_pairs([("NXKIT_PACE_DIAG", "2")])).pace_diag);
    }
}
