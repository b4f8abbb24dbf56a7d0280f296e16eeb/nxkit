//! Reading and writing texels from the CPU: `nvnTextureWriteTexels`, `nvnTextureReadTexels`, their strided forms and
//! `nvnTextureFlushTexels` and `nvnTextureInvalidateTexels`.
//!
//! The storage of a texture is the memory of the game, in the block-linear layout of the GPU. A write puts the bytes of
//! the region at their block-linear offsets in that memory, in place and in runs of 16 bytes (the 16 bytes of a run lie
//! next to each other in a GOB). It does not decode and encode a whole level. A read is the same walk the other way.
//! Neither allocates. A write then tells the front end which bytes changed, so the GPU copy is loaded again at its next
//! use. A read takes the bytes from the memory of the game. That is the current copy as long as the GPU did not write
//! the texture. A texture that the GPU wrote last has no readback in the backend, so a read of it stops the game
//! with a message.
//!
//! A linear texture is one level of one layer. Its rows are `stride` bytes apart. The GOB order is the order of the
//! Tegra X1.

use nxkit_abi::guest;
use nxkit_nvn::format::format_info;
use nxkit_nvn::layout::{FLAG_LINEAR, FLAG_LINEAR_RENDER_TARGET, Target};
use nxkit_nvn::resource::Residency;
use nxkit_nvn::swizzle::{GobOrder, block_linear_offset, level_geometry};

use crate::nvn::abi::{AbiResult, Obj, guard, other, table};
use crate::nvn::ctx::gpu;

use super::Tex;
use super::view::View;

/// The bytes of an `NVNcopyRegion`: x, y, z, width, height, depth (i32 each).
const REGION_BYTES: usize = 24;

/// What a copy touches, in blocks.
#[derive(Clone, Copy, Debug)]
struct Plan {
    level: u32,
    layer0: u32,
    /// The first byte of a row and the first row, in bytes and in block rows.
    x0: u32,
    y0: u32,
    z0: u32,
    /// The bytes of a row and the block rows of the region.
    wb: u32,
    hb: u32,
    nz: u32,
    /// The distance of rows and of images in the buffer of the game.
    row: u64,
    img: u64,
}

impl Plan {
    /// The bytes that the buffer of the game must hold.
    fn buffer_bytes(&self) -> u64 {
        u64::from(self.nz - 1) * self.img + u64::from(self.hb - 1) * self.row + u64::from(self.wb)
    }
}

fn plan(t: &Tex, view: u64, region: u64, row: i64, img: i64) -> AbiResult<Option<Plan>> {
    let mut r = [0u8; REGION_BYTES];
    Obj(region).read(0, &mut r)?;
    let v = |i: usize| i32::from_le_bytes([r[4 * i], r[4 * i + 1], r[4 * i + 2], r[4 * i + 3]]);
    let (x, y, z, w, h, d) = (v(0), v(1), v(2), v(3), v(4), v(5));
    if x < 0 || y < 0 || z < 0 || w < 0 || h < 0 || d < 0 || row < 0 || img < 0 {
        return Err(other(format!(
            "the copy region ({x}, {y}, {z}, {w}, {h}, {d}) or its strides ({row}, {img}) are negative"
        )));
    }
    let f = format_info(t.desc.format)?;
    let (bw, bh, bpb) = (u32::from(f.block_w), u32::from(f.block_h), u32::from(f.bytes_per_block));
    let wb = (w as u32).div_ceil(bw) * bpb;
    let hb = (h as u32).div_ceil(bh);
    if wb == 0 || hb == 0 {
        return Ok(None);
    }
    let view = View::load(view)?;
    let row = if row == 0 { u64::from(wb) } else { row as u64 };
    let img = if img == 0 { row * u64::from(hb) } else { img as u64 };
    Ok(Some(Plan {
        level: view.level(),
        layer0: view.first_layer(),
        x0: (x as u32 / bw) * bpb,
        y0: y as u32 / bh,
        z0: z as u32,
        wb,
        hb,
        nz: (d as u32).max(1),
        row,
        img,
    }))
}

/// Calls `f(storage_offset, buffer_offset, bytes)` for every run of the copy. The runs lie inside the storage.
fn walk(
    t: &Tex,
    p: &Plan,
    mut f: impl FnMut(usize, usize, usize) -> AbiResult<()>,
) -> AbiResult<()> {
    let to_usize = |v: u64| usize::try_from(v).map_err(|_| other("an offset does not fit"));
    let check = |at: u64, n: u64| {
        if at.checked_add(n).is_none_or(|end| end > t.size) {
            Err(other(format!(
                "the copy leaves the storage of the texture ({at:#x} + {n:#x} of {:#x} bytes)",
                t.size
            )))
        } else {
            Ok(())
        }
    };
    let d = &t.desc;
    if d.target == Target::Buffer || d.flags & (FLAG_LINEAR | FLAG_LINEAR_RENDER_TARGET) != 0 {
        if p.level != 0 || p.layer0 != 0 || p.z0 != 0 || p.nz != 1 {
            return Err(other("a linear texture has one level and one layer"));
        }
        let pitch = if d.stride != 0 {
            u64::from(d.stride)
        } else {
            t.layer_size / u64::from(d.height.max(1))
        };
        if u64::from(p.x0) + u64::from(p.wb) > pitch {
            return Err(other("the copy is wider than a row of the linear texture"));
        }
        for y in 0..p.hb {
            let at = (u64::from(p.y0) + u64::from(y)) * pitch + u64::from(p.x0);
            check(at, u64::from(p.wb))?;
            f(to_usize(at)?, to_usize(u64::from(y) * p.row)?, p.wb as usize)?;
        }
        return Ok(());
    }
    let is_3d = d.target == Target::D3;
    let gobs = d.level_gobs(p.level)?;
    for z in 0..p.nz {
        let (layer, slice) = if is_3d {
            (p.layer0, p.z0 + z)
        } else {
            (p.layer0 + p.z0 + z, 0)
        };
        let g = level_geometry(d, p.level, layer)?;
        if p.x0 + p.wb > g.row_bytes || p.y0 + p.hb > g.rows || slice >= g.slices {
            return Err(other(format!(
                "the copy leaves level {} of the texture (row bytes {}+{} of {}, rows {}+{} of {}, slice {slice} of {})",
                p.level, p.x0, p.wb, g.row_bytes, p.y0, p.hb, g.rows, g.slices
            )));
        }
        let laid_out = (gobs * 64).max(g.row_bytes);
        for y in 0..p.hb {
            let mut x = 0u32;
            while x < p.wb {
                let bx = p.x0 + x;
                let n = (16 - bx % 16).min(p.wb - x);
                let at = g.base
                    + block_linear_offset(
                        GobOrder::default(),
                        (bx, p.y0 + y, slice),
                        laid_out,
                        g.rows,
                        g.tile_h,
                        g.tile_d,
                    );
                check(at, u64::from(n))?;
                let from = u64::from(z) * p.img + u64::from(y) * p.row + u64::from(x);
                f(to_usize(at)?, to_usize(from)?, n as usize)?;
                x += n;
            }
        }
    }
    Ok(())
}

/// Puts the buffer at `src` into the texture. The result is the span of storage bytes that changed.
fn put(t: &Tex, p: &Plan, src: u64) -> AbiResult<(u64, u64)> {
    let (mut lo, mut hi) = (u64::MAX, 0u64);
    guest::with_slice(src, p.buffer_bytes(), |buf| {
        guest::with_slice_mut(t.host, t.size, |st| {
            walk(t, p, |so, bo, n| {
                let to = st.get_mut(so..so + n).ok_or_else(|| other("a run leaves the storage"))?;
                to.copy_from_slice(buf.get(bo..bo + n).ok_or_else(|| other("a run leaves the buffer"))?);
                lo = lo.min(so as u64);
                hi = hi.max((so + n) as u64);
                Ok(())
            })
        })
    })???;
    Ok((lo, hi))
}

/// Takes the region of the texture into the buffer at `dst`.
fn take(t: &Tex, p: &Plan, dst: u64) -> AbiResult<()> {
    guest::with_slice_mut(dst, p.buffer_bytes(), |buf| {
        guest::with_slice(t.host, t.size, |st| {
            walk(t, p, |so, bo, n| {
                let to = buf.get_mut(bo..bo + n).ok_or_else(|| other("a run leaves the buffer"))?;
                to.copy_from_slice(st.get(so..so + n).ok_or_else(|| other("a run leaves the storage"))?);
                Ok(())
            })
        })
    })???;
    Ok(())
}

fn write_texels(name: &str, t: u64, view: u64, region: u64, src: u64, row: i64, img: i64) -> AbiResult<()> {
    let t = Tex::load(t)?;
    if t.host == 0 {
        eprintln!("[RED] NVN {name}: the texture has no storage");
        return Ok(());
    }
    let Some(p) = plan(&t, view, region, row, img)? else {
        return Ok(());
    };
    let (lo, hi) = put(&t, &p, src)?;
    gpu(name).invalidated(t.host + lo, hi - lo);
    gpu(name).with_front(|f| f.texels_written(t.host + lo, hi - lo).map(|_| ()))
}

fn read_texels(name: &str, t: u64, view: u64, region: u64, dst: u64, row: i64, img: i64) -> AbiResult<()> {
    let t = Tex::load(t)?;
    if t.host == 0 {
        eprintln!("[RED] NVN {name}: the texture has no storage");
        return Ok(());
    }
    let Some(p) = plan(&t, view, region, row, img)? else {
        return Ok(());
    };
    let residency = gpu(name).with_front(|f| Ok(f.resources().get(t.id)?.residency))?;
    if residency == Residency::GpuNewer {
        return Err(other(
            "the GPU wrote this texture and the backend has no readback, so the CPU cannot read it",
        ));
    }
    take(&t, &p, dst)
}

extern "C" fn write(t: u64, view: u64, region: u64, src: u64) {
    guard("nvnTextureWriteTexels", || {
        write_texels("nvnTextureWriteTexels", t, view, region, src, 0, 0)
    });
}

extern "C" fn write_strided(t: u64, view: u64, region: u64, src: u64, row: i64, img: i64) {
    guard("nvnTextureWriteTexelsStrided", || {
        write_texels("nvnTextureWriteTexelsStrided", t, view, region, src, row, img)
    });
}

extern "C" fn read(t: u64, view: u64, region: u64, dst: u64) {
    guard("nvnTextureReadTexels", || {
        read_texels("nvnTextureReadTexels", t, view, region, dst, 0, 0)
    });
}

extern "C" fn read_strided(t: u64, view: u64, region: u64, dst: u64, row: i64, img: i64) {
    guard("nvnTextureReadTexelsStrided", || {
        read_texels("nvnTextureReadTexelsStrided", t, view, region, dst, row, img)
    });
}

/// The memory of the game is the one copy on the CPU side, so there is nothing to flush.
extern "C" fn flush(_t: u64, _view: u64, _region: u64) {}

/// A read always takes the bytes from the memory of the game, so there is nothing to invalidate.
extern "C" fn invalidate(_t: u64, _view: u64, _region: u64) {}

/// The functions of the texel access.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnTextureWriteTexels" => write as extern "C" fn(u64, u64, u64, u64),
        "nvnTextureWriteTexelsStrided" => write_strided as extern "C" fn(u64, u64, u64, u64, i64, i64),
        "nvnTextureReadTexels" => read as extern "C" fn(u64, u64, u64, u64),
        "nvnTextureReadTexelsStrided" => read_strided as extern "C" fn(u64, u64, u64, u64, i64, i64),
        "nvnTextureFlushTexels" => flush as extern "C" fn(u64, u64, u64),
        "nvnTextureInvalidateTexels" => invalidate as extern "C" fn(u64, u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;
    use nxkit_nvn::layout::TextureDesc;
    use nxkit_nvn::resource::TexId;
    use nxkit_nvn::swizzle::deswizzle_level;

    fn texture(w: u32, h: u32, format: i32, flags: u32) -> Tex {
        let desc = TextureDesc {
            target: Target::D2,
            format,
            flags,
            samples: 0,
            width: w,
            height: h,
            depth: 1,
            levels: 1,
            stride: 0,
            packaged_block: None,
        };
        let layout = desc.layout().expect("layout");
        Tex {
            id: TexId(1),
            pool: 0,
            off: 0,
            host: testing::alloc(layout.size),
            size: layout.size,
            layer_size: layout.layer_size,
            desc,
            stencil: false,
            swizzle: [2, 3, 4, 5],
        }
    }

    fn region(x: i32, y: i32, w: i32, h: i32) -> u64 {
        let r = testing::alloc(24);
        for (i, v) in [x, y, 0, w, h, 1].into_iter().enumerate() {
            Obj(r).set_i32(4 * i as u64, v).expect("region");
        }
        r
    }

    fn pattern(n: usize) -> u64 {
        let b = testing::alloc(n as u64);
        let bytes: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        Obj(b).write(0, &bytes).expect("pattern");
        b
    }

    #[test]
    fn a_region_written_in_place_is_what_the_whole_level_decodes_to() {
        let t = texture(64, 64, 0x25, 0);
        let src = pattern(8 * 8 * 4);
        let p = plan(&t, 0, region(4, 5, 8, 8), 0, 0).expect("plan").expect("not empty");
        let (lo, hi) = put(&t, &p, src).expect("put");
        assert!(lo < hi && hi <= t.size);
        let mut lin = vec![0u8; 64 * 64 * 4];
        let mut storage = vec![0u8; t.size as usize];
        guest::read(t.host, &mut storage).expect("storage");
        deswizzle_level(&t.desc, GobOrder::default(), (0, 0), &storage, &mut lin, 64 * 4)
            .expect("deswizzle");
        let mut want = vec![0u8; 8 * 8 * 4];
        guest::read(src, &mut want).expect("src");
        for y in 0..8usize {
            let from = ((5 + y) * 64 + 4) * 4;
            assert_eq!(lin[from..from + 32], want[y * 32..y * 32 + 32]);
        }
        assert_eq!(lin[..4], [0, 0, 0, 0]);
    }

    #[test]
    fn a_region_that_is_read_back_equals_what_was_written() {
        let t = texture(100, 70, 0x25, 0);
        let src = pattern(33 * 17 * 4);
        let p = plan(&t, 0, region(3, 2, 33, 17), 0, 0).expect("plan").expect("not empty");
        put(&t, &p, src).expect("put");
        let dst = testing::alloc(33 * 17 * 4);
        take(&t, &p, dst).expect("take");
        let (mut a, mut b) = (vec![0u8; 33 * 17 * 4], vec![0u8; 33 * 17 * 4]);
        guest::read(src, &mut a).expect("src");
        guest::read(dst, &mut b).expect("dst");
        assert_eq!(a, b);
    }

    #[test]
    fn a_linear_texture_is_written_row_by_row_with_its_pitch() {
        let t = texture(10, 4, 0x25, FLAG_LINEAR);
        let src = pattern(2 * 2 * 4);
        let p = plan(&t, 0, region(1, 1, 2, 2), 0, 0).expect("plan").expect("not empty");
        put(&t, &p, src).expect("put");
        let pitch = (t.size / 4) as usize;
        let mut st = vec![0u8; t.size as usize];
        guest::read(t.host, &mut st).expect("storage");
        let mut want = [0u8; 16];
        guest::read(src, &mut want).expect("src");
        assert_eq!(st[pitch + 4..pitch + 12], want[..8]);
        assert_eq!(st[2 * pitch + 4..2 * pitch + 12], want[8..]);
    }

    #[test]
    fn a_region_outside_the_level_is_an_error() {
        let t = texture(16, 16, 0x25, 0);
        let src = pattern(32 * 32 * 4);
        let p = plan(&t, 0, region(8, 8, 32, 32), 0, 0).expect("plan").expect("not empty");
        assert!(put(&t, &p, src).is_err());
    }

    #[test]
    fn an_empty_region_copies_nothing() {
        let t = texture(16, 16, 0x25, 0);
        assert!(plan(&t, 0, region(0, 0, 0, 4), 0, 0).expect("plan").is_none());
    }

    #[test]
    fn the_strides_of_the_buffer_are_kept() {
        let t = texture(32, 32, 0x25, 0);
        let src = pattern(64 * 4);
        let p = plan(&t, 0, region(0, 0, 4, 2), 64, 0).expect("plan").expect("not empty");
        assert_eq!(p.buffer_bytes(), 64 + 16);
        put(&t, &p, src).expect("put");
        let dst = testing::alloc(64 * 4);
        take(&t, &p, dst).expect("take");
        let mut a = [0u8; 80];
        let mut b = [0u8; 80];
        guest::read(src, &mut a).expect("src");
        guest::read(dst, &mut b).expect("dst");
        assert_eq!(a[..16], b[..16]);
        assert_eq!(a[64..80], b[64..80]);
    }

    #[test]
    fn the_group_exports_the_texel_names() {
        assert_eq!(procs().len(), 6);
    }
}
