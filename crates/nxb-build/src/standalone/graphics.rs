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

/// The API a game asks for by name, from the read-only data of its modules. A game on NVN carries `nvnBootstrapLoader`,
/// a game on Vulkan `vkCreateInstance`, a game on OpenGL `eglGetProcAddress` or `glGetString`. `None` names no API.
pub(super) fn detect<'a>(ro: impl IntoIterator<Item = &'a [u8]> + Clone) -> Option<GraphicsApi> {
    let has = |needle: &[u8]| {
        ro.clone()
            .into_iter()
            .any(|d| d.windows(needle.len()).any(|w| w == needle))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_comes_from_the_names_in_the_read_only_data() {
        let d = |s: &'static [u8]| [s];
        assert_eq!(
            detect(d(b"x\0nvnBootstrapLoader\0")),
            Some(GraphicsApi::Nvn)
        );
        assert_eq!(
            detect(d(b"x\0vkCreateInstance\0")),
            Some(GraphicsApi::Vulkan)
        );
        assert_eq!(detect(d(b"eglGetProcAddress\0")), Some(GraphicsApi::OpenGl));
        assert_eq!(detect(d(b"glGetStringi\0")), None);
        assert_eq!(detect(d(b"nothing")), None);
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
