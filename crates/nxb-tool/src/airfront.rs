//! The own front end in process (design-v2 16): `air_front::compile` turns MSL into AIR bitcode
//! without a process, then Apple's back half (`air-opt`, `metallib`) finishes the library.
//!
//! Off by default. It compiles only shaders the verdict file allows (per-shader verdicts of the emitter project's
//! validation, keyed by the SHA-256 of the MSL), every other shader and every reject goes to Apple's `metal -c`.
//! With only `own-identical` allowed the libraries are byte-identical to Apple's.
//!
//! [`RawFrontEnd`] is the raw container tier: `air_front::build_raw` writes the whole library in process (no Apple
//! tool). It takes only shaders in a [`RawAllowList`] (GPU differential test passed), with cache-key id [`raw_id`].

use crate::apple::{Io, MetalTools, StepCost};
use crate::frontend::{FrontEnd, FrontEndError, FrontEndOutput};
use crate::tool::ProcessTool;
use nxb_model::{Digest, sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Shaders the own front end may compile: digests of the MSL whose verdict is allowed.
#[derive(Clone, Debug, Default)]
pub struct Verdicts {
    allowed: BTreeSet<Digest>,
    /// Lines per verdict, for the report.
    pub counts: BTreeMap<String, usize>,
}

fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("\"{name}\": \"");
    let start = line.find(&pat)? + pat.len();
    let end = line[start..].find('"')?;
    Some(&line[start..start + end])
}

impl Verdicts {
    /// Parse the JSONL verdict file (`source_sha256`, `verdict` per line).`allow` lists the verdicts that select the
    /// own front end (for example `own-identical`).
    pub fn parse(text: &str, allow: &[&str]) -> Result<Verdicts, String> {
        let mut v = Verdicts::default();
        for (n, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            // JSONL (the front end's validation output) or TSV `<sha256>\t<verdict>` (the shipped data form)
            let tsv = line
                .split_once('\t')
                .filter(|(h, _)| h.len() == 64 && !line.starts_with('{'));
            let (Some(h), Some(verdict)) = (
                tsv.map(|t| t.0).or_else(|| field(line, "source_sha256")),
                tsv.map(|t| t.1.trim()).or_else(|| field(line, "verdict")),
            ) else {
                return Err(format!("verdict line {}: no source_sha256/verdict", n + 1));
            };
            *v.counts.entry(verdict.to_string()).or_default() += 1;
            if allow.contains(&verdict) {
                v.allowed.insert(
                    Digest::from_hex(h).map_err(|e| format!("verdict line {}: {e}", n + 1))?,
                );
            }
        }
        Ok(v)
    }

    /// Read a verdict file.
    pub fn load(path: &Path, allow: &[&str]) -> Result<Verdicts, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text, allow)
    }

    /// True when the shader with this MSL goes to the own front end.
    pub fn allows(&self, msl: &[u8]) -> bool {
        self.allowed.contains(&sha256(msl))
    }

    /// Number of allowed shaders.
    pub fn len(&self) -> usize {
        self.allowed.len()
    }

    /// True when nothing is allowed.
    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

/// `air_front::id()`: the front end's identity (crate version and output format generation).
pub fn front_id() -> String {
    air_front::id()
}

/// `air_front` in process plus Apple's back half.
#[derive(Debug)]
pub struct AirFrontEnd {
    tools: Arc<MetalTools>,
    air_opt: ProcessTool,
    target: air_front::Target,
    id: String,
    /// Shaders that went through `air-opt`'s text path (two calls).
    pub text_ir: AtomicU64,
}

impl AirFrontEnd {
    /// The own front end for macOS (`ios` false) or iOS, with `air-opt` from the same toolchain as `tools`.
    pub fn new(tools: Arc<MetalTools>, air_opt: ProcessTool, ios: bool) -> Self {
        AirFrontEnd {
            tools,
            air_opt,
            target: if ios {
                air_front::Target::ios()
            } else {
                air_front::Target::macos()
            },
            id: air_front::id(),
            text_ir: AtomicU64::new(0),
        }
    }
}

impl FrontEnd for AirFrontEnd {
    fn id(&self) -> &str {
        &self.id
    }

    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
        let ir = air_front::compile(msl, &self.target).map_err(|r| FrontEndError::Reject(r.0))?;
        if matches!(ir, air_front::Ir::Text(..)) {
            self.text_ir.fetch_add(1, Ordering::Relaxed);
        }
        let mut steps: Vec<StepCost> = Vec::new();
        let mut bytes = ir.bytes().to_vec();
        for args in ir.back_half() {
            let (out, c) = self
                .tools
                .filter(&self.air_opt, args, bytes)
                .map_err(FrontEndError::Tool)?;
            steps.push(c);
            bytes = out;
        }
        let (lib, l) = self
            .tools
            .link(&bytes, Io::Pipes)
            .map_err(FrontEndError::Tool)?;
        steps.push(l);
        Ok(FrontEndOutput {
            metallib: lib,
            frontend: self.id.clone(),
            steps,
        })
    }
}

/// The identity of the raw container tier: `own-raw@` + [`front_id`]. It is the cache-key field of raw libraries.
pub fn raw_id() -> String {
    format!("own-raw@{}", air_front::id())
}

/// The allow list of the raw tier: SHA-256 of the MSL of every shader whose raw library passed the GPU differential
/// test (`gputier.py tiers` with `GPUTIER_RAW=1`). One lower-case hex digest per line, `#` comments.
/// Allow semantics only: a shader not in the list never takes the raw path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawAllowList {
    digests: BTreeSet<Digest>,
}

impl RawAllowList {
    /// Parse the text form. A bad line is an error, so a damaged list turns the tier off instead of guessing.
    pub fn parse(text: &str) -> Result<RawAllowList, String> {
        let mut digests = BTreeSet::new();
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let d = Digest::from_hex(line).map_err(|e| format!("raw list line {}: {e}", n + 1))?;
            digests.insert(d);
        }
        Ok(RawAllowList { digests })
    }

    /// True when the shader with this MSL digest may take the raw path.
    pub fn allows_digest(&self, d: &Digest) -> bool {
        self.digests.contains(d)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.digests.len()
    }

    /// True when empty.
    pub fn is_empty(&self) -> bool {
        self.digests.is_empty()
    }
}

/// The in-process check of a raw library: it parses as a `metallib`, writes back to the same bytes (the hash, size
/// and offset tags are the writer's own), holds a function, and every function has a name and a bitcode blob with
/// a valid wrapper header (`0x0B17C0DE`, offset and size inside the blob, `BC 0xC0DE` at the offset).
pub fn validate_library(lib: &[u8]) -> Result<(), String> {
    use air_front::air_lib;
    let parsed = air_lib::parse(lib)?;
    if parsed.functions.is_empty() {
        return Err("no function".into());
    }
    if air_lib::write(&parsed) != lib {
        return Err("the library does not write back to its own bytes".into());
    }
    for f in &parsed.functions {
        let name = String::from_utf8_lossy(f.name()).into_owned();
        if f.name().is_empty() {
            return Err("a function has no name".into());
        }
        let bc = &f.bitcode;
        let word = |o: usize| {
            bc.get(o..o + 4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        if word(0) != Some(0x0B17_C0DE) {
            return Err(format!("{name}: no bitcode wrapper"));
        }
        let (Some(off), Some(size)) = (word(8), word(12)) else {
            return Err(format!("{name}: short bitcode wrapper"));
        };
        if size == 0 || off < 20 || off.checked_add(size).is_none_or(|e| e > bc.len()) {
            return Err(format!("{name}: bitcode wrapper outside the blob"));
        }
        if bc.get(off..off + 4) != Some(&[b'B', b'C', 0xC0, 0xDE]) {
            return Err(format!("{name}: no bitcode magic"));
        }
    }
    Ok(())
}

/// The raw container path: `air_front::build_raw` in process (front end, per-entry split, mem2reg, DCE, own bitcode
/// and MTLB writer), then [`validate_library`]. No Apple tool and no process runs. The library is not byte-identical to
/// Apple's. A reject, an internal error, a panic or a failed validation is a [`FrontEndError::Reject`]: the caller
/// uses the next tier. A [`RawAllowList`] can narrow the shaders the caller sends here.
#[derive(Debug)]
pub struct RawFrontEnd {
    target: air_front::Target,
    id: String,
}

impl RawFrontEnd {
    /// The raw tier for macOS (`ios` false) or iOS.
    pub fn new(ios: bool) -> Self {
        RawFrontEnd {
            target: if ios {
                air_front::Target::ios()
            } else {
                air_front::Target::macos()
            },
            id: raw_id(),
        }
    }
}

impl FrontEnd for RawFrontEnd {
    fn id(&self) -> &str {
        &self.id
    }

    fn compile(&self, msl: &[u8]) -> Result<FrontEndOutput, FrontEndError> {
        let t = cfg!(not(target_family = "wasm")).then(std::time::Instant::now);
        // A panic in the in-process front end must not end the build: the shader goes to the next tier.
        let res = std::panic::catch_unwind(|| air_front::build_raw(msl, &self.target));
        let lib = match res {
            Ok(Ok(lib)) => lib,
            Ok(Err(Ok(r))) => return Err(FrontEndError::Reject(r.0)),
            Ok(Err(Err(e))) => return Err(FrontEndError::Reject(format!("raw: {e}"))),
            Err(_) => return Err(FrontEndError::Reject("raw: front end panicked".into())),
        };
        validate_library(&lib)
            .map_err(|e| FrontEndError::Reject(format!("raw: invalid library: {e}")))?;
        let wall = t.map_or(std::time::Duration::ZERO, |t| t.elapsed());
        Ok(FrontEndOutput {
            metallib: lib,
            frontend: self.id.clone(),
            // in process: the wall time of the call stands for its CPU time
            steps: vec![StepCost {
                wall,
                cpu: wall,
                max_rss: 0,
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_list_parse_and_lookup() {
        let d = sha256(b"verified shader");
        let list = RawAllowList::parse(&format!("# raw-verified: 1\n{}\n\n", d.to_hex())).unwrap();
        assert_eq!(list.len(), 1);
        assert!(list.allows_digest(&d));
        assert!(!list.allows_digest(&sha256(b"other")));
        assert!(RawAllowList::parse("zz\n").is_err());
        assert!(RawAllowList::parse("").unwrap().is_empty());
    }

    #[test]
    fn raw_front_end_builds_and_rejects() {
        let fe = RawFrontEnd::new(false);
        assert!(fe.id().starts_with("own-raw@air-front@"));
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\nkernel void k(device float* o [[buffer(0)]], uint i [[thread_position_in_grid]]) { o[i] = 1.0; }\n";
        let out = fe.compile(msl).expect("raw library");
        assert_eq!(&out.metallib[..4], b"MTLB");
        validate_library(&out.metallib).expect("valid");
        let mut broken = out.metallib.clone();
        let n = broken.len();
        broken[n - 40] ^= 0x55;
        assert!(validate_library(&broken[..n - 9]).is_err());
        assert!(validate_library(b"MTLB").is_err());
        assert_eq!(out.frontend, fe.id());
        assert!(matches!(
            fe.compile(b"this is not metal {"),
            Err(FrontEndError::Reject(_))
        ));
    }

    #[test]
    fn verdicts_select_allowed() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        let text = format!(
            "{{\"i\": 0, \"source_sha256\": \"{a}\", \"verdict\": \"own-identical\"}}\n{{\"source_sha256\": \"{b}\", \"verdict\": \"apple\"}}\n"
        );
        let v = Verdicts::parse(&text, &["own-identical"]).expect("parses");
        assert_eq!(v.len(), 1);
        assert_eq!(v.counts.get("apple"), Some(&1));
        assert!(Verdicts::parse("{\"x\": 1}", &[]).is_err());
        let t = Verdicts::parse(
            &format!("{a}\town-identical\n{b}\tapple\n"),
            &["own-identical"],
        )
        .expect("tsv");
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn verdicts_match_msl_digest() {
        let msl = b"fragment float4 f() { return 0; }";
        let line = format!(
            "{{\"source_sha256\": \"{}\", \"verdict\": \"own-validated\"}}",
            sha256(msl).to_hex()
        );
        let v = Verdicts::parse(&line, &["own-validated"]).expect("parses");
        assert!(v.allows(msl));
        assert!(!v.allows(b"other"));
    }
}
