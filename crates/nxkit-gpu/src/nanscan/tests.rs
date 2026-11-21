//! Tests of the texture layer count and the other pure helpers of the nanscan diagnostic.

use super::*;

#[test]
fn a_cube_is_read_as_an_array_of_its_faces() {
    assert_eq!(layers_of(TextureKind::Cube, 6), 6);
    assert_eq!(layers_of(TextureKind::Cube, 1), 6);
    assert_eq!(layers_of(TextureKind::CubeArray, 3), 18);
    assert_eq!(layers_of(TextureKind::D2Array, 5), 5);
    assert_eq!(layers_of(TextureKind::D3, 1), 1);
}

#[test]
fn the_kernels_cover_the_float_formats_only() {
    for f in [
        Format::Rgba16Float,
        Format::Rg16Float,
        Format::R16Float,
        Format::Rgba32Float,
        Format::Rg32Float,
        Format::R32Float,
        Format::Rg11B10Float,
    ] {
        assert!(float_format(f), "{f:?}");
    }
    for f in [
        Format::Rgba8Unorm,
        Format::Depth32Float,
        Format::Rgb9E5Float,
    ] {
        assert!(!float_format(f), "{f:?}");
    }
}
