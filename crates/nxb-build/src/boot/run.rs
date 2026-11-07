//! The order of the boot steps after the rewrite, as the NSP script's main runs them: the NRO capture (inside its
//! rewrite), the heap and region checks, the GLSLC capture, the Unreal decode and the Vulkan or GL capture. What they
//! leave for the shader steps is recorded in `<work>/shader/host_inputs.tsv` ([`super::inputs::HostInputs`]).

use std::path::Path;

use super::Ctx;
use super::inputs::HostInputs;
use crate::Args;
use crate::events::Progress;
use crate::graph::Build;
use crate::host::guess_src::Sources;
use crate::host::survey_profile as sp;

/// `apple_silicon()`: the Mac runs arm64 code (the host runs the game's own code natively).
pub(crate) fn apple_silicon() -> bool {
    std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.optional.arm64"])
        .output()
        .is_ok_and(|o| o.stdout.trim_ascii() == b"1")
}

/// `pg.build_sources(game/ExeFS, romfs, g.modules_in(game/ExeFS))`: how the shaders of the packed version are got.
pub(crate) fn build_sources(profile: &Path, exefs: &Path, img: &Path) -> Result<Sources, String> {
    let g = crate::host::profile::Game::open(profile)?;
    let mods: Vec<String> = g
        .words_or("modules", "main")
        .into_iter()
        .filter(|m| exefs.join(m).is_file())
        .collect();
    let facts = crate::host::guess_facts::main_facts(exefs)?;
    let rom = crate::host::rom::Rom::open(img)?;
    let inp = crate::host::build_sources::build_inputs(&facts, exefs, &rom, &mods)?;
    Ok(crate::host::guess_src::shader_sources(&inp))
}

/// Which boot steps a build runs, from [`build_sources`] (`glslc`, `vkcap`, `glcap`, `uedec` and the Oodle split of
/// the script's main).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Plan {
    /// `nvn`, `vulkan` or `gl`.
    pub(crate) gfx: String,
    /// The GLSLC capture.
    pub(crate) glslc: bool,
    /// The Vulkan capture.
    pub(crate) vkcap: bool,
    /// The GL capture.
    pub(crate) glcap: bool,
    /// The Unreal decode.
    pub(crate) uedec: bool,
    /// The split decodes with the game's own decoder in the Mac host.
    pub(crate) oodle: bool,
    /// The lines the script says for the steps that are on.
    pub(crate) say: Vec<String>,
}

/// The script's decisions from `src`.
pub(crate) fn plan(src: &Sources) -> Plan {
    let gfx = src.graphics.0.clone();
    let nvn = gfx == "nvn";
    let p = Plan {
        glslc: nvn && src.glslc_capture.0,
        vkcap: gfx == "vulkan" && src.vk_capture.0,
        glcap: gfx == "gl" && src.gl_capture.0,
        uedec: nvn && src.ue_decode.0,
        oodle: nvn && (src.oodle_packs.0 || src.ue_legacy.0),
        gfx,
        say: Vec::new(),
    };
    let say = [
        (p.glslc, &src.glslc_capture.1),
        (p.vkcap, &src.vk_capture.1),
        (p.glcap, &src.gl_capture.1),
        (p.uedec, &src.ue_decode.1),
        (nvn && src.oodle_packs.0, &src.oodle_packs.1),
        (nvn && src.ue_legacy.0, &src.ue_legacy.1),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, s)| s.clone())
    .collect();
    Plan { say, ..p }
}

/// The boot steps after the rewrite (and, for a provisional profile, after `refine`). Each one is skipped while its
/// output is complete, so a build with unchanged inputs boots nothing.
pub(crate) fn after_rewrite(a: &Args, b: &Build, p: &Progress) -> Result<(), String> {
    let c = Ctx::of(b, p)?;
    let ver = c.game_dir();
    let exefs = ver.join("ExeFS");
    let img = ver.join("RomFS.img");
    if !exefs.join("main").is_file() || !img.is_file() {
        return Ok(());
    }
    let mut inputs = HostInputs::default();
    if super::nro::capture_nro(&c)? {
        crate::rewrite::ensure_with(&b.engine, &b.profile.0, &b.work, &b.package, Some(p))?;
    }
    inputs.nro_dir = super::captured_nros(&b.work.join(format!("nro_{}", b.package)));
    if b.prov {
        checks(a, &c, &exefs)?;
    }
    let src = build_sources(&b.profile.0, &exefs, &img)?;
    let pl = plan(&src);
    for l in &pl.say {
        p.say(l);
    }
    let dirs: Vec<(String, std::path::PathBuf)> = b
        .split_versions
        .iter()
        .map(|v| (v.clone(), c.work.join("extract").join(v)))
        .collect();
    if pl.glslc {
        inputs.glslc_observed = super::captures::capture_glslc(&c, &dirs)?.unwrap_or_default();
        inputs.split_again = true;
    }
    if pl.uedec {
        super::uedec::decode_ue_shaders(&c, &b.split_versions)?;
        inputs.split_again = true;
    }
    if pl.vkcap {
        inputs.capture = Some(super::captures::capture_shaders(
            &c,
            super::captures::Api::Vk,
        )?);
    }
    if pl.glcap {
        inputs.capture = Some(super::captures::capture_shaders(
            &c,
            super::captures::Api::Gl,
        )?);
    }
    if let Some(d) = &inputs.capture {
        // the Mac shader stage (crate::layer) reads the capture folder with the RomFS
        p.say(&format!(
            "the {} shaders captured in {} go into the Mac shader stage",
            if pl.vkcap { "Vulkan" } else { "GL" },
            d.display()
        ));
    }
    if pl.oodle {
        inputs.oodle = Some((c.host()?, c.patched()));
        inputs.split_again = true;
    }
    if inputs.nro_dir.is_some() {
        inputs.split_again = true;
    }
    inputs.save(&b.work)
}

/// The heap check and the region check of a provisional profile, in the script's order and with its conditions.
fn checks(a: &Args, c: &Ctx<'_>, exefs: &Path) -> Result<(), String> {
    if a.no_heap_check {
        c.p.say("--no-heap-check: no heap check and no region check, the guessed heap and address layout stays");
        return Ok(());
    }
    let gtsv = c.profile.join("game.tsv");
    let text = std::fs::read_to_string(&gtsv).map_err(|e| format!("{}: {e}", gtsv.display()))?;
    let arm = apple_silicon();
    if !sp::heap_checked(&text) && sp::heap_check_needed(exefs)? {
        if arm {
            super::checks::heap_check(c)?;
        } else {
            c.p.say("no heap check: it boots the game in the Mac host, which needs a Mac with Apple silicon; the profile keeps its guessed heap keys");
        }
    }
    let text = std::fs::read_to_string(&gtsv).map_err(|e| format!("{}: {e}", gtsv.display()))?;
    if sp::has_va(exefs)?
        && !sp::region_checked(&text, &super::checks::rewrite_id(&c.patched()))
        && arm
    {
        super::checks::region_check(c, exefs)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(g: &str, flags: [bool; 6]) -> Sources {
        let f = |i: usize, k: &str| (flags[i], format!("{k} reason"));
        Sources {
            graphics: (g.into(), String::new()),
            vk_capture: f(0, "vk"),
            gl_capture: f(1, "gl"),
            glslc_capture: f(2, "glslc"),
            ue_decode: f(3, "ue"),
            oodle_packs: f(4, "oodle"),
            ue_legacy: f(5, "legacy"),
        }
    }

    #[test]
    fn the_steps_follow_the_graphics_api() {
        let p = plan(&src("nvn", [true; 6]));
        assert!(p.glslc && p.uedec && p.oodle && !p.vkcap && !p.glcap);
        assert_eq!(
            p.say,
            ["glslc reason", "ue reason", "oodle reason", "legacy reason"]
        );
        let p = plan(&src("vulkan", [true; 6]));
        assert!(p.vkcap && !p.glslc && !p.uedec && !p.oodle && !p.glcap);
        assert_eq!(p.say, ["vk reason"]);
        let p = plan(&src("gl", [false, true, false, false, false, false]));
        assert!(p.glcap && !p.vkcap);
        let p = plan(&src("nvn", [false, false, false, false, false, true]));
        assert!(p.oodle && p.say == ["legacy reason"]);
        assert_eq!(
            plan(&src("nvn", [false; 6])),
            Plan {
                gfx: "nvn".into(),
                ..Plan::default()
            }
        );
    }
}
