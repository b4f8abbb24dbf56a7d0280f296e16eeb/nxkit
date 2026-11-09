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
        let e = super::model::shape(f);
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

/// The GPU address of guest offset 0 in Ocarina of Time's run, and the guest buffer's length: a stream near the end
/// lies more than 12 GiB above the base, so no address may be cut to 32 bits.
const GUEST_BASE: u64 = 0x14_F9C8_8000;
const GUEST_LEN: u64 = 12_626 << 20;

/// The address the stage_in fetch reads for `vid` and `iid`: stream address + offset + element * stride, in 64 bits.
fn stage_in_address(s: &StreamDetail, a: &AttributeDetail, vid: u32, iid: u32, bi: u32) -> u64 {
    let element = if s.stride == 0 {
        0
    } else {
        (iid - bi).checked_div(s.divisor).map_or(vid, |i| i + bi)
    };
    s.address + u64::from(a.offset) + u64::from(element) * u64::from(s.stride)
}

/// `dm_fetch` reads the element that the stage_in fetch reads, at the same GPU address, for per-vertex, per-instance
/// and constant streams at aligned and unaligned bases high in the guest buffer.
#[test]
fn a_pulled_element_has_the_address_of_the_stage_in_fetch() {
    let high = GUEST_BASE + GUEST_LEN - 0x10_0000;
    let streams = vec![
        stream(0, high, 20, 0),
        stream(1, high + 0x8_0002, 12, 3),
        stream(2, GUEST_BASE + 0x40, 0, 0),
    ];
    let attributes = vec![
        attr(0, 0x2E, 0, 0),
        attr(3, 0x25, 16, 0),
        attr(5, 0x2D, 0, 1),
        attr(7, 0x22, 6, 1),
        attr(9, 0x2E, 0, 2),
    ];
    let d = draw(attributes.clone(), streams.clone());
    let b = layout(&d);
    for (vid, iid, bi) in [(0, 0, 0), (2, 0, 0), (1000, 7, 2), (65_535, 9, 9)] {
        for a in &attributes {
            let s = streams.iter().find(|s| s.index == a.stream).unwrap();
            let e = model::element(&b, a.location, vid, iid, bi).unwrap();
            assert_eq!(
                e.address,
                stage_in_address(s, a, vid, iid, bi),
                "location {} vid {vid} iid {iid}",
                a.location
            );
        }
    }
    // the block keeps the full 64-bit base
    assert_eq!(u64_at(&b, 0), high);
}

#[test]
fn an_input_without_an_attribute_loads_nothing() {
    let d = draw(
        vec![attr(0, 0x2E, 0, 0), attr(1, 0x01, 0, 3)],
        vec![stream(0, GUEST_BASE, 16, 0)],
    );
    let b = layout(&d);
    assert!(model::element(&b, 1, 0, 0, 0).is_none());
    assert!(model::element(&b, 2, 0, 0, 0).is_none());
    assert!(model::element(&b, 16, 0, 0, 0).is_none());
    assert!(model::element(&b, 0, 0, 0, 0).is_some());
}

/// The bytes of each load of `dm_decode`: whole words for aligned 8- and 16-bit elements, exact sizes otherwise.
#[test]
fn the_load_widths_are_those_of_dm_decode() {
    use model::{read_bytes, shape};
    use nxkit_metal::VertexFormat as V;
    for (f, aligned, bytes) in [
        (V::Float4, true, 16),
        (V::Float3, false, 12),
        (V::Float, true, 4),
        (V::UChar4Norm, true, 4),
        (V::UChar4Norm, false, 4),
        (V::UChar, true, 4),
        (V::UChar, false, 1),
        (V::Half3, true, 8),
        (V::Half3, false, 6),
        (V::Half2, true, 4),
        (V::UInt1010102Norm, false, 4),
        (V::FloatRg11B10, true, 4),
        (V::UChar4NormBgra, true, 4),
    ] {
        assert_eq!(
            read_bytes(shape(f), aligned),
            bytes,
            "{f:?} aligned {aligned}"
        );
    }
    assert!(
        FETCH.contains("if (sz == 1u && n > 2u) w.xy = uint2(*(device const packed_uint2 *)p);")
    );
}

fn inside(a: u64, n: u64) -> bool {
    a >= GUEST_BASE && a + n <= GUEST_BASE + GUEST_LEN
}

/// `NXKIT_DIAG=pull` finds a read that leaves guest memory at the last vertex, and none for a draw that stays inside.
#[test]
fn the_check_finds_reads_outside_guest_memory() {
    let end = GUEST_BASE + GUEST_LEN;
    let d = draw(
        vec![attr(0, 0x2E, 0, 0)],
        vec![stream(0, end - 3 * 16, 16, 0)],
    );
    let b = layout(&d);
    let ranges = model::ranges(&d, |_, _, _| None).unwrap();
    assert_eq!(ranges, ((0, 2), (0, 0)));
    assert!(model::check(&b, &[0], ranges, inside).is_none());
    let more = ((0, 3), (0, 0));
    let s = model::check(&b, &[0], more, inside).unwrap();
    assert_eq!(
        (s.location, s.element.address, s.element.bytes),
        (0, end, 16)
    );
}

#[test]
fn indexed_draws_check_the_indices_plus_the_base_vertex() {
    let mut d = draw(
        vec![attr(0, 0x2E, 0, 0)],
        vec![stream(0, GUEST_BASE, 16, 0)],
    );
    d.args = DrawArgs::Indexed {
        index_type: 1,
        address: GUEST_BASE,
        count: 6,
        instances: 2,
        base_vertex: -4,
        base_instance: 1,
    };
    let r = model::ranges(&d, |t, _, n| {
        assert_eq!((t, n), (1, 6));
        Some((4, 9))
    });
    assert_eq!(r, Some(((0, 5), (1, 2))));
    assert_eq!(model::ranges(&d, |_, _, _| None), None);
}

#[test]
fn the_index_range_skips_restart_indices_but_not_widened_bytes() {
    assert_eq!(index_range(0, &[3, 0xff, 1]), Some((1, 0xff)));
    assert_eq!(index_range(1, &[2, 0, 0xff, 0xff, 7, 0]), Some((2, 7)));
    let mut b = Vec::new();
    for i in [u32::MAX, 5, 9] {
        b.extend(i.to_le_bytes());
    }
    assert_eq!(index_range(2, &b), Some((5, 9)));
    assert_eq!(index_range(1, &[0xff, 0xff]), None);
    assert_eq!(index_range(0, &[]), None);
}

/// Pulling stays off unless `NXKIT_PULL=1`.
#[test]
fn pulling_is_off_unless_asked_for() {
    assert!(!enabled(None));
    assert!(!enabled(Some("0")));
    assert!(!enabled(Some("")));
    assert!(enabled(Some("1")));
}
