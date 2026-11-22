//! What the build makes of a game's graphics API: the APIs nxkit has a host layer for.

use nxtitle::GraphicsApi;

/// `Ok` when nxkit has a host layer for the API, else the note the build stops with.
pub(super) fn supported(api: GraphicsApi, name: &str, id: u64) -> Result<(), String> {
    match api {
        GraphicsApi::Nvn | GraphicsApi::Vulkan => Ok(()),
        GraphicsApi::OpenGl => Err(format!(
            "{name} ({id:016X}) renders with OpenGL, and nxkit has no OpenGL layer yet: the title module holds its \
             build data, but the build cannot make the game"
        )),
    }
}

/// The API a game asks for by name: the NVN or Vulkan function names, or the OpenGL entry points, in the read-only data
/// of its modules. A game on NVN carries `nvnBootstrapLoader`, a game on Vulkan `vkGet*ProcAddr` and `vkCreateInstance`.
pub(super) fn detect(mods: &[(super::modules::Built, u64)]) -> Option<GraphicsApi> {
    let has = |needle: &[u8]| {
        mods.iter().any(|(m, _)| {
            let (off, size) = m.parts[1];
            m.image
                .get(off..off.saturating_add(size))
                .is_some_and(|ro| ro.windows(needle.len()).any(|w| w == needle))
        })
    };
    if has(b"nvnBootstrapLoader\0") {
        Some(GraphicsApi::Nvn)
    } else if has(b"vkCreateInstance\0") {
        Some(GraphicsApi::Vulkan)
    } else if has(b"eglGetProcAddress\0") || has(b"glGetString\0") {
        Some(GraphicsApi::OpenGl)
    } else {
        None
    }
}

/// `Ok` when the generic path can build a game on `api` (NVN only: it has no Vulkan shader step and no module
/// choice). `None` is a game that names no API, which stays on NVN as before.
pub(super) fn generic_supported(api: Option<GraphicsApi>, id: u64) -> Result<(), String> {
    match api {
        None | Some(GraphicsApi::Nvn) => Ok(()),
        Some(GraphicsApi::Vulkan) => Err(format!(
            "unsupported: the game {id:016X} renders with Vulkan, and the generic path builds games on NVN only: it needs a title module"
        )),
        Some(GraphicsApi::OpenGl) => Err(format!(
            "unsupported: the game {id:016X} renders with OpenGL, and nxkit has no OpenGL layer yet"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generic_path_builds_nvn_and_unnamed_only() {
        assert!(generic_supported(None, 1).is_ok());
        assert!(generic_supported(Some(GraphicsApi::Nvn), 1).is_ok());
        let e = generic_supported(Some(GraphicsApi::Vulkan), 0x1234).unwrap_err();
        assert!(
            e.contains("0000000000001234") && e.contains("Vulkan"),
            "{e}"
        );
        assert!(generic_supported(Some(GraphicsApi::OpenGl), 1).is_err());
    }

    #[test]
    fn an_opengl_game_stops_with_a_note_that_names_it() {
        assert!(supported(GraphicsApi::Nvn, "a", 1).is_ok());
        assert!(supported(GraphicsApi::Vulkan, "a", 1).is_ok());
        let e = supported(GraphicsApi::OpenGl, "Balatro", 0x0100_CD80_1CE5_E000).unwrap_err();
        assert!(
            e.contains("Balatro (0100CD801CE5E000)") && e.contains("no OpenGL layer"),
            "{e}"
        );
    }
}
