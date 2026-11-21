//! RomFS tables, the host's `DMROMFS1` image layout planned before any data is read, and `romfs.id`.
//!
//! Status: [`image`] reads and moves files in the index of a `DMROMFS1` image, [`sarc`] finds a member of a SARC
//! archive. The RomFS tables and the layout plan are in `nxb-extract` until their owner moves them (W30).

pub mod image;
pub mod sarc;
