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
///
/// `meshpos` 1: the position of a mesh shader is `invariant` (`msl/mesh.rs`), so two mesh shaders give equal depths.
///
/// `blendz` 1: a pixel shader writes RGB 0 to a float colour whose blend factor is 0 (`msl/variant/pixel.rs`).
///
/// `surface` 1: the one coordinate of a 1D or buffer image load, store or atomic is a `uint` (`msl/inst_img.rs`). 2: an
/// atomic on a texture buffer goes to its texels as device words (`img_words`, because Metal has no texture buffer
/// atomics), and the copy of a compare-and-swap value no longer takes the name of the coordinate.
///
/// `fetch1d` 1: the one coordinate of a buffer or 1D texel fetch is a `uint` (`msl/inst_tex.rs`).
pub const FEATURES: &[Feature] = &[
    Feature {
        name: "hmma",
        revision: 1,
        uses: uses_hmma,
    },
    Feature {
        name: "meshpos",
        revision: 1,
        uses: uses_mesh_position,
    },
    Feature {
        name: "blendz",
        revision: 1,
        uses: uses_blend_word,
    },
    Feature {
        name: "surface",
        revision: 2,
        uses: uses_surface,
    },
    Feature {
        name: "fetch1d",
        revision: 1,
        uses: uses_fetch,
    },
];

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

/// A mesh shader (`ms`): the only stage whose wrapper declares the position the feature changes.
fn uses_mesh_position(job: &Job, _code: &[u8]) -> bool {
    job.mesh_stage.as_ref().is_some_and(|m| m.kind == "ms")
}

/// A pixel shader: every one reads the blend word, whether it writes a float colour or not.
fn uses_blend_word(job: &Job, _code: &[u8]) -> bool {
    job.stage == "ps" && job.mesh_stage.is_none()
}

/// Code with a texel fetch (`TLD`, `TLD.B`, `TLDS`), Maxwell or Ampere, looked at as [`uses_surface`] does.
fn uses_fetch(job: &Job, code: &[u8]) -> bool {
    use nxb_maxwell::names::InstName as N;
    uses_inst(job, code, |n| matches!(n, N::Tld | N::TldB | N::Tlds))
}

/// Code with a surface instruction (an image load, store, reduction or atomic). Every 64-bit word of Maxwell code and
/// every 128-bit word of Ampere code is looked at, the header and the scheduling words included (an over-approximation).
fn uses_surface(job: &Job, code: &[u8]) -> bool {
    use nxb_maxwell::names::InstName as N;
    uses_inst(job, code, |n| {
        matches!(
            n,
            N::Suatom
                | N::SuatomB
                | N::SuatomB2
                | N::SuatomCas
                | N::SuatomCasB
                | N::Suld
                | N::SuldB
                | N::SuldD
                | N::SuldDB
                | N::Sured
                | N::SuredB
                | N::Sust
                | N::SustB
                | N::SustD
                | N::SustDB
        )
    })
}

/// Whether `code` holds an instruction that `wanted` names. Every 64-bit word of Maxwell code and every 128-bit word
/// of Ampere code is looked at, the header and the scheduling words included (an over-approximation).
fn uses_inst(
    job: &Job,
    code: &[u8],
    wanted: impl Fn(nxb_maxwell::names::InstName) -> bool,
) -> bool {
    if job.arch_sm86 {
        let (words, _) = code.as_chunks::<16>();
        return words.iter().any(|w| {
            let mut lo = [0u8; 8];
            lo.copy_from_slice(&w[..8]);
            ampere_table::lookup((u64::from_le_bytes(lo) & 0xfff) as usize)
                .is_some_and(|e| wanted(e.name))
        });
    }
    let (words, _) = code.as_chunks::<8>();
    words
        .iter()
        .any(|w| wanted(nxb_maxwell::table::get_op(0, u64::from_le_bytes(*w)).name))
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
    fn surface_instructions_are_found_in_maxwell_code() {
        // `SUST.B` (opcode 1110 1011 0010 0x at bits 63..50) among plain words.
        let sust: u64 = 0b11_1010_1100_1000 << 50;
        let mut with = vec![0u8; 0x50];
        with.extend_from_slice(&0u64.to_le_bytes());
        with.extend_from_slice(&sust.to_le_bytes());
        assert_eq!(tags(&job(false), &with), "surface=2");
        assert_eq!(tags(&job(false), &[0u8; 0x60]), "");
    }

    #[test]
    fn texel_fetches_are_found_in_maxwell_code() {
        // `TLD.B` (opcode 1101 1101 at bits 63..56) among plain words.
        let tld_b: u64 = 0b1101_1101 << 56;
        let mut with = vec![0u8; 0x50];
        with.extend_from_slice(&tld_b.to_le_bytes());
        assert_eq!(tags(&job(false), &with), "fetch1d=1");
    }

    #[test]
    fn hmma_is_found_in_ampere_code_only() {
        let with = code(&[(0, 0), HMMA]);
        assert_eq!(tags(&job(true), &with), "hmma=1");
        assert_eq!(tags(&job(false), &with), "");
        assert_eq!(tags(&job(true), &code(&[(0, 0)])), "");
    }

    #[test]
    fn a_mesh_shader_uses_the_mesh_position_feature() {
        let mesh = |kind: &str| {
            let mut j = job(true);
            j.mesh_stage = Some(crate::job::MeshStage {
                kind: kind.into(),
                ..Default::default()
            });
            j
        };
        assert_eq!(tags(&mesh("ms"), &code(&[])), "meshpos=1");
        assert_eq!(tags(&mesh("ms"), &code(&[HMMA])), "hmma=1;meshpos=1");
        assert_eq!(tags(&mesh("ts"), &code(&[])), "");
        assert_eq!(tags(&job(true), &code(&[])), "");
    }

    #[test]
    fn blendz_is_in_the_key_of_pixel_shaders_only() {
        let stage = |name: &str| {
            let mut j = job(true);
            j.stage = name.into();
            j
        };
        assert_eq!(tags(&stage("ps"), &code(&[])), "blendz=1");
        assert_eq!(tags(&stage("ps"), &code(&[HMMA])), "hmma=1;blendz=1");
        for other in ["vs", "cs", "gs"] {
            assert_eq!(tags(&stage(other), &code(&[])), "", "{other}");
        }
        let mut mesh = stage("cs");
        mesh.mesh_stage = Some(crate::job::MeshStage {
            kind: "ms".into(),
            ..Default::default()
        });
        assert_eq!(tags(&mesh, &code(&[])), "meshpos=1");
        // A mesh job is not a pixel shader, whatever its stage name.
        let mut odd = stage("ps");
        odd.mesh_stage = mesh.mesh_stage.clone();
        assert_eq!(tags(&odd, &code(&[])), "meshpos=1");
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
