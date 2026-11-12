//! Extract (design-pipeline-rest 9.1): the game's NSP packages to what `nx_extract` writes per version (`ExeFS/`,
//! `control.nacp`, `icon.jpg`, the `DMROMFS1` image `RomFS.img`), byte-identical, in one pass for all versions.
//!
//! - One process, all versions: the base section is decrypted through one [`cache::BlockCache`] that `v0` and every
//!   update's relocated (BKTR) reads share, so a base block is decrypted once while both versions need it.
//! - Every read is checked against the section's IVFC hash tree (`ErrorOnInvalid`), a failed block marks its file damaged
//!   and the stage fails listing them ([`ExtractError::Damaged`], exit code 3).
//! - The image layout is planned from the RomFS tables before any data is read ([`image::ImagePlan`]); workers write each
//!   file at its offset (`pwrite`), and one hasher per image takes the files in image order: SHA-256 + SHA-1 of the
//!   image and SHA-256 per file come from the bytes as they are written, never from a re-read ([`mod@write`]).
//!
//! Layer: L3 for now (it does file I/O and threads). Its pure parts (AES, PFS0/NCA/IVFC/BKTR/RomFS parsing, the image
//! plan) belong in `nxb-crypto`, `nxb-hos` and `nxb-romfs` once their owner takes them.
//! A sparse base program is read only together with its update NSP, as `nx_extract` does (a named `Unsupported` error
//! for the base alone, never a silent difference).

pub mod aes;
pub mod aoc;
pub mod bktr;
pub mod cache;
pub mod cli;
pub mod error;
pub mod facts;
pub mod fileext;
pub mod html;
pub mod image;
pub use nxb_split::inflate;
pub mod ivfc;
pub mod keys;
pub mod listing;
pub mod manifest;
pub mod meta;
pub mod nca;
pub mod ncz;
pub mod nup;
pub mod post;
pub mod prof;
pub mod romfs;
pub mod run;
pub mod safe_path;
pub mod sparse;
pub mod split;
pub mod split_cli;
pub mod split_out;
pub mod storage;
pub mod version;
pub mod write;

pub use error::{ExtractError, Result};
pub use run::{Counter, Request, Summary, extract, extract_code_only_update};
