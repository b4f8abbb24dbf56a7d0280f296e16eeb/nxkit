//! The title module of Mario Tennis Fever for Nintendo Switch 2 (`0400D4201C6EA000`).
//!
//! The game runs on Camelot's engine on NVN2. It has no family module yet. This module holds what is known to be
//! true for this game and that the generic crates must not know. The first fact is a command buffer quirk.

use nxtitle::{BuildHooks, Evidence, Fact, Need, RuntimeHooks, Title, TitleId};

/// The title module.
pub struct Mtf;

/// The one instance the registry lists.
pub static MTF: Mtf = Mtf;

const HLE_NVN: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/gfx/nvn/hle_nvn.c at a9d06b6f, nvnCommandBufferBindVertexBuffers",
    read: "2026-11-07",
};

const NEEDS: &[Fact<Need>] = &[Fact::new(
    Need {
        capability: "vertex_stream_null_bind",
        setting: "ignore",
        why: "the game binds a null range to the vertex streams 16 to 31 before each model, thousands of times a run. \
              NVN has 16 streams, so nothing reads them. The command buffer ignores a bind of one null range at \
              the stream 16 or above. A real range past stream 15 still ends the game.",
    },
    HLE_NVN,
)];

impl BuildHooks for Mtf {}

impl RuntimeHooks for Mtf {
    fn needs(&self) -> &'static [Fact<Need>] {
        NEEDS
    }
}

impl Title for Mtf {
    fn id(&self) -> TitleId {
        TitleId(0x0400_D420_1C6E_A000)
    }

    fn name(&self) -> &'static str {
        "Mario Tennis Fever"
    }

    fn family(&self) -> Option<&'static str> {
        None
    }

    fn profile(&self) -> Option<&'static str> {
        Some(include_str!("../profile.toml"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxb_profile::Profile;

    #[test]
    fn profile_matches_the_module() {
        let p = Profile::from_toml(MTF.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(MTF.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(MTF.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn the_command_buffer_need_has_a_record() {
        let keys: Vec<_> = MTF.needs().iter().map(|n| n.value.capability).collect();
        assert_eq!(keys, ["vertex_stream_null_bind"]);
        assert!(MTF.needs().iter().all(|n| n.evidence.is_checked()));
    }
}
