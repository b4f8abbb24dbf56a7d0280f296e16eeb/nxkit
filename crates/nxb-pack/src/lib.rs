//! `nxb-pack`: the Mac app stage (staging, signing) of the build.
//!
//! Origin: the signer and the app stage began as a standalone crate and are part of this workspace now.
//! The SHA-1/SHA-256 implementation (and the `unsafe` ARMv8 code) is `nxb-model`'s, the workspace lints
//! apply, and the CLI is `nxb-pack`. Its unit tests and `codesign` oracle tests are in `tests/oracle.rs`.
//! The fallback contract stays: any input the signer does not implement is `Error::Unsupported` (CLI exit
//! code 3) and the caller runs `codesign`.
//!
//! An ad-hoc code signer for macOS app bundles and Mach-O files.
//!
//! The output is meant to be byte-identical to what `codesign --force --sign - <path>` writes on
//! the same input (the `codesign` of the macOS this crate was proven on, see docs/codesign.md for the
//! format notes and the proof). `codesign` stays the oracle and the fallback.
//!
//! Layers, from the bottom:
//! - [`hash`]: SHA-1/SHA-256 (from `nxb-model`), hex helpers.
//! - [`plist`]: a small XML property list reader and a writer with CoreFoundation's layout.
//! - [`der`]: the DER form of entitlements (special slot 7).
//! - [`fat`]: universal files (split, `codesign_allocate` layout).
//! - [`macho`]: thin Mach-O parsing and the `codesign_allocate`-style layout of the signature.
//! - [`blob`]: CodeDirectory, requirements, entitlement blobs and the embedded-signature superblob.
//! - [`macho_sign`]: signing one Mach-O image in memory ([`sign_macho`]).
//! - [`resources`]: the resource rules, the bundle plan and the `CodeResources` seal ([`seal`]).
//! - [`digest`]: multi-threaded file hashing (each file read once) and precomputed digests.
//! - [`bundle`]: signing a whole bundle ([`sign_bundle`]): deep, versioned framework, shallow.
//! - [`appstage`]: staging a file set into a bundle (parallel clones) and signing it.
//!
//! All modules but [`bundle`], [`appstage`] and [`digest`] are pure: they take bytes and return bytes.

pub mod appstage;
pub mod blob;
pub mod bundle;
pub mod cli;
pub mod der;
pub mod digest;
pub mod error;
pub mod fat;
pub mod hash;
pub mod macho;
pub mod macho_sign;
pub mod plist;
pub mod resources;
pub mod tz;

pub use bundle::{BundleOptions, BundleReport, sign_bundle};
pub use digest::{DigestSource, FileDigests, NoDigests, TsvDigests};
pub use error::{Error, Result};
pub use macho_sign::{SignParams, SignedMachO, cdhashes_of, sign_macho};
pub use resources::{BundlePlan, PlanEntry, seal};
