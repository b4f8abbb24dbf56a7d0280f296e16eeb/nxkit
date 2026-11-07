//! What a standalone build refuses (R49). Every option the engine path reads and this path does not read gets a
//! named refusal, so no option is dropped without a word (R15).

use crate::Args;

fn no(code: i32, what: &str, why: &str) -> Result<(), String> {
    Err(crate::exit::fail(
        code,
        format!("unsupported: {what} {why}"),
    ))
}

/// Refuses the options of the engine path.
pub fn refuse(a: &Args) -> Result<(), String> {
    let engine = "belongs to the nxnative engine path. This title builds without nxnative";
    if a.engine.is_some() {
        return no(4, "--engine", &format!("({engine}): remove it"));
    }
    if a.platform == "ios" {
        return no(
            4,
            "--platform ios",
            "(an iOS app of a title that builds without nxnative is not built yet): use --platform mac",
        );
    }
    let flags: &[(bool, &str)] = &[
        (a.shaders.is_some(), "--shaders"),
        (a.companion.is_some(), "--companion-app"),
        (a.seen_shaders, "--seen-shaders"),
        (a.prepare_xcode, "--prepare-xcode"),
        (a.framework, "--framework"),
        (a.shell_framework.is_some(), "--shell-framework"),
        (a.provisional, "--provisional"),
        (a.survey_only, "--survey-only"),
        (a.nup.is_some(), "--nup"),
        (a.dump.is_some(), "--dump"),
        (a.romfs.is_some(), "--romfs"),
        (a.also_ios, "--also-ios"),
        (a.language.is_some(), "--language"),
        (a.env_file.is_some(), "--env-file"),
        (a.team_id.is_some(), "--team-id"),
        (a.xcode_workspace.is_some(), "--xcode-workspace"),
        (a.profile_out.is_some(), "--profile-out"),
        (a.package.is_some(), "--package"),
        (a.show_fps.is_some(), "--show-fps"),
        (a.motion.is_some(), "--motion"),
        (a.no_heap_check, "--no-heap-check"),
    ];
    for (set, name) in flags {
        if *set {
            return no(4, name, &format!("({engine} or an iOS app): remove it"));
        }
    }
    if a.program.is_some_and(|n| n != 0) {
        return no(4, "--program", "(a game with one program only): remove it");
    }
    if !(a.mac_sign.is_empty() || a.mac_sign == "-") {
        return no(4, "--mac-sign", "(the app is signed ad hoc): remove it");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Args {
        Args {
            platform: "mac".into(),
            mac_sign: "-".into(),
            ..Args::default()
        }
    }

    #[test]
    fn a_plain_mac_build_passes() {
        assert!(refuse(&args()).is_ok());
    }

    #[test]
    fn the_engine_is_refused_by_name() {
        let a = Args {
            engine: Some("/e".into()),
            ..args()
        };
        let e = refuse(&a).unwrap_err();
        assert!(e.starts_with("unsupported: --engine"), "{e}");
    }

    #[test]
    fn ios_and_the_shader_folder_are_refused() {
        let a = Args {
            platform: "ios".into(),
            ..args()
        };
        assert!(refuse(&a).unwrap_err().contains("--platform ios"));
        let a = Args {
            shaders: Some("/s".into()),
            ..args()
        };
        assert!(refuse(&a).unwrap_err().contains("--shaders"));
    }
}
