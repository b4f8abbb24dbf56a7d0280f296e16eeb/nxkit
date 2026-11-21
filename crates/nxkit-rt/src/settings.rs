//! The settings of the NVN procs from the `NXKIT_*` environment (W27): every key that the crate reads, its default and
//! its parsing. The host builds them once from its snapshot ([`RtSettings::from_env`]) and hands them to
//! `nvn::install` in [`crate::nvn::Settings::env`]; the procs read them from the installed context.

use nxkit_abi::env::Env;

/// The settings of the NVN procs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RtSettings {
    /// `NXKIT_REC_LOCK=0`: the end of a recording leaves the cache invalidation to the next lock of the front end.
    pub defer_recorded: bool,
    /// `NXKIT_QUEUE_ORDER` (on unless `0`): waits hold work.
    pub queue_order: bool,
    /// `NXKIT_RELEASE_FLUSH` (on unless `0`): the flush after a release.
    pub release_flush: bool,
    /// `NXKIT_RELEASE_FLUSH_MIN`: the least work (draws, dispatches and copies) that the released submits bring, 64.
    pub release_flush_min: usize,
}

impl Default for RtSettings {
    fn default() -> Self {
        RtSettings::from_env(&Env::default())
    }
}

impl RtSettings {
    /// The settings from the snapshot of the host.
    pub fn from_env(env: &Env) -> Self {
        RtSettings {
            defer_recorded: env.var("NXKIT_REC_LOCK").is_ok_and(|v| v.trim() == "0"),
            queue_order: env.var("NXKIT_QUEUE_ORDER").map_or(true, |v| v != "0"),
            release_flush: env.var("NXKIT_RELEASE_FLUSH").map_or(true, |v| v != "0"),
            release_flush_min: env
                .var("NXKIT_RELEASE_FLUSH_MIN")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_keys_parse_as_the_reads_did() {
        let d = RtSettings::default();
        assert!(!d.defer_recorded && d.queue_order && d.release_flush);
        assert_eq!(d.release_flush_min, 64);
        let s = RtSettings::from_env(&Env::from_pairs([
            ("NXKIT_REC_LOCK", " 0"),
            ("NXKIT_QUEUE_ORDER", "0"),
            ("NXKIT_RELEASE_FLUSH_MIN", "8"),
        ]));
        assert!(s.defer_recorded && !s.queue_order && s.release_flush);
        assert_eq!(s.release_flush_min, 8);
    }
}
