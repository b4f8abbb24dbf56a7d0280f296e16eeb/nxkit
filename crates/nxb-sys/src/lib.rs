//! Platform services for every stage (layer L0). The only crate with `unsafe` code.
//!
//! Each concern is one module with a safe API and `cfg(target_os)` implementations. Nothing above L0 names an OS.
//!
//! Present: process groups ([`process`]), machine facts ([`system`]), hardware SHA-256 and SHA-1 ([`sha256`], [`sha1`]), hardware AES-128 ([`aes`]), the system zlib ([`zlib`]),
//! standard output and interrupt signals ([`stdio`]), `flock` locks ([`lock`]), APFS clones ([`clone`]), child `rusage` ([`process::wait4`]).
//! Planned (design-v2 7.2, design-pipeline-rest 5.1): Windows (LockFileEx, Job Objects), QoS classes, `F_PREALLOCATE`, `renamex_np`, index exclusion, CommonCrypto / CNG.

pub mod aes;
pub mod clone;
pub mod lock;
pub mod process;
pub mod sha1;
pub mod sha256;
pub mod stdio;
pub mod system;
#[cfg(not(target_family = "wasm"))]
pub mod zlib;
