//! In-process front end for library callers: MSL bytes in, AIR bitcode out, with no `airc`
//! process. The caller runs Apple's back half itself:
//!
//! - bitcode: `air-opt -O1 --discard-value-names - -o -` piped into `metallib - -o OUT`
//!   ([`BACK_HALF_BITCODE`]);
//! - text fallback (a construct the bitcode writer does not cover): `air-opt -O1` then
//!   `air-opt --discard-value-names` then `metallib` ([`BACK_HALF_TEXT`]).
//!
//! Both give the same library bytes. A [`Reject`] means the front end declines the shader and
//! the caller's Apple fallback (`metal -c`) takes it. The function is pure and thread safe: no
//! files, no environment, no global state.

#![deny(missing_docs)]

pub use air_emit::Target;
/// The metallib container (parse a library, read its functions and tags).
pub use air_lib;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Back-half `air-opt` arguments for [`Ir::Bitcode`] (one call, names dropped on read).
pub const BACK_HALF_BITCODE: &[&[&str]] = &[&["-O1", "--discard-value-names", "-", "-o", "-"]];

/// Back-half `air-opt` calls for [`Ir::Text`] (text IR cannot be read name-free, so two calls).
pub const BACK_HALF_TEXT: &[&[&str]] = &[
    &["-O1", "-", "-o", "-"],
    &["--discard-value-names", "-", "-o", "-"],
];

/// The front end's identity for cache keys and TOOL_FP: [`ID_BASE`] plus the emitter's
/// output-format generation. Bump [`FORMAT`] whenever emitted IR changes for any input.
///
/// The crate version is not part of it: a release changes no emitted IR, and with the version in the id every library
/// of every game compiled again after a release (`docs/early-cutoff.md`).
pub fn id() -> String {
    format!("air-front@{ID_BASE}+f{FORMAT}")
}

/// The crate version at which [`id`] stopped following the crate version. It is fixed, so the libraries in the stores
/// and the verdict folders named by the id stay valid.
pub const ID_BASE: &str = "0.10.0";

/// Generation of the emitted IR.
pub const FORMAT: u32 = 10;

/// The module handed to the back half.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ir {
    /// LLVM 14 era typed-pointer bitcode, value names absent.
    Bitcode(Vec<u8>),
    /// Text IR, when the bitcode writer does not cover a construct (the reason is kept).
    Text(String, String),
}

impl Ir {
    /// The `air-opt` calls to run on this module, in order, before `metallib - -o OUT`.
    pub fn back_half(&self) -> &'static [&'static [&'static str]] {
        match self {
            Ir::Bitcode(_) => BACK_HALF_BITCODE,
            Ir::Text(..) => BACK_HALF_TEXT,
        }
    }

    /// Bytes to write into the first `air-opt` call's stdin.
    pub fn bytes(&self) -> &[u8] {
        match self {
            Ir::Bitcode(b) => b,
            Ir::Text(t, _) => t.as_bytes(),
        }
    }
}

/// The front end declines this shader, the string is a stable key (`parse:...`, `lower:...`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reject(pub String);

impl std::fmt::Display for Reject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "front end rejected: {}", self.0)
    }
}

impl std::error::Error for Reject {}

/// Compiles MSL bytes (as `metal -c` would read them) to the module for the back half.
pub fn compile(msl: &[u8], target: &Target) -> Result<Ir, Reject> {
    let text = match std::str::from_utf8(msl) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(msl).into_owned(),
    };
    let src = msl_lex::normalize_newlines(text);
    let ir = air_emit::compile_source(&src, target, "-").map_err(|e| Reject(e.key()))?;
    Ok(match air_bc::to_bitcode(&ir) {
        Ok(b) => Ir::Bitcode(b),
        Err(why) => Ir::Text(ir, why),
    })
}

/// Apple's back-half tools, found once per process (`xcrun -find`), with the `SDKROOT` they get.
#[derive(Clone, Debug)]
pub struct Tools {
    /// `air-opt`.
    pub air_opt: PathBuf,
    /// `metallib`.
    pub metallib: PathBuf,
    /// `SDKROOT` for both.
    pub sdkroot: PathBuf,
}

impl Tools {
    /// Finds the tools of an SDK (`macosx` or `iphoneos`).
    pub fn find(sdk: &str) -> Result<Tools, String> {
        let x = |args: &[&str]| -> Result<PathBuf, String> {
            let o = Command::new("xcrun")
                .args(args)
                .output()
                .map_err(|e| format!("xcrun: {e}"))?;
            if !o.status.success() {
                return Err(format!("xcrun {args:?} failed"));
            }
            Ok(PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        };
        Ok(Tools {
            air_opt: x(&["-sdk", sdk, "-find", "air-opt"])?,
            metallib: x(&["-sdk", sdk, "-find", "metallib"])?,
            sdkroot: x(&["-sdk", sdk, "--show-sdk-path"])?,
        })
    }

    /// Runs the back half on `ir`: the `air-opt` calls of [`Ir::back_half`] piped into
    /// `metallib - -o -`, returns the library bytes. Each tool gets only `SDKROOT` and a minimal `PATH`.
    pub fn back_half(&self, ir: &Ir) -> Result<Vec<u8>, String> {
        let spawn = |exe: &PathBuf, args: &[&str], stdin: Stdio| {
            Command::new(exe)
                .args(args)
                .env_clear()
                .env("SDKROOT", &self.sdkroot)
                .env("PATH", "/usr/bin:/bin")
                .stdin(stdin)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("{}: {e}", exe.display()))
        };
        let mut chain = Vec::new();
        let mut first = spawn(&self.air_opt, ir.back_half()[0], Stdio::piped())?;
        let feed = first.stdin.take();
        let mut prev = first.stdout.take().ok_or("no stdout")?;
        chain.push(first);
        for args in &ir.back_half()[1..] {
            let mut c = spawn(&self.air_opt, args, Stdio::from(prev))?;
            prev = c.stdout.take().ok_or("no stdout")?;
            chain.push(c);
        }
        let mut lib = spawn(&self.metallib, &["-", "-o", "-"], Stdio::from(prev))?;
        let mut out_pipe = lib.stdout.take().ok_or("no stdout")?;
        let bytes = ir.bytes().to_vec();
        let writer = feed.map(|mut w| std::thread::spawn(move || w.write_all(&bytes)));
        let mut out = Vec::new();
        out_pipe.read_to_end(&mut out).map_err(|e| e.to_string())?;
        if let Some(w) = writer {
            w.join()
                .map_err(|_| "writer panicked".to_string())?
                .map_err(|e| e.to_string())?;
        }
        chain.push(lib);
        for mut c in chain {
            if !c.wait().map_err(|e| e.to_string())?.success() {
                return Err("back-half tool failed".into());
            }
        }
        Ok(out)
    }
}

/// One function of a raw library: its tags as `metallib` would compute them.
fn raw_function(e: &air_bc::split::Entry, vers: [u16; 4]) -> Result<air_lib::Function, String> {
    let mut bc = air_bc::writer::write(&e.module)?;
    // blobs are 16-byte aligned in the bitcode section, MDSZ counts the padding like Apple's blobs
    while bc.len() % 16 != 0 {
        bc.push(0);
    }
    let mut f = air_lib::Function::new(&e.name, e.stage as u8, vers, bc);
    if !e.constants.is_empty() {
        let mut v = (e.constants.len() as u16).to_le_bytes().to_vec();
        for c in &e.constants {
            v.extend_from_slice(c.name.as_bytes());
            v.push(0);
            v.push(c.data_type);
            v.extend_from_slice(&c.index.to_le_bytes());
            v.push(c.required as u8);
        }
        f.public.push((*b"CNST", v));
    }
    if !e.attributes.is_empty() {
        let n = (e.attributes.len() as u16).to_le_bytes();
        let mut a = n.to_vec();
        let mut t = n.to_vec();
        for (name, idx, ty) in &e.attributes {
            a.extend_from_slice(name.as_bytes());
            a.push(0);
            a.extend_from_slice(&idx.to_le_bytes());
            t.push(*ty);
        }
        f.public.push((*b"VATT", a));
        f.public.push((*b"VATY", t));
    }
    Ok(f)
}

/// MSL bytes to a library with no Apple tool at all (the raw container path): the front end's module split per
/// entry point, function constants pruned per entry, our bitcode, our MTLB container. Not byte-identical to Apple's
/// libraries, the gates are air-validate and the GPU differential test.
pub fn build_raw(msl: &[u8], target: &Target) -> Result<Vec<u8>, Result<Reject, String>> {
    let text = match std::str::from_utf8(msl) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(msl).into_owned(),
    };
    let src = msl_lex::normalize_newlines(text);
    let ir = air_emit::compile_source(&src, target, "-").map_err(|e| Ok(Reject(e.key())))?;
    let mut m = air_bc::parse::parse(&ir).map_err(Err)?;
    if std::env::var_os("AIR_FRONT_NO_OPT").is_none() {
        air_bc::opt::optimize(&mut m);
    }
    raw_library(&m, target).map_err(Err)
}

/// The raw library of a parsed module.
pub fn raw_library(m: &air_bc::ast::Module, target: &Target) -> Result<Vec<u8>, String> {
    let header = air_lib::Header::from_triple(&m.triple).ok_or("unknown triple")?;
    let vers = [
        target.air_version.0 as u16,
        target.air_version.1 as u16,
        target.language.0 as u16,
        target.language.1 as u16,
    ];
    let mut functions = Vec::new();
    for e in air_bc::split::split(m)? {
        functions.push(raw_function(&e, vers)?);
    }
    Ok(air_lib::write(&air_lib::Library {
        header,
        functions,
        extension: Vec::new(),
    }))
}

/// MSL bytes to library bytes: [`compile`] in-process, then Apple's back half through pipes.
/// `Err(Ok(reject))` = the front end declines (use Apple's `metal -c`).`Err(Err(msg))` = a tool failed.
pub fn build(
    msl: &[u8],
    target: &Target,
    tools: &Tools,
) -> Result<Vec<u8>, Result<Reject, String>> {
    let ir = compile(msl, target).map_err(Ok)?;
    tools.back_half(&ir).map_err(Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragment_to_bitcode() {
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\nfragment float4 f(float4 p [[position]]) { return p + 1.0; }\n";
        let ir = compile(msl, &Target::macos()).expect("lowers");
        assert!(matches!(ir, Ir::Bitcode(_)));
        assert_eq!(ir.back_half().len(), 1);
    }

    #[test]
    fn raw_library_with_code_after_return() {
        // the second return is an unreachable block that stores to the promoted return slot
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\nstatic int pick(int a, float b)\n{\n    float s = b * 2.0;\n    if (s > 1.0)\n    {\n        return a;\n    }\n    return int(s);\n    return 0;\n}\nfragment float4 f(float4 p [[position]])\n{\n    return float4(float(pick(int(p.x), p.y)));\n}\n";
        let lib = build_raw(msl, &Target::macos()).expect("raw library");
        let parsed = air_lib::parse(&lib).expect("metallib");
        assert_eq!(parsed.functions[0].name(), b"f");
    }

    #[test]
    fn rejects_garbage() {
        assert!(compile(b"this is not msl {", &Target::macos()).is_err());
    }

    #[test]
    #[ignore = "needs Xcode's Metal toolchain"]
    fn builds_library_with_apple_back_half() {
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\nfragment float4 f(float4 p [[position]]) { return p + 1.0; }\n";
        let tools = Tools::find("macosx").expect("xcrun");
        let lib = build(msl, &Target::macos(), &tools).expect("builds");
        let parsed = air_lib::parse(&lib).expect("metallib");
        assert_eq!(parsed.functions[0].name(), b"f");
        assert_eq!(air_lib::write(&parsed), lib);
    }

    #[test]
    fn id_names_format() {
        assert!(id().ends_with(&format!("+f{FORMAT}")));
    }

    #[test]
    fn id_does_not_follow_the_crate_version() {
        // the id of the 0.10.0 stores: a release keeps every library's key
        assert_eq!(id(), format!("air-front@0.10.0+f{FORMAT}"));
    }
}
