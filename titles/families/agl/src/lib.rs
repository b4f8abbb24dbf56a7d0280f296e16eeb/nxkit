//! The engine family of Nintendo EPD games on sead, agl and gsys.
//!
//! A fact goes here when every game of the family shows it. A fact that one game of the family does not share
//! stays in the title modules. Ocarina of Time (`0400486024DEC000`) and Tears of the Kingdom (`0100F2C0115B6000`)
//! set `render_cond`, `tex_flush_upload` and `tex_dirty_upload`. Breath of the Wild (`01007EF00011E000`) sets none
//! of them (the engine tree `dm/games/*/game.tsv` at `f25fa956`). So they are title facts.
//!
//! The frame step is a title fact too. Tears of the Kingdom has a `FRAME_STEP` stub of its own in the engine tree
//! (`dm/games/0100F2C0115B6000/patches.py` at `88c7e03f`, read 2026-11-07), and Ocarina of Time has none. The stubs
//! patch addresses of one build of one game, so no stub is shared. What the games share is the state page the
//! stubs read, and that protocol belongs to the loader, in `nxkit-svc-pacing`.
use nxtitle::{
    BuildHooks, Evidence, Fact, Family, Match, RuntimeHooks, ShaderRole, ShaderView, TitleFacts,
};

/// The family.
pub struct Agl;

/// The one instance the registry lists.
pub static AGL: Agl = Agl;

impl BuildHooks for Agl {
    fn shader_role(&self, shader: &ShaderView<'_>) -> Option<Fact<ShaderRole>> {
        // The place strings of the shader map end in `<pack>.pck#NDSR@<offset>`. `agl_ndsr_vr_light.pck` and
        // `agl_ndsr_vr.pck` do not contain each other, and neither contains `agl_ndsr.pck`.
        let name = NDSR_PACKS
            .iter()
            .find(|(pack, _)| shader.archive.is_some_and(|a| a.contains(pack)))
            .map(|(_, name)| *name)?;
        Some(Fact::new(
            ShaderRole::Upscaler { name },
            Evidence::Recorded {
                source: "docs/plans/ndsr-native.md section 2 at 118ada8",
                read: "2026-11-09",
            },
        ))
    }
}

/// The three NDSR archives in `Lib/agl/agl_resource.Nin_Ounce_NVN2.release.sarc.zs` and the upscaler each is.
const NDSR_PACKS: [(&str, &str); 3] = [
    ("agl_ndsr_vr_light.pck", "agl_ndsr_vr_light"),
    ("agl_ndsr_vr.pck", "agl_ndsr_vr"),
    ("agl_ndsr.pck", "agl_ndsr"),
];

impl RuntimeHooks for Agl {}

impl Family for Agl {
    fn id(&self) -> &'static str {
        "agl"
    }

    fn name(&self) -> &'static str {
        "Nintendo EPD engine (sead, agl, gsys)"
    }

    fn detect(&self, facts: &TitleFacts) -> Match {
        if facts.libraries.is_empty() {
            Match::Unknown
        } else if facts.has_library("sead") && facts.has_library("agl") {
            Match::Yes
        } else {
            Match::No
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxtitle::ShaderStage;

    fn facts(libs: &[&str]) -> TitleFacts {
        let mut f = TitleFacts::new(nxtitle::TitleId(0));
        f.libraries = libs.iter().map(|s| s.to_string()).collect();
        f
    }

    #[test]
    fn detects_from_libraries() {
        assert_eq!(AGL.detect(&facts(&[])), Match::Unknown);
        assert_eq!(AGL.detect(&facts(&["sead", "agl", "gsys"])), Match::Yes);
        assert_eq!(AGL.detect(&facts(&["UnrealEngine"])), Match::No);
    }

    #[test]
    fn ndsr_packs_are_upscalers() {
        for (archive, want) in [
            (
                "Lib/agl/agl_resource.sarc/agl_ndsr.pck#NDSR@0x10",
                "agl_ndsr",
            ),
            ("agl_ndsr_vr.pck", "agl_ndsr_vr"),
            ("agl_ndsr_vr_light.pck", "agl_ndsr_vr_light"),
        ] {
            let s = ShaderView {
                key: "0000000000000000",
                stage: ShaderStage::Compute,
                archive: Some(archive),
            };
            let r = AGL.shader_role(&s).expect(archive);
            assert_eq!(r.value, ShaderRole::Upscaler { name: want });
            assert!(r.evidence.is_checked());
        }
        let other = ShaderView {
            key: "0000000000000000",
            stage: ShaderStage::Compute,
            archive: Some("Shader/agl_ndsr_vr.bnsh"),
        };
        assert!(AGL.shader_role(&other).is_none());
        assert!(AGL.needs().is_empty());
    }
}
