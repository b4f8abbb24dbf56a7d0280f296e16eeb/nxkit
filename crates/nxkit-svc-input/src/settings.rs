//! The settings of the input service from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The host builds them once from its snapshot ([`InputSettings::from_env`]) and installs them ([`install`])
//! before it builds any part. The code of the crate reads them through [`settings`].

use nxkit_abi::env::{Env, Slot, SlotError};

use std::path::PathBuf;

/// The settings of the input service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InputSettings {
    /// `NXKIT_INPUT_FILE` (not empty): the file of scripted input for player 1.
    pub script_file: Option<PathBuf>,
}

impl InputSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        InputSettings {
            script_file: env
                .var_os("NXKIT_INPUT_FILE")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from),
        }
    }
}

// Process-wide (W26): the `nn::hid` take-overs, which the game calls with no context, read the script file. The host installs them once at start.
static SETTINGS: Slot<InputSettings> = Slot::new("nxkit-svc-input");

/// Installs the settings. The host calls it once, before it builds any part.
pub fn install(settings: InputSettings) -> Result<(), SlotError> {
    SETTINGS.install(settings)
}

/// The installed settings (the defaults in a unit test).
pub fn settings() -> &'static InputSettings {
    SETTINGS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        assert_eq!(InputSettings::from_env(&Env::default()).script_file, None);
        let s = InputSettings::from_env(&Env::from_pairs([("NXKIT_INPUT_FILE", "/x/in.txt")]));
        assert_eq!(s.script_file, Some(PathBuf::from("/x/in.txt")));
        assert_eq!(
            InputSettings::from_env(&Env::from_pairs([("NXKIT_INPUT_FILE", "")])).script_file,
            None
        );
    }
}
