#![allow(missing_docs)]
//! The shader translator: Maxwell and Ampere machine code to Metal Shading Language.
//!
//! The modules form layers (W28, `docs/architecture.md`). A module uses only its own layer and the ones below:
//!
//! 1. base: `types`, `job`, `accessor` (the answers to the queries of the job), `corpus`
//! 2. `ir`: the intermediate representation, with no translation state
//! 3. `translation`: the state of one translation (header, definitions, host answers, resources)
//! 4. decode: `cfg` and `emit` (the instruction emitters and their `EmitterContext`)
//! 5. passes: `helper` (helper functions, soft float), `opt`, `xform`
//! 6. structure: `structured` and `props` (the back end's view of the definitions)
//! 7. front end: `driver` (the retry loop with the guessed image and texture types and the 64 bit float lowering) and
//!    `front` (the pipeline from the machine code to the structured program)
//! 8. back end: `msl` turns the structured program into MSL libraries and their reflection. Its entry
//!    `msl::translate` runs the front end first.

pub mod accessor;
pub mod cfg;
pub mod corpus;
pub mod driver;
pub mod emit;
pub mod front;
pub mod helper;
pub mod ir;
pub mod job;
pub mod msl;
pub mod opt;
pub mod props;
pub mod structured;
pub mod translation;
pub mod types;
pub mod xform;

/// The Maxwell decoder crate (kept under its old module path).
pub use nxb_maxwell as maxwell;
pub use nxb_maxwell::attr;
mod ampere_decode_test;
mod decoder_test;
