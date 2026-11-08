//! The title module of The Legend of Zelda: Ocarina of Time for Nintendo Switch 2 (`0400486024DEC000`).
//!
//! The game runs on Nintendo EPD's engine (family `agl`) on NVN2. What every game of that engine shares is in the
//! family. This module holds what is known to be true for this game. `render_cond`, `tex_flush_upload` and
//! `tex_dirty_upload` come from agl code, but Breath of the Wild does not set them, so they are here.
//!
//! The upscaler choice and the frame pacing have no answer yet, so the core defaults apply. The seed list for the
//! prepare step is `pso_manifest.tsv` in `seedds/nxnative` `dm/games/0400486024DEC000/`.

use nxtitle::{BuildHooks, Evidence, Fact, Need, RuntimeHooks, Title, TitleId};

mod resolution;

/// The title module.
pub struct Oot;

/// The one instance the registry lists.
pub static OOT: Oot = Oot;

const README: Evidence = Evidence::Recorded {
    source: "seedds/nxnative dm/games/0400486024DEC000/README.md at c8bfa161",
    read: "2026-11-07",
};

const NEEDS: &[Fact<Need>] = &[
    Fact::new(
        Need {
            capability: "gpu_timestamps",
            setting: "segment",
            why: "the game's dynamic resolution decides from GPU timestamps. With one time per submission it never \
                  lowers its resolution.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "draw_barriers",
            setting: "game",
            why: "about 180 full barriers a frame that NVN does not ask for. GPU time 25 ms down to 18 ms at full \
                  resolution.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "mem_map_limit",
            setting: "on",
            why: "the SDK allocator keeps freed pages mapped until a map fails, then gives them back. Without a \
                  failing map past the application pool the memory only grows.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "render_cond",
            setting: "on",
            why: "agl clears a copy of the prepass depth again under a render condition the console skips. Run \
                  unconditionally, the opaque pass's EQUAL depth test fails everywhere.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "tex_flush_upload",
            setting: "on",
            why: "the CPU writes terrain height, blend and hole maps into textures after creating them. Without \
                  the upload they stay zero: blue holes in the ground. Also seen in Tears of the Kingdom.",
        },
        README,
    ),
    Fact::new(
        Need {
            capability: "tex_dirty_upload",
            setting: "on",
            why: "the terrain maps are rewritten from the CPU as tile slots are reused. Also seen in Tears of the \
                  Kingdom.",
        },
        README,
    ),
];

impl BuildHooks for Oot {
    fn resource_patches(&self) -> &'static [Fact<nxtitle::ResourcePatch>] {
        resolution::PATCHES
    }
}

impl RuntimeHooks for Oot {
    fn needs(&self) -> &'static [Fact<Need>] {
        NEEDS
    }
}

impl Title for Oot {
    fn id(&self) -> TitleId {
        TitleId(0x0400_4860_24DE_C000)
    }

    fn name(&self) -> &'static str {
        "The Legend of Zelda: Ocarina of Time"
    }

    fn family(&self) -> Option<&'static str> {
        Some("agl")
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
        let p = Profile::from_toml(OOT.profile().unwrap_or_default()).unwrap();
        assert_eq!(p.title_id.as_deref(), Some(OOT.id().to_string().as_str()));
        assert_eq!(p.declared.title.as_deref(), Some(OOT.name()));
        assert_eq!(p.problems(), Vec::<String>::new());
    }

    #[test]
    fn every_need_has_a_record() {
        assert!(OOT.needs().iter().all(|n| n.evidence.is_checked()));
        assert!(OOT.upscaler().is_none() && OOT.pacing().is_none());
    }
}
