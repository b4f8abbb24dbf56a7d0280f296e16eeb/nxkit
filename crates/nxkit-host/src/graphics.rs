//! The graphics API of the game, from `game.json`: the host installs the layer of that API and no other.

use crate::error::Result;
use crate::json::Value;
use crate::fields::bad;

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
    /// Reads the key; an absent key is NVN.
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
