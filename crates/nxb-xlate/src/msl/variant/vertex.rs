//! The extra entries of a vertex library: `main0_z`, `main0_discard`, `main0_l`, `main0_lz` and `main0_gs`.
//!
//! The main function is written once, as the body function `dm_body_main0`. It returns `out` without the depth fix.
//! Every entry but `main0_discard` is a short function that calls it and then adds its own tail.
//! The base entry `main0` ends with the depth fix of the GL convention. `main0_z` ends without it, for a host that
//! keeps the Maxwell depth range. `main0_discard` has no outputs and runs the stores to buffers and images only.
//! `main0_l` and `main0_lz` return the layer a shader packs into an integer store of a user output.
//! `main0_gs` writes the outputs to a capture buffer for the geometry shader emulation.

use super::super::args;
use super::super::emit::{Em, R};
use super::super::entry::{EntryName, Form, Param, StageIn, call_text, entry_text};
use super::super::io::Plan;
use crate::translation::msl_entry::Structured;
use nxb_pyjson::Value;
use std::fmt::Write;

/// The capture buffer binding.
const CAPTURE_SLOT: u32 = 13;
/// Words of the Maxwell attribute map: 16 fixed and 32 locations of 4 components.
pub const CAPTURE_WORDS: u32 = 144;

/// The layer entries.
pub struct Layer {
    /// The user output location the layer comes from.
    pub loc: i32,
    /// The component.
    pub comp: i32,
}

/// The bodies of a vertex library.
pub struct Bodies {
    /// The body function `dm_body_main0`: the main function with `return out;` at every return.
    pub shared: String,
    /// The body of `main0_discard`, when the shader writes buffers or images.
    pub discard: Option<String>,
    /// The layer entries.
    pub layer: Option<Layer>,
}

fn member_name(m: &str) -> &str {
    let head = m.split(" [[").next().unwrap_or(m);
    head.rsplit(' ').next().unwrap_or(head)
}

/// The element count of an array member.
fn array_len(m: &str) -> Option<u32> {
    let t = m.trim_end().strip_suffix("];")?;
    t.rsplit('[').next()?.parse().ok()
}

/// The body of the main function with `exit` at every return and at its end, if it ends without one.
pub(super) fn emit_with(em: &mut Em<'_>, exit: Vec<String>) -> R<String> {
    em.exit = Some(exit.clone());
    let body = em.body(0);
    em.exit = None;
    let mut body = body?;
    let last = body.trim_end().lines().last().map(str::trim);
    if !last.is_some_and(|l| l.starts_with("return")) {
        for l in exit {
            let _ = writeln!(body, "    {l}");
        }
    }
    Ok(body)
}

/// The first integer store to a user output in the order of the text: the helper functions, then the main function.
fn layer_source(em: &Em<'_>) -> Option<(i32, i32)> {
    let key = |f: usize| if f == 0 { usize::MAX } else { f };
    let mut best: Option<(usize, usize, i32, i32)> = None;
    for (n, (f, l, c)) in em.layer_src.iter().enumerate() {
        let k = (key(*f), n);
        if best.is_none_or(|b| k < (b.0, b.1)) {
            best = Some((k.0, k.1, *l, *c));
        }
    }
    best.map(|b| (b.2, b.3))
}

/// Emit the shared body and the body of `main0_discard` with their tails.
pub fn bodies(em: &mut Em<'_>, plan: &Plan, writes_memory: bool) -> R<Bodies> {
    let notes = em.notes.len();
    let layer_src = em.layer_src.len();
    let shared = emit_with(em, vec!["return out;".to_string()])?;
    let discard = if writes_memory {
        Some(emit_with(em, vec!["return;".to_string()])?)
    } else {
        None
    };
    let has_instance = plan.builtins.iter().any(|b| b.name == "gl_InstanceIndex");
    let layer = match layer_source(em) {
        Some((loc, comp)) if has_instance && !plan.layer && (0..4).contains(&comp) => {
            Some(Layer { loc, comp })
        }
        _ => None,
    };
    em.notes.truncate(notes);
    em.layer_src.truncate(layer_src);
    Ok(Bodies {
        shared,
        discard,
        layer,
    })
}

/// The structs and functions the entries use.
pub fn decls(plan: &Plan, b: &Bodies) -> R<String> {
    let mut t = String::new();
    if b.layer.is_some() {
        let members: Vec<&String> = plan
            .out_members
            .iter()
            .filter(|m| !m.contains("[[point_size]]"))
            .collect();
        t.push_str("struct main0l_out\n{\n");
        for m in &members {
            let _ = writeln!(t, "    {m}");
        }
        t.push_str("    uint gl_Layer [[render_target_array_index]];\n};\n");
        t.push_str("static inline main0l_out dm_layer_out(thread main0_out& out, uint layer)\n{\n    main0l_out r = {};\n");
        for m in members {
            let n = member_name(m);
            match array_len(m) {
                Some(len) => {
                    let _ = writeln!(
                        t,
                        "    for (int i = 0; i < {len}; i += 1) r.{n}[i] = out.{n}[i];"
                    );
                }
                None => {
                    let _ = writeln!(t, "    r.{n} = out.{n};");
                }
            }
        }
        t.push_str("    r.gl_Layer = layer;\n    return r;\n}\n");
    }
    t.push_str(&capture_decls(plan)?);
    Ok(t)
}

/// The capture buffer layout and the function that writes the words of one vertex.
fn capture_decls(plan: &Plan) -> R<String> {
    let mut t = String::new();
    let _ = writeln!(
        t,
        "struct dm_geometry_capture\n{{\n    device uint* dst;\n    uint4 info;\n    uint stride;\n    short slot[{CAPTURE_WORDS}];\n}};"
    );
    t.push_str("static inline void dm_capture(constant dm_geometry_capture& g, device uint* p, thread main0_out& out)\n{\n");
    let _ = writeln!(t, "    uint w[{CAPTURE_WORDS}];");
    let _ = writeln!(
        t,
        "    for (uint k = 0; k < {CAPTURE_WORDS}; k += 1u) w[k] = (k == 3 || k == 4 || (k >= 19 && k % 4 == 3)) ? 0x3f800000u : 0u;"
    );
    for (k, c) in ["x", "y", "z", "w"].iter().enumerate() {
        let _ = writeln!(t, "    w[{k}] = as_type<uint>(out.gl_Position.{c});");
    }
    for m in &plan.out_members {
        match member_name(m) {
            "gl_PointSize" => t.push_str("    w[4] = as_type<uint>(out.gl_PointSize);\n"),
            "gl_Layer" => t.push_str("    w[13] = out.gl_Layer;\n"),
            "gl_ViewportIndex" => t.push_str("    w[14] = out.gl_ViewportIndex;\n"),
            "gl_ClipDistance" => {
                for c in 0..array_len(m).unwrap_or(0).min(8) {
                    let _ = writeln!(
                        t,
                        "    w[{}] = as_type<uint>(out.gl_ClipDistance[{c}]);",
                        5 + c
                    );
                }
            }
            _ => {}
        }
    }
    let pads = plan
        .pads
        .iter()
        .map(|p| (p.location, format!("dm_pad{}", p.location)));
    let users = plan.user_locs.iter().map(|l| (*l, format!("loc{l}")));
    for (l, name) in users.chain(pads) {
        if l >= 32 {
            return Err("geometry capture: attribute outside Maxwell map".into());
        }
        for (c, x) in ["x", "y", "z", "w"].iter().enumerate() {
            let _ = writeln!(
                t,
                "    w[{}] = as_type<uint>(out.{name}.{x});",
                16 + l * 4 + c as u32
            );
        }
    }
    let _ = writeln!(
        t,
        "    for (uint k = 0; k < {CAPTURE_WORDS}; k += 1u) {{ short s = g.slot[k]; if (s >= 0) p[s] = w[k]; }}\n}}"
    );
    Ok(t)
}

/// How the entries of a library take their stage input and which entries it keeps.
#[derive(Default)]
pub struct Frame<'a> {
    /// The stage input.
    pub stage_in: StageIn<'a>,
    /// Parameters after the standard ones.
    pub extra: &'a [Param],
    /// Statements before the output is declared.
    pub pro: &'a [String],
    /// Only `main0` and `main0_z`.
    pub plain_only: bool,
}

/// The text of the entries of a vertex library: the body function, then the entries that call it.
pub fn entries(
    s: &Structured,
    res: &args::Res,
    plan: &Plan,
    b: &Bodies,
    f: &Frame<'_>,
) -> R<String> {
    let shader = super::super::emit::Shader::Vertex;
    let body = EntryName {
        form: Form::Body { inline: false },
        stage_in: f.stage_in,
        extra: f.extra,
        pro: f.pro,
        ..EntryName::new("main0")
    };
    let mk = |name: &'static str, ret: Option<&'static str>| EntryName {
        ret,
        stage_in: f.stage_in,
        extra: f.extra,
        ..EntryName::new(name)
    };
    let mut t = entry_text(s, &body, shader, res, plan, &b.shared)?;
    let call = |e: &EntryName<'_>, tail: &[String]| call_text(e, &body, shader, res, plan, tail);
    let ret = ["return out;".to_string()];
    let fix = |last: &str| {
        let mut l = plan.vertex_epilogue(true);
        l.push(last.to_string());
        l
    };
    t.push_str(&call(&mk("main0", None), &fix("return out;"))?);
    t.push_str(&call(&mk("main0_z", None), &ret)?);
    if f.plain_only {
        return Ok(t);
    }
    if let Some(d) = &b.discard {
        t.push_str(&entry_text(
            s,
            &mk("main0_discard", Some("void")),
            shader,
            res,
            plan,
            d,
        )?);
    }
    if let Some(l) = &b.layer {
        let last = format!(
            "return dm_layer_out(out, as_type<uint>(out.loc{}.{}));",
            l.loc,
            ["x", "y", "z", "w"][l.comp as usize]
        );
        t.push_str(&call(&mk("main0_l", Some("main0l_out")), &fix(&last))?);
        t.push_str(&call(&mk("main0_lz", Some("main0l_out")), &[last])?);
    }
    let (vid, iid) = (
        ("gl_VertexIndex", "uint", "vertex_id"),
        ("gl_InstanceIndex", "uint", "instance_id"),
    );
    let mut params = vec![Param::new(
        "constant dm_geometry_capture& dm_gs",
        format!("buffer({CAPTURE_SLOT})"),
        "dm_gs",
    )];
    for (id, ty, attr) in [&vid, &iid] {
        if !plan.builtins.iter().any(|b| b.name == *id) {
            params.push(Param::new(format!("{ty} {id}"), *attr, *id));
        }
    }
    let pro = vec![
        "uint4 dm_ci = dm_gs.info;".to_string(),
        "device uint* dm_dst = dm_gs.dst;".to_string(),
        format!(
            "if ({} - dm_ci.x >= dm_ci.y || {} - dm_ci.z >= dm_ci.w) return;",
            vid.0, iid.0
        ),
        format!(
            "device uint* dm_p = dm_dst + ((ulong({} - dm_ci.z) * dm_ci.y + {} - dm_ci.x) * dm_gs.stride);",
            iid.0, vid.0
        ),
    ];
    let gs = EntryName {
        ret: Some("void"),
        stage_in: f.stage_in,
        extra: &params,
        pro: &pro,
        ..EntryName::new("main0_gs")
    };
    t.push_str(&call(&gs, &["dm_capture(dm_gs, dm_p, out);".to_string()])?);
    Ok(t)
}

/// The reflection fields of the entries.
pub fn fields(b: &Bodies) -> Vec<(String, Value)> {
    let mut v = vec![
        ("entry_z".to_string(), Value::str("main0_z")),
        ("entry_geometry_capture".to_string(), Value::str("main0_gs")),
    ];
    if b.discard.is_some() {
        v.push(("entry_discard".to_string(), Value::str("main0_discard")));
    }
    if let Some(l) = &b.layer {
        v.push(("entry_layer".to_string(), Value::str("main0_l")));
        v.push(("entry_layer_z".to_string(), Value::str("main0_lz")));
        v.push((
            "layer_from".to_string(),
            Value::obj([
                ("loc", Value::Int(i64::from(l.loc))),
                ("comp", Value::str(["x", "y", "z", "w"][l.comp as usize])),
            ]),
        ));
    }
    v
}
