//! The one list of families and title modules.
//!
//! Each module is behind its own cargo feature: `engine-<id>` for `nxengine-<id>`, `graphics-<id>` for
//! `nxgraphics-<id>` and the crate name without the prefix for a title (`smbw` for `nxgame-smbw`), so a build can leave
//! any of them out. This is the only crate that depends on family and title crates. The core depends on this crate and
//! on `nxtitle`. The lists are written by the registry script from the folders `titles/engines`, `titles/graphics`
//! and `titles/games`.

use nxtitle::Catalog;

mod families;
mod titles;

/// The catalog of every module this build holds.
pub fn catalog() -> Result<Catalog, String> {
    Catalog::new(families::all(), titles::all())
}

#[cfg(test)]
mod tests;
