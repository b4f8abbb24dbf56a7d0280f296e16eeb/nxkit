//! The alpha test and logic operation entries of a pixel shader. The stage step cuts them into `_bx`.
//!
//! Each entry is `<entry>_a` or `<entry>_l` and calls `dm_body_<entry>`, the function with the shader's own result.
//! The host binds the alpha state at fragment buffer 30 and the logic state at fragment buffer 29. The logic entry
//! reads the colour attachments through function constant parameters (`dm_lfb<n>`, function constant 200 + n).

use super::pixel::Ent;
use crate::msl::entry::{Param, params_decl};
use std::collections::BTreeSet;

/// Fragment buffer of the alpha state.
pub const ALPHA_BUFFER: u32 = 30;
/// Fragment buffer of the logic state.
pub const LOGIC_BUFFER: u32 = 29;
/// First function constant of the logic entries.
pub const LOGIC_FC: u32 = 200;

/// The alpha helper.
pub const ALPHA_HELPER: &str = "
struct dm_alpha_state { uint func; float ref; };   // NVN_ALPHA_FUNC_* and the reference (dmsh_alpha.py)
static inline bool dm_alpha_pass(float a, constant dm_alpha_state& s)
{
    switch (s.func)
    {
        case 1u: return false;
        case 2u: return a < s.ref;
        case 3u: return a == s.ref;
        case 4u: return a <= s.ref;
        case 5u: return a > s.ref;
        case 6u: return a != s.ref;
        case 7u: return a >= s.ref;
        default: return true;
    }
}
";

/// The blend word helper: true when the source RGB factor of a target is exactly 0 for this pixel.
///
/// `k` is the kind of the target in the blend word (`blendz`): 1 `ZERO`, 2 `SRC_ALPHA`, 3 `ONE_MINUS_SRC_ALPHA`,
/// 4 `SRC1_ALPHA`, 5 `ONE_MINUS_SRC1_ALPHA`. The tests are on integer bits, so fast math cannot fold them away.
pub const BLENDZ_HELPER: &str = "
static inline __attribute__((always_inline)) bool dm_bz_zero(uint k, float a, float a1)
{
    uint b = k < 4u ? as_type<uint>(a) : as_type<uint>(a1);
    return k == 1u || ((k == 2u || k == 4u) && (b & 0x7fffffffu) == 0u) || ((k == 3u || k == 5u) && b == 0x3f800000u);
}
";

/// The logic operation helper.
pub const LOGIC_HELPER: &str = "
struct dm_logic_state { uint op; uint pad0, pad1, pad2; uint4 mask[8]; };   // NVN_LOGIC_OP_* and each colour target's channel masks (dmsh_logic.py)
static inline uint4 dm_logic(uint4 s, uint4 d, constant dm_logic_state& l, uint n)
{
    uint4 r;
    switch (l.op)
    {
        case 0u: r = uint4(0u); break;
        case 1u: r = s & d; break;
        case 2u: r = s & ~d; break;
        case 4u: r = ~s & d; break;
        case 5u: r = d; break;
        case 6u: r = s ^ d; break;
        case 7u: r = s | d; break;
        case 8u: r = ~(s | d); break;
        case 9u: r = ~(s ^ d); break;
        case 10u: r = ~d; break;
        case 11u: r = s | ~d; break;
        case 12u: r = ~s; break;
        case 13u: r = ~s | d; break;
        case 14u: r = ~(s & d); break;
        case 15u: r = uint4(~0u); break;
        default: r = s; break;
    }
    return r & l.mask[n];
}
static inline float4 dm_logic_norm(float4 s, float4 d, constant dm_logic_state& l, uint n)
{
    if (all(l.mask[n] == uint4(0u))) return s;   // not a normalized target: the operation does not apply
    float4 scale = float4(l.mask[n]);
    return float4(dm_logic(uint4(floor(saturate(s) * scale + 0.5f)), uint4(floor(saturate(d) * scale + 0.5f)), l, n)) / scale;
}
";

fn takes_buffer(ps: &[Param], n: u32) -> bool {
    ps.iter().any(|p| p.attr == format!("buffer({n})"))
}

/// The colour 0 member of an entry: the type and the name, when it can carry an alpha.
fn colour0(e: &Ent<'_>) -> Option<(&'static str, String)> {
    let m = e
        .members
        .iter()
        .find(|m| m.attr == "color(0)" || m.attr == "color(0), index(0)")?;
    let ty = ["float4", "half4", "uint4"]
        .into_iter()
        .find(|t| *t == m.ty)?;
    Some((ty, m.name.clone()))
}

/// Why the entries get no alpha test.
pub fn alpha_refusal(es: &[Ent<'_>], ps: &[Param]) -> Option<&'static str> {
    let main = es.first()?;
    if main.ret == "void" {
        return Some("no colour output");
    }
    if matches!(colour0(main), None | Some(("uint4", _))) {
        return Some("colour 0 has no float alpha");
    }
    if takes_buffer(ps, ALPHA_BUFFER) {
        return Some("fragment buffer 30 is taken");
    }
    if es.iter().any(|e| colour0(e).is_none()) {
        return Some("an entry point without a colour 0");
    }
    None
}

/// The alpha test entry of `e`, or `None` when it has no colour 0.
pub fn alpha_entry(e: &Ent<'_>, ps: &[Param], early: bool) -> Option<String> {
    let (ty, name) = colour0(e)?;
    let alpha = if ty == "uint4" {
        format!("as_type<float>(dm_r.{name}.w)")
    } else {
        format!("float(dm_r.{name}.w)")
    };
    let mut full: Vec<String> = ps.iter().map(Param::full).collect();
    full.push(format!(
        "constant dm_alpha_state& dm_alpha [[buffer({ALPHA_BUFFER})]]"
    ));
    Some(format!(
        "{attr}fragment {ret} {n}_a({p})\n{{\n    {ret} dm_r = dm_body_{n}({args});\n    if (!dm_alpha_pass({alpha}, dm_alpha)) discard_fragment();\n    return dm_r;\n}}\n",
        attr = if early {
            "[[early_fragment_tests]] "
        } else {
            ""
        },
        ret = e.ret,
        n = e.name,
        p = full.join(", "),
        args = params_decl(ps).1,
    ))
}

/// Whether the entry gets a logic operation entry.
pub fn logic_entry_name(name: &str) -> bool {
    ["main0", "main0_u", "main0_m"].contains(&name)
}

/// The logic operation entry of `e` and the colour targets it reads.
pub fn logic_entry(e: &Ent<'_>, ps: &[Param], early: bool) -> Option<(String, BTreeSet<u32>)> {
    if !logic_entry_name(&e.name) || takes_buffer(ps, LOGIC_BUFFER) {
        return None;
    }
    let scalar = |ty: &str| {
        if ty.starts_with("uint") {
            "uint"
        } else {
            "float"
        }
    };
    let outs: Vec<_> = e
        .members
        .iter()
        .filter(|m| {
            let color = m.color.is_some() && m.attr == format!("color({})", m.color.unwrap_or(0));
            let ty_ok = ["uint", "float"].iter().any(|b| {
                m.ty.strip_prefix(b)
                    .is_some_and(|r| matches!(r, "" | "2" | "3" | "4"))
            });
            color && ty_ok
        })
        .collect();
    if outs.is_empty() {
        return None;
    }
    let mut full: Vec<String> = ps.iter().map(Param::full).collect();
    let mut apply = Vec::new();
    let mut targets = BTreeSet::new();
    for m in &outs {
        let n = m.color.unwrap_or(0);
        let s = scalar(&m.ty);
        full.push(format!(
            "{s}4 dm_fb{n} [[color({n}), function_constant(dm_lfb{n})]]"
        ));
        let width: usize = m.ty[s.len()..].parse().unwrap_or(1);
        let zero = if s == "uint" { "0u" } else { "0.0f" };
        let pad = format!(", {zero}").repeat(4usize.saturating_sub(width));
        let sw = if width < 4 {
            format!(".{}", &"xyzw"[..width])
        } else {
            String::new()
        };
        let f = if s == "uint" {
            "dm_logic"
        } else {
            "dm_logic_norm"
        };
        let (nm, ty) = (&m.name, &m.ty);
        apply.push(format!(
            "if (dm_lfb{n}) dm_r.{nm} = {ty}({f}({s}4(dm_r.{nm}{pad}), dm_fb{n}, dm_lop, {n}u){sw});"
        ));
        targets.insert(n);
    }
    full.push(format!(
        "constant dm_logic_state& dm_lop [[buffer({LOGIC_BUFFER})]]"
    ));
    let text = format!(
        "{attr}fragment {ret} {n}_l({p})\n{{\n    {ret} dm_r = dm_body_{n}({args});\n    {apply}\n    return dm_r;\n}}\n",
        attr = if early {
            "[[early_fragment_tests]] "
        } else {
            ""
        },
        ret = e.ret,
        n = e.name,
        p = full.join(", "),
        args = params_decl(ps).1,
        apply = apply.join(" "),
    );
    Some((text, targets))
}

/// The function constants of the logic entries.
pub fn logic_constants(targets: &BTreeSet<u32>) -> String {
    targets
        .iter()
        .map(|n| {
            format!(
                "constant bool dm_lfb{n} [[function_constant({})]];\n",
                LOGIC_FC + n
            )
        })
        .collect()
}
