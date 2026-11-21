//! The transform feedback capture entry `main0_tf` of a vertex library.
//!
//! The entry runs the vertex function and stores the captured user outputs to the feedback buffers instead of
//! returning them. The host draws it as points with rasterization off. Vertex `n` of the draw writes the record
//! `dm_linear = (instance - base_instance) * count + vertex - first`, and only while `dm_linear < dm_tf.args.x`.
//! The geometry reader `_bgv` takes its vertex from `dm_tf.order[dm_linear]`, so its records stay linear.
//! A capture has no shadow inputs, so the entry belongs to `_b` and `_bgv` only.

use super::super::abi::{TF_SLOT, TF_STATE_SLOT};
use super::super::args;
use super::super::emit::{Em, R, Shader};
use super::super::entry::{EntryName, Param, entry_text};
use super::super::io::Plan;
use super::vertex::emit_with;
use crate::front::msl_entry::Structured;
use crate::job::TransformFeedback;
use nxb_pyjson::Value;

/// The capture state the host binds at `TF_STATE_SLOT`.
const STATE: &str = "struct dm_tf_capture\n{\n    uint4 offset;\n    uint4 args;\n    device const uint* order;\n    uint first, count, base_instance, pad;\n    ulong reserved;\n};\n";

const VERTEX_ID: &str = "gl_VertexIndex";
const INSTANCE_ID: &str = "gl_InstanceIndex";

/// The member of `main0_out` that holds user output `loc`: the output itself, or a vector pad.
fn member(plan: &Plan, loc: u32) -> Option<String> {
    if plan.user_locs.contains(&loc) {
        return Some(format!("loc{loc}"));
    }
    plan.pads
        .iter()
        .find(|p| p.location == loc && p.ty == "float4")
        .map(|_| format!("dm_pad{loc}"))
}

/// The lines at every return of the main function: the stores of one record and the return.
/// An output the shader does not write is `(0, 0, 0, 1)`.
fn exit(plan: &Plan, tf: &TransformFeedback) -> Vec<String> {
    let mut v = vec!["if (dm_linear < dm_tf.args.x)".to_string(), "{".to_string()];
    for (b, buf) in tf.buffers.iter().enumerate() {
        let Some(buf) = buf else { continue };
        let mut word = 0;
        for &(loc, n) in &buf.outputs {
            let from = member(plan, loc);
            for c in 0..n {
                let value = match &from {
                    Some(m) => format!(
                        "as_type<uint>(out.{m}.{})",
                        ["x", "y", "z", "w"][c as usize]
                    ),
                    None if c == 3 => "0x3f800000u".to_string(),
                    None => "0u".to_string(),
                };
                v.push(format!(
                    "    dm_tf{b}[dm_tf.offset[{b}] / 4 + dm_linear * {} + {}] = {value};",
                    buf.stride / 4,
                    word + c
                ));
            }
            word += n;
        }
    }
    v.push("}".into());
    v.push("return;".into());
    v
}

/// The body of `main0_tf`: the main function with the capture at every return.
pub fn body(em: &mut Em<'_>, plan: &Plan, tf: &TransformFeedback) -> R<String> {
    let notes = em.notes.len();
    let layer_src = em.layer_src.len();
    let body = emit_with(em, exit(plan, tf));
    em.notes.truncate(notes);
    em.layer_src.truncate(layer_src);
    body
}

/// The state struct and the entry function. `reader` is the geometry reader, which takes its vertex from the order buffer.
pub fn entry(
    s: &Structured,
    res: &args::Res,
    plan: &Plan,
    body: &str,
    tf: &TransformFeedback,
    reader: bool,
) -> R<String> {
    let mut extra = Vec::new();
    for (id, attr) in [(VERTEX_ID, "vertex_id"), (INSTANCE_ID, "instance_id")] {
        if !plan.builtins.iter().any(|b| b.name == id) {
            let name = if id == VERTEX_ID { "dm_vid" } else { "dm_iid" };
            extra.push(Param::new(format!("uint {name}"), attr, name));
        }
    }
    let name_of = |id: &str, own: &str| {
        if plan.builtins.iter().any(|b| b.name == id) {
            id.to_string()
        } else {
            own.to_string()
        }
    };
    let (vid, iid) = (name_of(VERTEX_ID, "dm_vid"), name_of(INSTANCE_ID, "dm_iid"));
    for (b, buf) in tf.buffers.iter().enumerate() {
        if buf.is_some() {
            let name = format!("dm_tf{b}");
            extra.push(Param::new(
                format!("device uint* {name}"),
                format!("buffer({})", TF_SLOT + b as u32),
                name,
            ));
        }
    }
    extra.push(Param::new(
        "constant dm_tf_capture& dm_tf",
        format!("buffer({TF_STATE_SLOT})"),
        "dm_tf",
    ));
    let mut pro = vec![format!(
        "uint dm_linear = ({iid} - dm_tf.base_instance) * dm_tf.count + {vid} - dm_tf.first;"
    )];
    if reader {
        pro.push(format!("{vid} = dm_tf.order[dm_linear];"));
    }
    let e = EntryName {
        ret: Some("void"),
        extra: &extra,
        pro: &pro,
        ..EntryName::new("main0_tf")
    };
    let mut t = String::from(STATE);
    t.push_str(&entry_text(s, &e, Shader::Vertex, res, plan, body)?);
    Ok(t)
}

/// The reflection field that names the entry.
pub fn entry_field() -> (String, Value) {
    ("entry_tf".to_string(), Value::str("main0_tf"))
}

/// The reflection field with the bytes per vertex of each feedback buffer, 0 for a buffer without capture.
pub fn strides_field(tf: &TransformFeedback) -> (String, Value) {
    let v = tf
        .strides()
        .iter()
        .map(|s| Value::Int(i64::from(*s)))
        .collect();
    ("tf_strides".to_string(), Value::List(v))
}
