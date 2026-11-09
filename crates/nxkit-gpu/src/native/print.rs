//! A fingerprint of a picture's luma plane: how the Neural Engine path finds a picture whose planes the game copied
//! (by the GPU or by the CPU) before it used them.
//!
//! The fingerprint is a hash of [`SAMPLES`] bytes of the plane at fixed positions and of the plane's size. The decoder
//! side takes it from the decoded picture ([`of_frame`]); the game's side takes the same samples from guest memory
//! wherever the plane lies there, through the layout of that memory ([`Layout`]): rows `pitch` bytes apart (a buffer
//! the game read into, the source of a copy, or a pitch-linear texture) or block-linear (a texture). The samples depend
//! on the position of a texel in the picture only, never on the pitch or the tiling, so the same picture in any layout
//! gives the same fingerprint. Two pictures whose samples are all equal (a black frame) give one fingerprint; they look
//! the same at the samples, and the newest of them is taken.

use nxkit_nvn::swizzle::{GobOrder, block_linear_offset};
use nxkit_svc_video::Frame;

/// The bytes a fingerprint reads.
pub(super) const SAMPLES: u32 = 256;

/// The fingerprint of a plane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Print(u64);

/// How the rows of a plane lie in memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
    /// Rows `pitch` bytes apart.
    Pitch(u64),
    /// Block-linear (one byte a texel) with the log2 block height in GOBs, as a texture of the game.
    Block(u32),
}

impl Layout {
    /// The byte offset of texel `x`, `y` of a plane `width` by `height`.
    fn offset(self, (x, y): (u32, u32), (width, height): (u32, u32)) -> u64 {
        match self {
            Self::Pitch(p) => u64::from(y) * p + u64::from(x),
            Self::Block(tile_h) => {
                let row_bytes = width.div_ceil(64) * 64;
                block_linear_offset(GobOrder::Tegra, (x, y, 0), row_bytes, height, tile_h, 0)
            }
        }
    }
}

/// The position of sample `i` in a plane `width` by `height`. The positions scatter over the plane (the strides are
/// odd and large) so that a vertical or horizontal structure does not hide a change.
fn point(i: u32, width: u32, height: u32) -> (u32, u32) {
    let (i, w, h) = (u64::from(i), u64::from(width), u64::from(height));
    (
        ((i * 7_919 + 101) % w) as u32,
        ((i * 104_729 + 53) % h) as u32,
    )
}

/// The fingerprint of a plane `width` by `height` whose texel `(x, y)` is `at(x, y)`. `None` when the plane is empty or
/// `at` has no byte for a sample.
pub(super) fn of_plane(
    (width, height): (u32, u32),
    mut at: impl FnMut(u32, u32) -> Option<u8>,
) -> Option<Print> {
    if width == 0 || height == 0 {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (u64::from(width) << 32 | u64::from(height));
    for i in 0..SAMPLES {
        let (x, y) = point(i, width, height);
        h = (h ^ u64::from(at(x, y)?)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(Print(h))
}

/// The fingerprint of the plane of a picture in memory that reads through `layout`: `read(offset)` gives the byte
/// at a byte offset of the memory, or `None` when the memory ends there.
pub(super) fn of_memory(
    size: (u32, u32),
    layout: Layout,
    mut read: impl FnMut(u64) -> Option<u8>,
) -> Option<Print> {
    of_plane(size, |x, y| read(layout.offset((x, y), size)))
}

/// The fingerprint of the luma of a decoded picture.
pub(super) fn of_frame(frame: &Frame) -> Option<Print> {
    let (w, h) = (frame.width(), frame.height());
    frame
        .with_planes(|y, stride, _, _| {
            of_plane((w, h), |px, py| {
                y.get(py as usize * stride + px as usize).copied()
            })
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nxkit_nvn::layout::{Target, TextureDesc};
    use nxkit_nvn::swizzle::swizzle_level;

    const W: u32 = 1920;
    const H: u32 = 1080;

    /// A plane with structure in both directions.
    fn picture() -> Vec<u8> {
        (0..W * H)
            .map(|i| ((i % W) * 7 + (i / W) * 13 + (i % 5) * 31) as u8)
            .collect()
    }

    fn padded(plane: &[u8], pitch: usize) -> Vec<u8> {
        let mut out = vec![0xAA; pitch * H as usize];
        for (r, row) in plane.chunks(W as usize).enumerate() {
            out[r * pitch..r * pitch + W as usize].copy_from_slice(row);
        }
        out
    }

    fn print_of(mem: &[u8], layout: Layout) -> Option<Print> {
        of_memory((W, H), layout, |o| mem.get(o as usize).copied())
    }

    #[test]
    fn a_padded_copy_has_the_fingerprint_of_the_tight_picture() {
        let plane = picture();
        let tight = print_of(&plane, Layout::Pitch(u64::from(W)));
        assert!(tight.is_some());
        for pitch in [2048usize, 1984, 4096] {
            let copy = padded(&plane, pitch);
            assert_eq!(print_of(&copy, Layout::Pitch(pitch as u64)), tight);
        }
        // The decoder's side reads the plane through `of_plane` with the decoder's stride.
        let by_rows = of_plane((W, H), |x, y| plane.get((y * W + x) as usize).copied());
        assert_eq!(by_rows, tight);
    }

    #[test]
    fn a_block_linear_texture_has_the_fingerprint_of_the_tight_picture() {
        let plane = picture();
        let desc = TextureDesc {
            target: Target::D2,
            format: 0x01,
            flags: 0,
            samples: 0,
            width: W,
            height: H,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        let tile_h = desc.level_tiles(0).map(|t| t.0).unwrap_or(4);
        let mut storage = vec![0u8; 4 * 1024 * 1024];
        swizzle_level(
            &desc,
            GobOrder::Tegra,
            (0, 0),
            &plane,
            W as usize,
            &mut storage,
        )
        .unwrap_or_else(|e| panic!("swizzle: {e}"));
        assert_eq!(
            print_of(&storage, Layout::Block(tile_h)),
            print_of(&plane, Layout::Pitch(u64::from(W)))
        );
    }

    #[test]
    fn another_picture_has_another_fingerprint() {
        let plane = picture();
        let mut other = plane.clone();
        // Change one byte that a sample reads.
        let (x, y) = point(17, W, H);
        let at = (y * W + x) as usize;
        other[at] = other[at].wrapping_add(1);
        let p = Layout::Pitch(u64::from(W));
        assert_ne!(print_of(&plane, p), print_of(&other, p));
        // The size is part of it.
        let half = of_plane((W / 2, H), |x, y| plane.get((y * W + x) as usize).copied());
        assert_ne!(half, print_of(&plane, p));
    }

    #[test]
    fn memory_that_ends_early_has_no_fingerprint() {
        let plane = picture();
        let pitch = Layout::Pitch(u64::from(W));
        assert_eq!(
            of_memory((W, H), pitch, |o| plane
                .get(o as usize)
                .copied()
                .filter(|_| o < 1000)),
            None
        );
        assert_eq!(of_plane((0, 5), |_, _| Some(0)), None);
    }

    #[test]
    fn the_samples_lie_inside_the_plane() {
        for (w, h) in [(1920, 1080), (7, 3), (1, 1), (2560, 1440)] {
            for i in 0..SAMPLES {
                let (x, y) = point(i, w, h);
                assert!(x < w && y < h);
            }
        }
    }
}
