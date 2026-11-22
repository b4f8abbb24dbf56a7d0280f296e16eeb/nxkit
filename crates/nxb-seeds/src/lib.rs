//! The one-time import of the old engine's pipeline lists into the pipeline seeds of the title modules
//! (`nxkit_pipestate::seeds`). Only this crate knows the engine's formats: the build and the host read seeds.

pub mod import;
pub mod skey;
