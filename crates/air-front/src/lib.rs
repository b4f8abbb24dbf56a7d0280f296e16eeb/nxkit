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
pub const FORMAT: u32 = 23;

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
    fn raw_library_with_thread_array_reference() {
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\n\
                    static float read(thread float (&values)[4], uint i) { return values[i]; }\n\
                    fragment float4 f(float4 p [[position]]) {\n\
                        float values[4]; values[1] = p.x; return float4(read(values, 1u));\n\
                    }\n";
        let lib = build_raw(msl, &Target::macos_for(26)).expect("raw library");
        let parsed = air_lib::parse(&lib).expect("metallib");
        assert_eq!(parsed.functions[0].name(), b"f");
    }

    #[test]
    fn raw_library_with_static_threadgroup_storage_and_barrier() {
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\n\
            static uint read_slot(threadgroup uint (&values)[4], uint index) { return values[index]; }\n\
            kernel void exchange(device uint* out [[buffer(0)]], uint lane [[thread_index_in_threadgroup]]) {\n\
                threadgroup uint values[4];\n\
                values[lane] = lane + 10u;\n\
                threadgroup_barrier(mem_flags::mem_threadgroup);\n\
                out[lane] = read_slot(values, (lane + 1u) & 3u);\n\
            }\n\
            kernel void separate(device uint* out [[buffer(0)]], uint lane [[thread_index_in_threadgroup]]) {\n\
                threadgroup uint values[4]; values[lane] = lane; out[lane] = values[lane];\n\
            }\n";
        let target = Target::macos_for(26);
        let ir =
            air_emit::compile_source(std::str::from_utf8(msl).unwrap(), &target, "-").expect("AIR");
        assert_eq!(
            ir.matches("internal addrspace(3) global [4 x i32] undef, align 4")
                .count(),
            2
        );
        for name in ["exchange", "separate"] {
            assert!(
                ir.contains(&format!("@_ZZ8{name}PU9MTLdevicejjE6values = ")),
                "{ir}"
            );
        }
        assert!(
            ir.contains("getelementptr [4 x i32], [4 x i32] addrspace(3)*"),
            "{ir}"
        );
        assert!(
            ir.contains("call void @air.wg.barrier(i32 2, i32 5, i32 1)"),
            "{ir}"
        );
        let module = air_bc::parse::parse(&ir).expect("AIR module");
        let split = air_bc::split::split(&module).expect("entry split");
        assert_eq!(split.len(), 2);
        for entry in &split {
            assert_eq!(
                entry.module.globals.iter().filter(|g| g.space == 3).count(),
                1
            );
        }
        let lib = build_raw(msl, &target).expect("raw library");
        let parsed = air_lib::parse(&lib).expect("metallib");
        assert_eq!(parsed.functions[0].name(), b"exchange");
        assert_eq!(parsed.functions[1].name(), b"separate");
    }

    fn assert_same_shader(source: &str, reference: &str, target: &Target) {
        let got = compile(source.as_bytes(), target).expect("source bitcode");
        assert!(matches!(got, Ir::Bitcode(_)), "{source}");
        assert_eq!(
            got,
            compile(reference.as_bytes(), target).expect("reference bitcode"),
            "{source}"
        );
        assert_eq!(
            build_raw(source.as_bytes(), target).expect("source metallib"),
            build_raw(reference.as_bytes(), target).expect("reference metallib"),
            "{source}"
        );
    }

    #[test]
    fn partial_constant_arrays_match_explicit_zeroes() {
        let target = Target::macos_for(27);
        for (ty, len, prefix, zero) in [
            ("uint", 4, &["9u", "0u"][..], "0u"),
            ("float", 3, &["-0.0", "1.5"][..], "0.0"),
            ("bool", 3, &["true"][..], "false"),
            ("uint", 11, &["5u", "0u"][..], "0u"),
            ("float", 3, &[][..], "0.0"),
            ("uint2", 3, &["uint2(2u, 7u)"][..], "uint2(0u)"),
            ("float3", 3, &["float3(-0.0, 0.0, 0.0)"][..], "float3(0.0)"),
            ("float3", 10, &["float3(2.0)"][..], "float3(0.0)"),
            ("uint4", 3, &[][..], "uint4(0u)"),
        ] {
            let shader = |values: &[&str]| {
                format!(
                "#include <metal_stdlib>\nusing namespace metal;\n\
                 constant {ty} values[{len}] = {{{}}};\n\
                 kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ out[i] = values[i]; }}\n",
                values.join(", ")
            )
            };
            let partial = shader(prefix);
            let mut padded = prefix.to_vec();
            padded.resize(len, zero);
            let full = shader(&padded);
            assert_same_shader(&partial, &full, &target);
        }
    }

    #[test]
    fn constant_array_casts_fall_back_when_the_legacy_fold_is_not_exact() {
        let target = Target::macos_for(27);
        let source = |ty: &str, extent: &str, value: &str| {
            format!(
                "#include <metal_stdlib>\nusing namespace metal;\n\
                 constant {ty} values[{extent}] = {{{value}}};\n\
                 kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ out[i] = values[i]; }}\n"
            )
        };
        for (ty, extent, value, expected) in [
            ("uint", "3", "uint(ushort(65537u))", "1u"),
            ("uint", "", "uint(ushort(65537u))", "1u"),
            ("bool", "3", "bool(0.5f)", "true"),
            ("float", "3", "float(uint(-1))", "4294967296.0"),
            ("float", "3", "float(half(1.0001f))", "1.0"),
        ] {
            let bad = source(ty, extent, value);
            let good = source(ty, extent, expected);
            assert!(
                matches!(
                    compile(bad.as_bytes(), &target),
                    Err(Reject(key)) if key.contains("const-array:non-constant-init")
                ),
                "{value}"
            );
            assert!(
                matches!(
                    build_raw(bad.as_bytes(), &target),
                    Err(Ok(Reject(key))) if key.contains("const-array:non-constant-init")
                ),
                "{value}"
            );
            assert!(matches!(
                compile(good.as_bytes(), &target),
                Ok(Ir::Bitcode(_))
            ));
            assert!(build_raw(good.as_bytes(), &target).is_ok());
        }
    }

    #[test]
    fn partial_const_array_rejects_unsupported_initializers() {
        let target = Target::macos_for(27);
        for (decl, reason) in [
            (
                "constant uint values[1] = {1u, 2u};",
                "const-array:excess-init",
            ),
            (
                "constant uint values[2][2] = {};",
                "const-array:partial-element-type",
            ),
            (
                "struct Value { uint x; }; constant Value values[2] = {};",
                "const-array:partial-element-type",
            ),
            (
                "constant float3 values[1048577] = {};",
                "const-array:partial-size",
            ),
            (
                "constant ulong values[4294967295] = {};",
                "const-array:partial-size",
            ),
        ] {
            let source = format!("#include <metal_stdlib>\nusing namespace metal;\n{decl}\n");
            let error = air_emit::compile_source(&source, &target, "-").unwrap_err();
            assert!(error.key().contains(reason), "{error:?}");
        }
    }

    #[test]
    fn partial_copy_list_requires_a_comma_between_elements() {
        let source = "#include <metal_stdlib>\nusing namespace metal;\n\
            constant uint values[3] = {1u 2u};\n\
            kernel void k(device uint* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) { out[i] = values[i]; }\n";
        let error = air_emit::compile_source(source, &Target::macos_for(27), "-").unwrap_err();
        assert!(
            error.key().contains("const-array:missing-comma"),
            "{error:?}"
        );
    }

    #[test]
    fn inferred_constant_array_bounds_match_explicit_bounds() {
        let target = Target::macos_for(27);
        for (ty, len, values) in [
            ("uint", "3", "7u, 0u, 11u,"),
            ("bool", "1", "true"),
            ("float3", "2", "float3(-0.0, 5.0, 0.0), float3(0.0)"),
        ] {
            let shader = |bound: &str| {
                format!(
                "#include <metal_stdlib>\nusing namespace metal;\n\
                 constant {ty} values[{bound}] = {{ {values} }};\n\
                 kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ out[i] = values[i]; }}\n"
            )
            };
            let inferred = shader("");
            let explicit = shader(len);
            assert_same_shader(&inferred, &explicit, &target);
        }
    }

    #[test]
    fn inferred_constant_arrays_reject_unsupported_forms() {
        let target = Target::macos_for(27);
        for (decl, reason) in [
            ("constant uint values[] = {};", "const-array:unsized-empty"),
            (
                "constant uint values[][2] = {};",
                "const-array:unsized-nested",
            ),
            (
                "struct S { uint x; }; constant S values[] = {};",
                "const-array:unsized-element-type",
            ),
            (
                "constant uint values[] = {1u 2u};",
                "const-array:unsized-missing-comma",
            ),
        ] {
            let source = format!("#include <metal_stdlib>\nusing namespace metal;\n{decl}\n");
            let error = air_emit::compile_source(&source, &target, "-").unwrap_err();
            assert!(error.key().contains(reason), "{error:?}");
        }
    }

    #[test]
    fn direct_list_constant_arrays_match_copy_list_initializers() {
        let target = Target::macos_for(27);
        for (ty, bound, list) in [
            ("uint", "4", "1u"),
            ("uint", "", "1u, 2u,"),
            ("float3", "3", "float3(-0.0, 1.0, 0.0)"),
        ] {
            let shader = |equals: &str| {
                format!(
                "#include <metal_stdlib>\nusing namespace metal;\n\
                 constant {ty} values[{bound}]{equals}{{{list}}};\n\
                 kernel void k(device {ty}* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {{ out[i] = values[i]; }}\n"
            )
            };
            let direct = shader("");
            let copy = shader(" = ");
            assert_same_shader(&direct, &copy, &target);
        }
    }

    #[test]
    fn direct_lists_do_not_admit_unrelated_constants_or_invalid_arrays() {
        let target = Target::macos_for(27);
        for decl in [
            "constant uint value{1u};",
            "constant uint values[2][2]{};",
            "constant uint values[2]{1u 2u};",
            "constant uint values[]{ };",
        ] {
            let source = format!("#include <metal_stdlib>\nusing namespace metal;\n{decl}\n");
            assert!(
                air_emit::compile_source(&source, &target, "-").is_err(),
                "{decl}"
            );
        }
    }

    #[test]
    fn unsigned_binary_masks_match_hex_and_decimal_masks() {
        let target = Target::macos_for(27);
        let binary = "#include <metal_stdlib>\nusing namespace metal;\n\
            constant uint masks[2] = {0B101010U, 0b10000000000000000000000000000000u};\n\
            kernel void k(device uint* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {\n\
                out[i] = masks[i & 0b1u] ^ (i & 0B00101u);\n\
            }\n";
        let reference = binary
            .replace("0B101010U", "0x2au")
            .replace("0b10000000000000000000000000000000u", "0x80000000u")
            .replace("0b1u", "1u")
            .replace("0B00101u", "5u");
        assert_same_shader(binary, &reference, &target);
    }

    #[test]
    fn binary_literals_reject_unsupported_suffix_and_extent() {
        let target = Target::macos_for(27);
        for literal in [
            "0b",
            "0b102u",
            "0b1u2u",
            "0b11",
            "0b11ul",
            "0b100000000000000000000000000000000u",
        ] {
            let source = format!("#include <metal_stdlib>\nusing namespace metal;\nconstant uint a[2] = {{{literal}}};\n");
            assert!(
                air_emit::compile_source(&source, &target, "-").is_err(),
                "{literal}"
            );
        }
    }

    #[test]
    fn binary_digit_separators_match_unseparated_masks() {
        let target = Target::macos_for(27);
        let msl = "#include <metal_stdlib>\nusing namespace metal;\n\
            constant uint masks[]{0B10'10U, 0b1111'1111'1111'1111'1111'1111'1111'1111u};\n\
            kernel void k(device uint* out [[buffer(0)]], uint i [[thread_index_in_threadgroup]]) {\n\
                out[i] = masks[i & 1u] ^ (i & 0b1'0'1u);\n\
            }\n";
        let reference = msl.replace('\'', "");
        assert_same_shader(msl, &reference, &target);
    }

    #[test]
    fn binary_digit_separators_require_surrounding_binary_digits() {
        let target = Target::macos_for(27);
        for literal in [
            "0b'1u",
            "0b1''0u",
            "0b1'u",
            "0b1u'0",
            "0b1'2u",
            "0b1'0ul",
            "0b1'00000000000000000000000000000000u",
        ] {
            let source = format!("#include <metal_stdlib>\nusing namespace metal;\nconstant uint a[2] = {{{literal}}};\n");
            assert!(
                air_emit::compile_source(&source, &target, "-").is_err(),
                "{literal}"
            );
        }
    }

    #[test]
    fn raw_library_for_macos_26() {
        // macOS 26 refuses Metal 4.1: the library says macOS 26.0, AIR 2.8 and Metal 4.0 (issue 21)
        let msl = b"#include <metal_stdlib>\nusing namespace metal;\nfragment float4 f(constant float4* c [[buffer(0)]], float4 p [[position]]) { return p * c[0]; }\n";
        let lib = build_raw(msl, &Target::macos_for(26)).expect("raw library");
        let parsed = air_lib::parse(&lib).expect("metallib");
        assert_eq!(
            (parsed.header.os, parsed.header.os_version),
            (0x81, (26, 0))
        );
        let m = build_raw(msl, &Target::macos_for(27)).expect("raw library");
        assert_eq!(
            air_lib::parse(&m).expect("metallib").header.os_version,
            (27, 2)
        );
        assert_ne!(lib, m);
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
