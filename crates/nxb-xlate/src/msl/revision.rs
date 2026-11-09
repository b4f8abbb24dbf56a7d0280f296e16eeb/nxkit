//! Feature revisions: versions of parts of the translator whose output changes only for the shaders that use them.
//!
//! The translation store keys every shader on [`super::TRANSLATOR_ID`], so a bump of it translates every shader again.
//! A change that alters the MSL only of shaders that use one feature (an instruction class, a helper) bumps that
//! feature's [`Feature::revision`] instead: only the shaders whose code [`Feature::uses`] the feature get a new key.
//!
//! Rules:
//! 1. [`Feature::uses`] reads only the job and the code bytes, never the translator's output, so the key is known
//!    before the translation. It may say yes for a shader that does not use the feature (a word of data that decodes
//!    as the instruction), never no for one that does.
//! 2. Bump a revision only when every shader whose MSL changes is one that [`Feature::uses`] says yes for. Otherwise
//!    bump `TRANSLATOR_ID`.
//! 3. A shader that uses no feature has no feature field in its key, so a new feature leaves the keys of all other
//!    shaders as they are.

use crate::job::Job;
use crate::maxwell::ampere_table::{self, AEmit};

/// A part of the translator with its own revision.
#[derive(Clone, Copy, Debug)]
pub struct Feature {
    /// The name in the key (`hmma`).
    pub name: &'static str,
    /// The revision. Bump it for a change that alters the output only of shaders that use the feature.
    pub revision: u32,
    /// True when the shader may use the feature (rule 1).
    pub uses: fn(&Job, &[u8]) -> bool,
}

/// The features of the translator.
///
/// `hmma` 1: an F16 HMMA chain rounds its accumulator to half after every step (`msl/hmma.rs`).
pub const FEATURES: &[Feature] = &[Feature {
    name: "hmma",
    revision: 1,
    uses: uses_hmma,
}];

/// The emitter of every 128-bit word of Ampere code, the header included (an over-approximation of the decoded ops).
fn ampere_emitters(code: &[u8]) -> impl Iterator<Item = AEmit> + '_ {
    let (words, _) = code.as_chunks::<16>();
    words.iter().filter_map(|w| {
        let mut lo = [0u8; 8];
        lo.copy_from_slice(&w[..8]);
        ampere_table::lookup((u64::from_le_bytes(lo) & 0xfff) as usize).map(|e| e.emit)
    })
}

/// Ampere code with an HMMA. Maxwell code has no tensor instructions.
fn uses_hmma(job: &Job, code: &[u8]) -> bool {
    job.arch_sm86 && ampere_emitters(code).any(|e| e == AEmit::AHmma)
}

/// The feature field of a shader's key over `features`: `name=revision` of each feature it uses, `;`-joined, empty
/// when it uses none.
pub fn tags_of(features: &[Feature], job: &Job, code: &[u8]) -> String {
    features
        .iter()
        .filter(|f| (f.uses)(job, code))
        .map(|f| format!("{}={}", f.name, f.revision))
        .collect::<Vec<_>>()
        .join(";")
}

/// [`tags_of`] over [`FEATURES`].
pub fn tags(job: &Job, code: &[u8]) -> String {
    tags_of(FEATURES, job, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `HMMA.16816.F16 R46, R36, R4, RZ` (`tensor_tests.rs`).
    const HMMA: (u64, u64) = (0x0000_0004_242e_723c, 0x041f_f000_0000_08ff);

    fn code(words: &[(u64, u64)]) -> Vec<u8> {
        let mut c = vec![0u8; 0x80];
        for (lo, hi) in words {
            c.extend_from_slice(&lo.to_le_bytes());
            c.extend_from_slice(&hi.to_le_bytes());
        }
        c
    }

    fn job(sm86: bool) -> Job {
        let arch = if sm86 { "sm86" } else { "sm53" };
        let v = nxb_pyjson::Value::obj([
            ("key", nxb_pyjson::Value::str("0123456789abcdef")),
            ("stage", nxb_pyjson::Value::str("cs")),
            ("arch", nxb_pyjson::Value::str(arch)),
        ]);
        Job::from_value(&v).expect("job")
    }

    #[test]
    fn hmma_is_found_in_ampere_code_only() {
        let with = code(&[(0, 0), HMMA]);
        assert_eq!(tags(&job(true), &with), "hmma=1");
        assert_eq!(tags(&job(false), &with), "");
        assert_eq!(tags(&job(true), &code(&[(0, 0)])), "");
    }

    #[test]
    fn a_bump_changes_the_tags_of_users_only() {
        let bumped = [Feature {
            revision: 2,
            ..FEATURES[0]
        }];
        let with = code(&[HMMA]);
        let without = code(&[]);
        assert_ne!(tags_of(&bumped, &job(true), &with), tags(&job(true), &with));
        assert_eq!(
            tags_of(&bumped, &job(true), &without),
            tags(&job(true), &without)
        );
    }
}
