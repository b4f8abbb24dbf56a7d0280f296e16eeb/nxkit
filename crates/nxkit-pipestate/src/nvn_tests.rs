use super::*;

#[test]
fn colour_and_depth_formats_map() {
    assert_eq!(
        texture_format(0x25).map(Mapped::format),
        Some(Format::Rgba8Unorm)
    );
    assert_eq!(
        texture_format(0x35),
        Some(Mapped::Widened(Format::Depth32FloatStencil8))
    );
    assert_eq!(texture_format(0x19), None);
    assert_eq!(
        texture_format(0x80).map(Mapped::format),
        Some(Format::Astc {
            size: 7,
            srgb: false
        })
    );
}

#[test]
fn rgbx_formats_are_rgba_and_read_alpha_as_one() {
    // Xenoblade Chronicles 3 samples a 16x16x16 RGBX8 colour grading table
    assert_eq!(
        texture_format(0x6D).map(Mapped::format),
        Some(Format::Rgba8Unorm)
    );
    assert_eq!(
        texture_format(0x76).map(Mapped::format),
        Some(Format::Rgba32Float)
    );
    assert_eq!(sample_swizzle(0x6D, [2, 3, 4, 5]), [2, 3, 4, 1]);
    assert_eq!(sample_swizzle(0x6D, [5, 5, 5, 5]), [1, 1, 1, 1]);
    assert_eq!(sample_swizzle(0x25, [2, 3, 4, 5]), [2, 3, 4, 5]);
}

#[test]
fn the_layout_holds_only_the_attributes_the_shader_reads() {
    let attributes = [
        Attribute {
            location: 0,
            format: 0x25 + 9,
            offset: 0,
            stream: 0,
        },
        Attribute {
            location: 1,
            format: 0x25 + 9,
            offset: 16,
            stream: 0,
        },
        Attribute {
            location: 2,
            format: 0x0D + 9,
            offset: 0,
            stream: 1,
        },
    ];
    let streams = [
        Stream {
            index: 0,
            stride: 32,
            divisor: 0,
        },
        Stream {
            index: 1,
            stride: 8,
            divisor: 0,
        },
    ];
    let d = vertex_descriptor(&attributes, &streams, &[(0, VertexFormat::Float4)]).unwrap();
    assert_eq!(d.attributes.len(), 1);
    assert_eq!(d.attributes[0].0, 0);
    assert_eq!(
        d.layouts.len(),
        1,
        "stream 1 has no attribute the shader reads"
    );
    assert_eq!(d.layouts[0].buffer, stream_slot(0));
    assert_eq!(
        d,
        vertex_descriptor(
            &attributes[..1],
            &streams[..1],
            &[(0, VertexFormat::Float4)]
        )
        .unwrap(),
        "the attributes the shader does not read change nothing"
    );
}

#[test]
fn vertex_formats_follow_the_group_layout() {
    assert_eq!(vertex_format(0x0A), Some(VertexFormat::Float));
    assert_eq!(vertex_format(0x0D + 0x09), Some(VertexFormat::Float2));
    assert_eq!(vertex_format(0x25 + 0x09), Some(VertexFormat::Float4));
    assert_eq!(vertex_format(0x25), Some(VertexFormat::UChar4Norm));
    assert_eq!(vertex_format(0x9A), Some(VertexFormat::UChar4NormBgra));
    assert_eq!(vertex_format(0x31), None);
}

#[test]
fn small_integers_read_with_the_other_kind_do_not_fit_stage_in() {
    use VertexFormat as V;
    // 0x2D SINT16x4, 0x2C UINT16x4, 0x27 UINT8x4, 0x25 UNORM8x4, 0x2E FLOAT32x4, 0x30 SINT32x4.
    assert_eq!(vertex_format(0x2D), Some(V::Short4));
    assert!(
        !stage_in_fits(0x2D, V::UInt4),
        "Pokopia: SINT16 read as uint4"
    );
    assert!(stage_in_fits(0x2D, V::Int4));
    assert!(!stage_in_fits(0x2C, V::Int4));
    assert!(!stage_in_fits(0x27, V::Float4));
    assert!(stage_in_fits(0x25, V::Float4));
    assert!(!stage_in_fits(0x25, V::UInt4));
    // A 32-bit format changes its kind without changing the bits.
    assert!(stage_in_fits(0x2E, V::UInt4));
    assert!(stage_in_fits(0x30, V::Float4));
    // A format without a Metal vertex format is left to the layout.
    assert!(stage_in_fits(0x31, V::Float4));
}
