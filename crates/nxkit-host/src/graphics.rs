//! The graphics API of the game, from `game.json`: the host installs the layer of that API and no other.

use crate::error::Result;
use crate::fields::bad;
use crate::json::Value;

/// The graphics API of the game (`"graphics"`). An app of a build before the key renders with NVN.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Graphics {
    /// NVN: the host's NVN layer.
    #[default]
    Nvn,
    /// Vulkan: the host's Vulkan layer answers the `vk*` imports.
    Vulkan,
}

impl Graphics {
    /// The layer of `nxkit-rt` for the API: the loader takes the functions of the game from its table
    /// (`nxkit_rt::entries_for`).
    pub fn layer(self) -> nxkit_rt::Graphics {
        match self {
            Graphics::Nvn => nxkit_rt::Graphics::Nvn,
            Graphics::Vulkan => nxkit_rt::Graphics::Vulkan,
        }
    }

    /// Reads the key. An absent key is NVN.
    pub(crate) fn parse(v: Option<&Value>) -> Result<Graphics> {
        match v.map(|v| v.as_str()) {
            None => Ok(Graphics::Nvn),
            Some(Some("nvn")) => Ok(Graphics::Nvn),
            Some(Some("vulkan")) => Ok(Graphics::Vulkan),
            Some(other) => Err(bad(format!(
                "\"graphics\" is {other:?}, not \"nvn\" or \"vulkan\""
            ))),
        }
    }
}
