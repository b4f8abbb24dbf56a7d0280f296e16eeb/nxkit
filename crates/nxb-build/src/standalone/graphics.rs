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

#[cfg(test)]
mod tests {
    use super::*;

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
