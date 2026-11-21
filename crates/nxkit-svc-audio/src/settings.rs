//! The settings of the audio service from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The host builds them once from its snapshot ([`AudioSettings::from_env`]) and installs them ([`install`])
//! before it builds any part. The code of the crate reads them through [`settings`].

use nxkit_abi::env::{Env, Slot, SlotError};
use nxkit_abi::launch;

/// The settings of the audio service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioSettings {
    /// `NXKIT_AUDIO_CLICKS=1`: the renderer reports the clicks of its output blocks.
    pub clicks: bool,
    /// `NXKIT_AUDIO_DEVICE_FRAMES`: the buffer size of the output device in frames, `None` keeps the device's own.
    pub device_frames: Option<u32>,
    /// `NXKIT_SPATIAL_AUDIO=1`: the outputs carry six channels (5.1) and the device plays them as spatial audio.
    pub spatial: bool,
    /// `NXKIT_HEAD_TRACKING=1`: spatial audio on AirPods follows the head.
    pub head_tracking: bool,
}

impl AudioSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        AudioSettings {
            clicks: env.var("NXKIT_AUDIO_CLICKS").is_ok_and(|v| v == "1"),
            device_frames: env
                .var("NXKIT_AUDIO_DEVICE_FRAMES")
                .ok()
                .and_then(|v| v.parse::<u32>().ok()),
            spatial: env
                .var(launch::SPATIAL_AUDIO)
                .is_ok_and(|v| v.trim() == "1"),
            head_tracking: env
                .var(launch::HEAD_TRACKING)
                .is_ok_and(|v| v.trim() == "1"),
        }
    }
}

// Process-wide (W26): the renderer thread and the AUHAL callback read these switches with no context. The host installs them once at start.
static SETTINGS: Slot<AudioSettings> = Slot::new("nxkit-svc-audio");

/// Installs the settings. The host calls it once, before it builds any part.
pub fn install(settings: AudioSettings) -> Result<(), SlotError> {
    SETTINGS.install(settings)
}

/// The channels of every frame between the outputs and the device: 6 (5.1) with spatial audio, else 2.
pub(crate) fn width() -> usize {
    if settings().spatial { 6 } else { 2 }
}

/// The installed settings (the defaults in a unit test).
pub fn settings() -> &'static AudioSettings {
    SETTINGS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        assert_eq!(
            AudioSettings::from_env(&Env::default()),
            AudioSettings::default()
        );
        let s = AudioSettings::from_env(&Env::from_pairs([
            ("NXKIT_AUDIO_CLICKS", "1"),
            ("NXKIT_AUDIO_DEVICE_FRAMES", "512"),
            ("NXKIT_SPATIAL_AUDIO", "1"),
        ]));
        assert!(s.clicks && s.spatial && !s.head_tracking);
        assert_eq!(s.device_frames, Some(512));
    }
}
