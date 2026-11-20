//! Opt-in indexed capture for diagnostic tessellation replay.
//!
//! This emits only a separately staged vertex-only library. Neither the ordinary
//! translator nor the game producer will generate or select it.

use super::args;
use super::emit::{R, Shader};
use super::entry::{EntryName, Form, Param, StageIn, call_text, entry_text};
use super::io::Plan;
use super::variant::vertex::{Bodies, CAPTURE_SLOT, CAPTURE_WORDS};
use super::{Context, Failure, Translated, failure, library, options, structure_job};
use crate::attr::Stage;
use crate::job::Job;
use crate::translation::msl_entry::Structured;
use nxb_pyjson::Value;
use std::fmt::Write;

/// Indexing wrapper entry in the diagnostic vertex library.
pub const ENTRY: &str = "main0_tess_indexed";

/// Emits a standalone `_bt` library and reflection. The caller must explicitly
/// stage it beside the existing vertex library, and use matched buffers from
/// the same version's capture and draw layout.
pub fn translate_diagnostic(
    job: &Job,
    code: &[u8],
    ctx: &Context<'_>,
) -> Result<Translated, Failure> {
    if job.stage != "vs" {
        return Err(failure("indexed capture needs a vertex job".into()));
    }
    let structured = structure_job(job, code).map_err(failure)?;
    if structured.props.definitions.stage != Some(Stage::Vertex) {
        return Err(failure("indexed capture needs a vertex header".into()));
    }
    library::build_indexed_capture(&structured, ctx, &options("vs", code, ctx)).map_err(failure)
}

pub(super) fn reflection() -> Value {
    Value::obj([("version", Value::Int(1)), ("entry", Value::str(ENTRY))])
}

pub(super) fn in_struct(plan: &Plan) -> R<String> {
    if plan.attributes.len() != plan.in_members.len()
        || plan
            .attributes
            .iter()
            .any(|(l, ty)| *l >= 16 || !matches!(ty.as_str(), "float4" | "uint4" | "int4"))
    {
        return Err("unported: indexed capture vertex input layout".into());
    }
    if plan.attributes.is_empty() {
        return Ok(String::new());
    }
    let mut text = String::from("struct main0_in {\n");
    for (loc, ty) in &plan.attributes {
        let _ = writeln!(text, "    {ty} loc{loc};");
    }
    text.push_str("};\n");
    Ok(text)
}

// Embedded MSL is deliberately independent from the non-diagnostic fetch path's
// unchecked address arithmetic. Faster aligned narrow reads would overread
// the exact element extent, so narrow formats always take dm_decode's exact path.
const HELPERS: &str = "
struct dm_indexed_draw {
    ulong index_bytes;
    uint first_index;
    uint index_count;
    int base_vertex;
    uint width;
    uint max_vertex;
};

static bool dm_indexed_vertex(device const uchar* indices, constant dm_indexed_draw& draw,
                              uint ordinal, thread uint& vid)
{
    uint width = draw.width;
    if (ordinal >= draw.index_count || (width != 1u && width != 2u && width != 4u)) return false;
    ulong off = (ulong(draw.first_index) + ulong(ordinal)) * ulong(width);
    if (off > draw.index_bytes || ulong(width) > draw.index_bytes - off) return false;
    uint index = uint(indices[off]);
    if (width >= 2u) index |= uint(indices[off + 1ul]) << 8u;
    if (width == 4u) {
        index |= uint(indices[off + 2ul]) << 16u;
        index |= uint(indices[off + 3ul]) << 24u;
    }
    long biased = long(index) + long(draw.base_vertex);
    if (biased < long(0) || biased > long(draw.max_vertex)) return false;
    vid = uint(biased);
    return true;
}

static bool dm_indexed_attr(constant dm_vlayout& layout, constant ulong* available,
                            uint loc, uint vid, thread uint4& value)
{
    uint w = layout.attr[loc], fmt = w & 0xffu;
    if (fmt == 0u) {
        value = uint4(0u);
        return true;
    }
    uint st = w >> 27;
    if (st >= 16u) return false;
    uint t = dm_ftab[fmt], kind = t >> 4u, n = (t & 3u) + 1u, sz = (t >> 2u) & 3u;
    if (kind == 0u || sz > 2u) return false;
    bool packed = kind >= 8u;
    ulong width = packed ? 4ul : ulong(n << sz);
    ulong align = packed ? 4ul : ulong(1u << sz);
    uint stride = layout.s[st].stride;
    uint i = layout.s[st].divisor == 0u ? vid : 0u;
    ulong rel = ulong(i) * ulong(stride) + ulong((w >> 8u) & 0x7ffffu);
    ulong limit = available[st];
    if (rel > limit || width > limit - rel || (rel & (align - 1ul)) != 0ul) return false;
    value = dm_decode(layout.s[st].base + rel, fmt, false);
    return true;
}
";

pub(super) fn entry(s: &Structured, res: &args::Res, plan: &Plan, b: &Bodies) -> R<String> {
    let body = EntryName {
        form: Form::Body { inline: false },
        stage_in: StageIn::None,
        ..EntryName::new("main0")
    };
    // The body still takes its original stage input type, as a plain argument.
    let in_param = Param::new("main0_in in", "stage_in", "in");
    let body = EntryName {
        stage_in: if plan.in_members.is_empty() {
            StageIn::None
        } else {
            StageIn::Custom(&in_param)
        },
        ..body
    };
    let shader = Shader::Vertex;
    let mut t = entry_text(s, &body, shader, res, plan, &b.shared)?;
    t.push_str(HELPERS);
    let params = [
        Param::new(
            "constant dm_geometry_capture& dm_gs",
            format!("buffer({CAPTURE_SLOT})"),
            "dm_gs",
        ),
        Param::new("constant dm_vlayout& dm_lay", "buffer(16)", "dm_lay"),
        Param::new("device const uchar* dm_indices", "buffer(17)", "dm_indices"),
        Param::new("constant dm_indexed_draw& dm_draw", "buffer(18)", "dm_draw"),
        Param::new("device uint* dm_valid", "buffer(19)", "dm_valid"),
        Param::new("constant ulong* dm_available", "buffer(20)", "dm_available"),
        Param::new("uint dm_ordinal", "vertex_id", "dm_ordinal"),
    ];
    let mut pro = vec![
        "if (dm_ordinal >= dm_gs.info.y) return;".to_string(),
        "dm_valid[dm_ordinal] = 0u;".into(),
        format!(
            "if (dm_gs.info.x != 0u || dm_gs.stride != {CAPTURE_WORDS}u || dm_draw.index_count != dm_gs.info.y) return;"
        ),
        "uint gl_VertexIndex = 0u;".into(),
        "if (!dm_indexed_vertex(dm_indices, dm_draw, dm_ordinal, gl_VertexIndex)) return;".into(),
        "uint gl_BaseVertex = as_type<uint>(dm_draw.base_vertex);".into(),
        "uint gl_InstanceIndex = 0u;".into(),
        "uint gl_BaseInstance = 0u;".into(),
    ];
    if !plan.attributes.is_empty() {
        pro.push("main0_in in = {};".into());
    }
    for (loc, ty) in &plan.attributes {
        pro.extend([
            format!("uint4 dm_word{loc} = uint4(0u);"),
            format!("if (!dm_indexed_attr(dm_lay, dm_available, {loc}u, gl_VertexIndex, dm_word{loc})) return;"),
            format!("in.loc{loc} = as_type<{ty}>(dm_word{loc});"),
        ]);
    }
    let wrapper = EntryName {
        ret: Some("void"),
        stage_in: StageIn::None,
        omit_vertex_ids: true,
        extra: &params,
        pro: &pro,
        ..EntryName::new(ENTRY)
    };
    t.push_str(&call_text(
        &wrapper,
        &body,
        shader,
        res,
        plan,
        &[
            "dm_capture(dm_gs, dm_gs.dst + ulong(dm_ordinal) * dm_gs.stride, out);".to_string(),
            "dm_valid[dm_ordinal] = 1u;".to_string(),
        ],
    )?);
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::parse_jobs;
    use crate::translation::emit_test_util::PRED7;

    #[test]
    fn indexed_capture_is_guarded_and_only_opt_in() {
        let job = parse_jobs(r#"[{"key":"synthetic","stage":"vs"}]"#)
            .unwrap()
            .remove(0);
        let ald = |rd: u64, off: u64| {
            (0b11101111110110u64 << 50) | rd | (255 << 8) | PRED7 | (off << 20) | (255 << 39)
        };
        let ast = |rd: u64, off: u64| {
            (0b11101111111100u64 << 50) | rd | (255 << 8) | PRED7 | (off << 20) | (255 << 39)
        };
        let mut code = super::super::geom_tests::code(
            0,
            &[ald(0, 0x080), ald(1, 0x2fc), ast(0, 0x070), ast(1, 0x080)],
        );
        code[..4].copy_from_slice(&(1u32 << 10).to_le_bytes());
        let context = Context::default();
        let t = translate_diagnostic(&job, &code, &context).unwrap();
        assert_eq!(t.libraries.len(), 1);
        let library = &t.libraries[0];
        assert_eq!(library.suffix, "_bt");
        let reflect = library.reflection.as_ref().unwrap();
        assert_eq!(reflect.entry, ENTRY);
        let extra = super::super::geom_tests::extra(reflect, "indexed_capture").unwrap();
        assert_eq!(extra.get("version").and_then(Value::as_i64), Some(1));
        assert_eq!(extra.get("entry").and_then(Value::as_str), Some(ENTRY));

        let msl = &library.msl;
        assert!(
            msl.contains("ulong(width) > draw.index_bytes - off"),
            "{msl}"
        );
        assert!(
            msl.contains("long(index) + long(draw.base_vertex)"),
            "{msl}"
        );
        assert!(msl.contains("width > limit - rel"), "{msl}");
        assert!(
            msl.contains("dm_decode(layout.s[st].base + rel, fmt, false)"),
            "{msl}"
        );
        let (_, wrapper) = msl.split_once("vertex void main0_tess_indexed(").unwrap();
        let (params, body) = wrapper.split_once(")\n{").unwrap();
        for n in [13, 16, 17, 18, 19, 20] {
            assert!(params.contains(&format!("buffer({n})")), "{params}");
        }
        assert_eq!(params.matches("vertex_id").count(), 1, "{params}");
        assert!(!params.contains("base_vertex"), "{params}");
        assert!(body.contains("dm_body_main0(in, gl_VertexIndex)"), "{msl}");
        let invalid = body.find("dm_valid[dm_ordinal] = 0u").unwrap();
        let index = body.find("if (!dm_indexed_vertex(").unwrap();
        let attr = body.find("if (!dm_indexed_attr(").unwrap();
        let evaluate = body.find("dm_body_main0(").unwrap();
        let store = body
            .find("dm_capture(dm_gs, dm_gs.dst + ulong(dm_ordinal)")
            .unwrap();
        let valid = body.find("dm_valid[dm_ordinal] = 1u").unwrap();
        assert!(
            invalid < index && index < attr && attr < evaluate && evaluate < store && store < valid
        );
        let normal = super::super::translate(&job, &code, &context).unwrap();
        assert!(normal.libraries.iter().all(|l| l.suffix != "_bt"));
    }
}
