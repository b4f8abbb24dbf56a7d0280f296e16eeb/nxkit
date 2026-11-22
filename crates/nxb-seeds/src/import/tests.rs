//! Tests for the import of the engine's rows, with keys built at the offsets of `pso_skey`.

use nxkit_pipestate::seeds::Seed;

use super::*;

const V: &str = "00000000000000a1";
const F: &str = "00000000000000b2";

/// The hex of a key with a target of `cfmt`, blending in slot 0, one attribute and its stream, cut to `len` digits.
fn key(cfmt: u32, merged: bool, len: usize) -> String {
    let mut b = vec![0u8; 512];
    b[..18].copy_from_slice(format!("{V}_b").as_bytes());
    b[24..42].copy_from_slice(format!("{F}_b").as_bytes());
    b[50] = u8::from(merged);
    let put = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 88 + 16, cfmt);
    put(&mut b, 88 + 56, 1);
    put(&mut b, 88 + 60, 1);
    put(&mut b, 88 + 64, 0xF);
    // src 5, dst 6, src alpha 2, dst alpha 6, equations 1 and 1
    let w: u64 = (5 << 3) | (6 << 10) | (2 << 17) | (6 << 24) | (1 << 31) | (1 << 34);
    b[88 + 72..88 + 80].copy_from_slice(&w.to_le_bytes());
    // attribute 3: Float3 (0x22), offset 12, stream 1, and stream 1 has stride 24
    put(&mut b, 88 + 136 + 12, 0x22 | (12 << 8) | (1 << 27));
    put(&mut b, 88 + 200 + 8, 24);
    let hex: String = b.iter().map(|c| format!("{c:02x}")).collect();
    hex[..len].to_string()
}

#[test]
fn a_render_row_becomes_a_seed_in_nvn_terms() {
    for len in [864, 1024] {
        let mut i = Imported::default();
        i.manifest(&format!(
            "# a comment\nS\t{V}_b\t0\nC\t{F}_b\nR\t{}\n",
            key(115, false, len)
        ));
        assert_eq!(i.left, Left::default());
        let s = i.seeds.seeds();
        assert_eq!(s[0], Seed::Shader(V.into()));
        assert_eq!(s[1], Seed::Compute(F.into()));
        let Seed::Render(r) = &s[2] else {
            panic!("render")
        };
        assert_eq!((r.vertex.as_str(), r.fragment.as_deref()), (V, Some(F)));
        assert_eq!(r.colors.len(), 1);
        let c = r.colors[0].unwrap();
        assert_eq!(c.format, Format::Rgba16Float);
        let b = c.blend.unwrap();
        assert_eq!(
            (
                b.src_rgb,
                b.dst_rgb,
                b.op_rgb,
                b.src_alpha,
                b.dst_alpha,
                b.op_alpha
            ),
            (5, 6, 1, 2, 6, 1)
        );
        assert_eq!(
            r.attributes,
            vec![Attribute {
                location: 3,
                format: 0x22,
                offset: 12,
                stream: 1
            }]
        );
        assert_eq!(
            r.streams,
            vec![Stream {
                index: 1,
                stride: 24,
                divisor: 0
            }]
        );
    }
}

#[test]
fn the_engines_own_rows_are_left_out_and_counted() {
    let mut i = Imported::default();
    i.manifest(&format!(
        "R\t{}\nR\t{}\nR\tzz\nM\t-\t{}\nX\t1\n",
        key(115, true, 1024),
        key(7777, false, 1024),
        key(70, false, 1008)
    ));
    assert_eq!(
        i.left,
        Left {
            damaged: 2,
            merged: 1,
            format: 1
        }
    );
    let Seed::Mesh(m) = &i.seeds.seeds()[0] else {
        panic!("mesh")
    };
    assert_eq!((m.task.as_deref(), m.mesh.as_str()), (None, V));
    i.shader_seen("# keys\n0000000000000abc\nnot a key\n");
    assert_eq!(i.left.damaged, 3);
    assert_eq!(i.seeds.len(), 2);
}
