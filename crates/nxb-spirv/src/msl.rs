//! The MSL writer step: naga's MSL back end with the slot map, then the entry point
//! name and IEEE min, max and clamp.

use naga::back::msl::{self, EntryPointResourceMap, Options, PipelineOptions};
use naga::{Module, valid::ModuleInfo};

use crate::slots::Slots;

/// Float overloads of min, max and clamp with IEEE NaN rules (`fmin`, `fmax`), which the Switch's GPU follows: min(NaN, x) is x.
/// Metal compiles `metal::min` of a float under fast math, where the result for a NaN operand is undefined. Shaders rely on
/// the IEEE result, for example Super Mario Galaxy 2 clamps a NaN to hide it. Other types keep `metal::`. The helpers are `static`, as every shader of a library has them.
pub(crate) const PRECISE: &str = "
template <typename A, typename B> static inline auto nxb_min(A a, B b) { return metal::min(a, b); }
template <typename A, typename B> static inline auto nxb_max(A a, B b) { return metal::max(a, b); }
template <typename A, typename B, typename C> static inline auto nxb_clamp(A a, B b, C c) { return metal::clamp(a, b, c); }
#define NXB_PRECISE(T) \\
static inline T nxb_min(T a, T b) { return metal::precise::min(a, b); } \\
static inline T nxb_max(T a, T b) { return metal::precise::max(a, b); } \\
static inline T nxb_clamp(T a, T b, T c) { return metal::precise::clamp(a, b, c); }
NXB_PRECISE(float)
NXB_PRECISE(metal::float2)
NXB_PRECISE(metal::float3)
NXB_PRECISE(metal::float4)
#undef NXB_PRECISE
";

/// Writes the single entry point of `m` as MSL 2.3 with the slots of `slots`, and names its Metal function `name`. The entry
/// point of `m` must already carry `name`.
pub(crate) fn write(
    m: &Module,
    info: &ModuleInfo,
    slots: &Slots,
    name: &str,
) -> Result<String, String> {
    let mut map = EntryPointResourceMap::new();
    map.insert(name.to_string(), slots.binding_map());
    let options = Options {
        lang_version: (2, 3),
        per_entry_point_map: map,
        fake_missing_bindings: false,
        ..Options::default()
    };
    let (src, ti) = msl::write_string(m, info, &options, &PipelineOptions::default())
        .map_err(|e| format!("MSL writer: {e}"))?;
    let emitted = match ti.entry_point_names.first() {
        Some(Ok(n)) => n.clone(),
        Some(Err(e)) => return Err(format!("MSL writer: {e}")),
        None => return Err("MSL writer: no entry point".into()),
    };
    let decl = format!(" {emitted}(");
    if src.matches(&decl).count() != 1 {
        return Err(format!(
            "the entry point {emitted} is not declared once in the MSL"
        ));
    }
    let src = src
        .replace(&decl, &format!(" {name}("))
        .replace("metal::min(", "nxb_min(")
        .replace("metal::max(", "nxb_max(")
        .replace("metal::clamp(", "nxb_clamp(");
    Ok(with_helpers(&internal(&src)))
}

/// Makes the functions other than the entry point `static`. A library links all shaders of a game, and two shaders with a
/// helper of one name (naga names them after the SPIR-V's names, `main_` among them) would collide.
pub(crate) fn internal(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 1024);
    for l in src.split_inclusive('\n') {
        let head = l.split(['(', ' ']).next().unwrap_or("");
        // naga writes a function head as `<type> <name>(` with the arguments on the lines after, or its helpers (`naga_f2i32`)
        // as `<type> <name>(<arguments>) {`
        let t = l.trim_end();
        let def = (t.ends_with('(') || t.contains('(') && t.ends_with(") {"))
            && l.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && ![
                "fragment", "vertex", "kernel", "struct", "template", "typedef", "using", "static",
                "constant", "}",
            ]
            .contains(&head);
        if def {
            out.push_str("static ");
        }
        out.push_str(l);
    }
    out
}

/// Puts the `PRECISE` helpers after the last line of the MSL preamble (`#include` and `using`).
fn with_helpers(src: &str) -> String {
    let mut at = 0;
    for (k, l) in src.split_inclusive('\n').scan(0, |o, l| {
        let k = *o;
        *o += l.len();
        Some((k, l))
    }) {
        if l.starts_with("#include") || l.starts_with("using ") {
            at = k + l.len();
        } else if !l.trim().is_empty() && at > 0 {
            break;
        }
    }
    format!("{}{PRECISE}{}", &src[..at], &src[at..])
}

/// The dual-source variant of a fragment shader's MSL: function `name` becomes `name_d` and the output at location 1 becomes
/// the second blend source of attachment 0. None when the shader has no output at location 1.
pub(crate) fn dual_source(src: &str, name: &str) -> Option<String> {
    src.contains("[[color(1)]]").then(|| {
        src.replace(&format!(" {name}("), &format!(" {name}_d("))
            .replace("[[color(1)]]", "[[color(0), index(1)]]")
    })
}
