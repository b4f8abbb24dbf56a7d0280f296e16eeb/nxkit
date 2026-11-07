//! The engine family of Nintendo EPD games on sead, agl and gsys.
//!
//! A fact goes here when every game of the family shows it. A fact that one game of the family does not share
//! stays in the title modules. Ocarina of Time (`0400486024DEC000`) and Tears of the Kingdom (`0100F2C0115B6000`)
//! set `render_cond`, `tex_flush_upload` and `tex_dirty_upload`. Breath of the Wild (`01007EF00011E000`) sets none
//! of them (`seedds/nxnative` `dm/games/*/game.tsv` at `f25fa956`). So they are title facts.
//!
//! The frame step is a title fact too. Tears of the Kingdom has a `FRAME_STEP` stub of its own in `seedds/nxnative`
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
        shader.archive.filter(|a| a.contains("agl_ndsr")).map(|_| {
            Fact::new(
                ShaderRole::Upscaler {
                    name: "agl_ndsr_vr",
                },
                Evidence::Unverified {
                    note: "the upscaler networks are named agl_ndsr_vr in the nxnative OoT README. That their \
                           archive path carries this name is not checked.",
                },
            )
        })
    }
}

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
    fn ndsr_is_an_upscaler_and_unverified() {
        let s = ShaderView {
            key: "0000000000000000",
            stage: ShaderStage::Compute,
            archive: Some("Shader/agl_ndsr_vr.bnsh"),
        };
        let r = AGL.shader_role(&s);
        assert!(matches!(
            r.map(|f| f.value),
            Some(ShaderRole::Upscaler { .. })
        ));
        assert!(r.is_some_and(|f| !f.evidence.is_checked()));
        assert!(AGL.needs().is_empty());
    }
}
