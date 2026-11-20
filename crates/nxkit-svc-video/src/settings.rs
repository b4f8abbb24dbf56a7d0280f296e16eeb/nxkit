//! The settings of the video service from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The host builds them once from its snapshot ([`VideoSettings::from_env`]) and installs them ([`install`])
//! before it builds any part. The code of the crate reads them through [`settings`].

use nxkit_abi::env::{Env, Slot, SlotError};

/// The settings of the video service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoSettings {
    /// `NXKIT_AV1_SOFTWARE` (set, not empty and not `0`): AV1 decodes in software (rav1d) on every Mac.
    pub av1_software: bool,
    /// `NXKIT_AV1_FRAMES` (1 to 8): the frames in flight of the software AV1 decoder. `None` keeps the default.
    pub av1_frames: Option<usize>,
    /// `NXKIT_DIAG` names `codec`, `codecall` or `all`: one line per decoder call.
    pub diag_codec: bool,
    /// `NXKIT_DIAG` names `codec` or `all` (without `codecall`): the trace of the decoder calls, at most 600.
    pub trace_codec: bool,
    /// `NXKIT_DIAG` names `codecall`: every call is traced, with its time.
    pub trace_all: bool,
}

impl VideoSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        let diag = env.var("NXKIT_DIAG").ok();
        let has = |c: &str| {
            diag.as_deref()
                .is_some_and(|v| v.split(',').any(|x| x == c))
        };
        VideoSettings {
            av1_software: env
                .var("NXKIT_AV1_SOFTWARE")
                .is_ok_and(|v| !v.is_empty() && v != "0"),
            av1_frames: env
                .var("NXKIT_AV1_FRAMES")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .map(|n| n.clamp(1, 8)),
            diag_codec: has("codec") || has("codecall") || has("all"),
            trace_codec: has("codecall") || has("codec") || has("all"),
            trace_all: has("codecall"),
        }
    }
}

// Process-wide (W26): the decoder threads and the `movie::Decoder` take-overs, which the game calls with no context, read these switches. The host installs them once at start.
static SETTINGS: Slot<VideoSettings> = Slot::new("nxkit-svc-video");

/// Installs the settings. The host calls it once, before it builds any part.
pub fn install(settings: VideoSettings) -> Result<(), SlotError> {
    SETTINGS.install(settings)
}

/// The installed settings (the defaults in a unit test).
pub fn settings() -> &'static VideoSettings {
    SETTINGS.get()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        assert_eq!(
            VideoSettings::from_env(&Env::default()),
            VideoSettings::default()
        );
        let s = VideoSettings::from_env(&Env::from_pairs([
            ("NXKIT_AV1_SOFTWARE", "1"),
            ("NXKIT_AV1_FRAMES", " 12 "),
            ("NXKIT_DIAG", "health,codecall"),
        ]));
        assert!(s.av1_software && s.diag_codec && s.trace_codec && s.trace_all);
        assert_eq!(s.av1_frames, Some(8));
        assert!(
            !VideoSettings::from_env(&Env::from_pairs([("NXKIT_AV1_SOFTWARE", "0")])).av1_software
        );
    }
}
