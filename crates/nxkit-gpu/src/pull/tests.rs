//! The layout block against the translator's `dm_fetch.metal`, and pipelines without a vertex layout.

use super::*;
use crate::detail::{AttributeDetail, DrawArgs, ProgramRef, StreamDetail};
use crate::format_map::vertex_format;
use nxkit_metal::VertexFormat;

/// The fetch helpers of the `_bp` libraries, as the translator writes them.
const FETCH: &str = include_str!("../../../nxb-xlate/src/msl/variant/dm_fetch.metal");
/// The translator's side of the contract (`SLOT`).
const PULL_RS: &str = include_str!("../../../nxb-xlate/src/msl/variant/pull.rs");

/// The members of `struct <name> { ... };` in `FETCH`: (type, name, array length).
fn members(name: &str) -> Vec<(String, String, usize)> {
    let start = FETCH
        .find(&format!("struct {name} {{"))
        .unwrap_or_else(|| panic!("struct {name}"));
    let body = &FETCH[start..];
    let body = &body[body.find('{').unwrap() + 1..body.find("};").unwrap()];
    body.split(';')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(|m| {
            let (decl, count) = match m.split_once('[') {
                Some((d, n)) => (d.trim(), n.trim_end_matches(']').trim().parse().unwrap()),
                None => (m, 1),
            };
            let at = decl.rfind([' ', '*']).unwrap();
            let (ty, field) = (decl[..=at].trim(), decl[at + 1..].trim());
            (ty.to_string(), field.to_string(), count)
        })
        .collect()
}

/// (size, alignment) of an MSL type of the block.
fn size_align(ty: &str) -> (usize, usize) {
    if ty.ends_with('*') {
        return (8, 8);
    }
    match ty {
        "uint" | "int" | "float" => (4, 4),
        other => {
            let (size, align, _) = struct_layout(other);
            (size, align)
        }
    }
}

/// The size, alignment and member offsets of a struct of `FETCH`, by the C layout rules that MSL follows.
fn struct_layout(name: &str) -> (usize, usize, Vec<(String, usize)>) {
    let mut at = 0usize;
    let mut align = 1usize;
    let mut offsets = Vec::new();
    for (ty, field, count) in members(name) {
        let (s, a) = size_align(&ty);
        at = at.next_multiple_of(a);
        offsets.push((field, at));
        at += s * count;
        align = align.max(a);
    }
    (at.next_multiple_of(align), align, offsets)
}

#[test]
fn the_block_has_the_layout_of_dm_vlayout() {
    let (size, _, offsets) = struct_layout("dm_vstream");
    assert_eq!(size, STREAM_BYTES);
    assert_eq!(
        offsets,
        vec![
            ("base".to_string(), 0),
            ("stride".to_string(), 8),
            ("divisor".to_string(), 12)
        ]
    );
    let (size, _, offsets) = struct_layout("dm_vlayout");
    assert_eq!(size, BYTES);
    assert_eq!(BYTES, 320);
    assert_eq!(
        offsets,
        vec![("s".to_string(), 0), ("attr".to_string(), ATTR_OFFSET)]
    );
    assert_eq!(
        members("dm_vlayout"),
        vec![
            ("dm_vstream".to_string(), "s".to_string(), STREAMS),
            ("uint".to_string(), "attr".to_string(), ATTRIBUTES),
        ]
    );
}

#[test]
fn the_slot_and_the_attribute_word_are_those_of_the_translator() {
    assert!(PULL_RS.contains(&format!("pub const SLOT: u32 = {SLOT};")));
    // format bits 0 to 7, offset 8 to 26, stream 27 to 31
    assert!(FETCH.contains("off = (w >> 8) & 0x7ffffu, st = w >> 27"));
    assert!(FETCH.contains("w & 0xffu"));
    assert_eq!(attribute_word(0x25, 12, 3), 0x25 | 12 << 8 | 3 << 27);
    assert_eq!(attribute_word(0xff, 0x7_ffff, 15), u32::MAX >> 5 | 15 << 27);
    assert_eq!(attribute_word(0x100, 0, 0), 0);
    assert_eq!(attribute_word(1, 0x8_0000, 0), 0);
    assert_eq!(attribute_word(1, 0, 16), 0);
}

/// `dm_ftab` of `FETCH`.
fn ftab() -> Vec<u8> {
    let start = FETCH.find("dm_ftab[256] = {").unwrap();
    let body = &FETCH[start..];
    let body = &body[body.find('{').unwrap() + 1..body.find("};").unwrap()];
    let t: Vec<u8> = body
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(t.len(), 256);
    t
}

/// (components, size code, kind) of `dm_ftab` for a Metal vertex format.
fn expected(f: VertexFormat) -> (u8, u8, u8) {
    use VertexFormat as V;
    // kinds: 1 unorm, 2 snorm, 3 uint, 4 sint, 5 float, 8 unorm 10:10:10:2, 9 snorm 10:10:10:2, 10 R11G11B10F, 11 BGRA8
    match f {
        V::UCharNorm => (1, 0, 1),
        V::UChar2Norm => (2, 0, 1),
        V::UChar3Norm => (3, 0, 1),
        V::UChar4Norm => (4, 0, 1),
        V::CharNorm => (1, 0, 2),
        V::Char2Norm => (2, 0, 2),
        V::Char3Norm => (3, 0, 2),
        V::Char4Norm => (4, 0, 2),
        V::UChar => (1, 0, 3),
        V::UChar2 => (2, 0, 3),
        V::UChar3 => (3, 0, 3),
        V::UChar4 => (4, 0, 3),
        V::Char => (1, 0, 4),
        V::Char2 => (2, 0, 4),
        V::Char3 => (3, 0, 4),
        V::Char4 => (4, 0, 4),
        V::Half => (1, 1, 5),
        V::Half2 => (2, 1, 5),
        V::Half3 => (3, 1, 5),
        V::Half4 => (4, 1, 5),
        V::UShortNorm => (1, 1, 1),
        V::UShort2Norm => (2, 1, 1),
        V::UShort3Norm => (3, 1, 1),
        V::UShort4Norm => (4, 1, 1),
        V::ShortNorm => (1, 1, 2),
        V::Short2Norm => (2, 1, 2),
        V::Short3Norm => (3, 1, 2),
        V::Short4Norm => (4, 1, 2),
        V::UShort => (1, 1, 3),
        V::UShort2 => (2, 1, 3),
        V::UShort3 => (3, 1, 3),
        V::UShort4 => (4, 1, 3),
        V::Short => (1, 1, 4),
        V::Short2 => (2, 1, 4),
        V::Short3 => (3, 1, 4),
        V::Short4 => (4, 1, 4),
        V::Float => (1, 2, 5),
        V::Float2 => (2, 2, 5),
        V::Float3 => (3, 2, 5),
        V::Float4 => (4, 2, 5),
        V::UInt => (1, 2, 3),
        V::UInt2 => (2, 2, 3),
        V::UInt3 => (3, 2, 3),
        V::UInt4 => (4, 2, 3),
        V::Int => (1, 2, 4),
        V::Int2 => (2, 2, 4),
        V::Int3 => (3, 2, 4),
        V::Int4 => (4, 2, 4),
        V::UInt1010102Norm => (4, 2, 8),
        V::Int1010102Norm => (4, 2, 9),
        V::FloatRg11B10 => (3, 2, 10),
        V::UChar4NormBgra => (4, 0, 11),
    }
}

/// Every NVN format that the stage_in path draws decodes to the same components, size and kind through `dm_fetch`, so
/// switching to pulling loses no attribute and changes no value.
#[test]
fn every_format_of_the_stage_in_path_has_its_decode_in_the_table() {
    let t = ftab();
    let mut checked = 0;
    for nvn in 0..256u32 {
        let Some(f) = vertex_format(nvn) else {
            continue;
        };
        let e = expected(f);
        let b = t[nvn as usize];
        let got = ((b & 3) + 1, (b >> 2) & 3, b >> 4);
        assert_eq!(got, e, "format {nvn:#x} {f:?}");
        checked += 1;
    }
    assert_eq!(checked, 52);
    // A format the table lacks reads zeros, like an unbound input.
    assert_eq!(t[0], 0);
}

fn draw(attributes: Vec<AttributeDetail>, streams: Vec<StreamDetail>) -> DrawDetail {
    DrawDetail {
        program: ProgramRef {
            vertex: "pull-vs".into(),
            fragment: Some("pull-fs".into()),
            task: None,
            mesh: None,
        },
        push: None,
        blend: Default::default(),
        write_masks: [15; 8],
        depth_stencil: Default::default(),
        raster: Default::default(),
        topology: 4,
        attributes,
        streams,
        args: DrawArgs::Arrays {
            first: 0,
            count: 3,
            instances: 1,
            base_instance: 0,
        },
        ubos: vec![],
        sbufs: vec![],
        textures: vec![],
        images: vec![],
        dynamic: Default::default(),
    }
}

fn attr(location: u32, format: u32, offset: u32, stream: u32) -> AttributeDetail {
    AttributeDetail {
        location,
        format,
        offset,
        stream,
    }
}

fn stream(index: u32, address: u64, stride: u32, divisor: u32) -> StreamDetail {
    StreamDetail {
        index,
        address,
        size: 4096,
        stride,
        divisor,
    }
}

fn u32_at(b: &Block, at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &Block, at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

#[test]
fn the_block_holds_the_streams_and_attributes_of_the_draw() {
    let d = draw(
        vec![
            attr(0, 0x2E, 0, 0),
            attr(3, 0x25, 12, 0),
            attr(5, 0x22, 0, 2),
        ],
        vec![
            stream(0, 0x1_0000_1000, 16, 0),
            stream(2, 0x1_0000_8000, 4, 0),
        ],
    );
    let b = layout(&d);
    assert_eq!(u64_at(&b, 0), 0x1_0000_1000);
    assert_eq!(u32_at(&b, 8), 16);
    assert_eq!(u32_at(&b, 12), 0);
    assert_eq!(u64_at(&b, 2 * STREAM_BYTES), 0x1_0000_8000);
    assert_eq!(u32_at(&b, 2 * STREAM_BYTES + 8), 4);
    // stream 1 is not bound
    assert!(b[STREAM_BYTES..2 * STREAM_BYTES].iter().all(|&x| x == 0));
    assert_eq!(u32_at(&b, ATTR_OFFSET), 0x2E);
    assert_eq!(u32_at(&b, ATTR_OFFSET + 3 * 4), 0x25 | 12 << 8);
    assert_eq!(u32_at(&b, ATTR_OFFSET + 5 * 4), 0x22 | 2 << 27);
    for l in [1, 2, 4, 6, 15] {
        assert_eq!(u32_at(&b, ATTR_OFFSET + l * 4), 0, "location {l}");
    }
}

/// The base address keeps 4-byte alignment for `dm_fetch`'s whole-word reads; its low bits move into the offsets.
#[test]
fn an_unaligned_base_moves_its_low_bits_into_the_offsets() {
    let d = draw(
        vec![attr(0, 0x25, 0, 1), attr(1, 0x0A, 6, 1)],
        vec![stream(1, 0x2000_0003, 10, 0)],
    );
    let b = layout(&d);
    assert_eq!(u64_at(&b, STREAM_BYTES), 0x2000_0000);
    assert_eq!(u32_at(&b, ATTR_OFFSET), 0x25 | 3 << 8 | 1 << 27);
    assert_eq!(u32_at(&b, ATTR_OFFSET + 4), 0x0A | 9 << 8 | 1 << 27);
}

#[test]
fn instanced_and_constant_streams_keep_their_divisor_and_stride() {
    let d = draw(
        vec![
            attr(0, 0x2E, 0, 0),
            attr(1, 0x2E, 0, 1),
            attr(2, 0x2E, 0, 2),
        ],
        vec![
            stream(0, 0x1000, 16, 0),
            stream(1, 0x2000, 16, 3),
            stream(2, 0x3000, 0, 0),
        ],
    );
    let b = layout(&d);
    // per vertex, per instance with divisor 3 (dm_fetch adds the base instance), constant
    assert_eq!((u32_at(&b, 8), u32_at(&b, 12)), (16, 0));
    assert_eq!(
        (u32_at(&b, STREAM_BYTES + 8), u32_at(&b, STREAM_BYTES + 12)),
        (16, 3)
    );
    assert_eq!(
        (
            u32_at(&b, 2 * STREAM_BYTES + 8),
            u32_at(&b, 2 * STREAM_BYTES + 12)
        ),
        (0, 0)
    );
}

/// An input without a bound attribute reads zeros, as the zero default vertex of the stage_in path gives it.
#[test]
fn an_input_without_a_bound_stream_reads_the_default() {
    let d = draw(
        vec![
            attr(0, 0x2E, 0, 4),
            attr(1, 0x2E, 0, 20),
            attr(16, 0x2E, 0, 0),
        ],
        vec![stream(0, 0x1000, 16, 0), stream(20, 0x5000, 16, 0)],
    );
    let b = layout(&d);
    assert_eq!(u32_at(&b, ATTR_OFFSET), 0);
    assert_eq!(u32_at(&b, ATTR_OFFSET + 4), 0);
    // only stream 0 is in the block
    assert_eq!(u64_at(&b, 0), 0x1000);
    assert!(b[STREAM_BYTES..ATTR_OFFSET].iter().all(|&x| x == 0));
    assert!(b[ATTR_OFFSET..].iter().all(|&x| x == 0));
}

/// A pulling vertex function has no vertex layout: draws with different layouts share one pipeline key, and a
/// thread without the memo finds the same pipeline in the cache.
#[test]
fn pipelines_of_a_pulling_vertex_function_do_not_depend_on_the_layout() {
    use crate::pipelines::{Pipelines, Targets};
    use crate::programs::StageProgram;
    use nxkit_metal::{ArchiveConfig, Context, Format, ShaderLibrary};
    let Ok(ctx) = Context::system_default() else {
        return;
    };
    let lib = ShaderLibrary::from_source(
        &ctx,
        "pull-test",
        r#"
#include <metal_stdlib>
using namespace metal;
struct L { device const uchar *base; uint stride; uint divisor; };
vertex float4 vs(constant L *l [[buffer(16)]], uint id [[vertex_id]]) {
    return float4(float(l[0].stride + id), 0, 0, 1);
}
fragment float4 fs() { return float4(1); }
"#,
    )
    .unwrap();
    let stage = |entry: &str, pull: bool| StageProgram {
        lib: lib.clone(),
        entry: entry.into(),
        entry_uint: None,
        entry_mixed: None,
        binds: vec![],
        arg_bytes: 0,
        inputs: if pull {
            vec![(0, VertexFormat::Float4)]
        } else {
            vec![]
        },
        pull,
        colors: Some(vec![0]),
        threadgroup: [1; 3],
        c1: vec![],
        mesh: None,
    };
    let (vs, fs) = (stage("vs", true), stage("fs", false));
    let targets = Targets {
        colors: vec![Some(Format::Rgba8Unorm)],
        samples: 1,
    };
    let a = draw(vec![attr(0, 0x2E, 0, 0)], vec![stream(0, 0x1000, 16, 0)]);
    let b = draw(vec![attr(0, 0x25, 4, 1)], vec![stream(1, 0x2000, 8, 2)]);
    let pipelines = Pipelines::new(&ctx, &ArchiveConfig::default()).unwrap();
    let (_, ka) = pipelines
        .render(&a, &vs, Some(&fs), &targets, false)
        .unwrap();
    let (_, kb) = pipelines
        .render(&b, &vs, Some(&fs), &targets, false)
        .unwrap();
    assert_eq!(ka, kb);
    assert_eq!(pipelines.counts(), (1, 0));
    // Another thread has no memo: its lookup builds the key and finds the pipeline.
    let kc = std::thread::scope(|s| {
        s.spawn(|| {
            pipelines
                .render(&b, &vs, Some(&fs), &targets, false)
                .unwrap()
                .1
        })
        .join()
        .unwrap()
    });
    assert_eq!(kc, ka);
    assert_eq!(pipelines.counts(), (1, 1));
}
