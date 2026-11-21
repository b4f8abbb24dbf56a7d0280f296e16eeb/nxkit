//! Decompressors and compressors used by extract, split, rewrite and the IPA writer.
//!
//! All decoders are total: bounded output, no panic on malformed input.
//!
//! Status: only the ZBIC decoder is here. LZ4, zstd and the other decoders that the stages use live in the stage crates
//! (`nxb-split`, `nxb-rewrite`) until their owner moves them (W30).

/// ZBIC, Nintendo's zstd variant (NSO flag bit 7).
pub mod zbic;
