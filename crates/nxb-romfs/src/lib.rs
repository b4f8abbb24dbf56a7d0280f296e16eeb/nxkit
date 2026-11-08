//! RomFS tables, the host's `DMROMFS1` image layout planned before any data is read, and `romfs.id`.
//!
//! Status: [`image`] reads and moves files in the index of a `DMROMFS1` image, [`sarc`] finds a member of a SARC
//! archive. The other modules are placeholders that fix the crate's shape, see `docs/workspace-layout.md` for the
//! planned public surface.

pub mod image;
pub mod sarc;

/// Nintendo RomFS directory and file tables.
pub mod tables {}

/// `ImageLayout::plan`: offsets, index and header of a `DMROMFS1` image.
pub mod layout {}

/// `romfs.id` of a layout.
pub mod identity {}
