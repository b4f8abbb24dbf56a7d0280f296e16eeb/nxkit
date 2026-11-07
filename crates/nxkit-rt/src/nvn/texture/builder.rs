//! The texture builder: `nvnTextureBuilder*`.
//!
//! An `NVNtextureBuilder` is 0x80 bytes of plain data in guest memory. The layout is private to nxkit:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | the device (u64) |
//! | 8 | the guest address of the `NVNmemoryPool` (u64) |
//! | 0x10 | the offset in the memory pool (i64) |
//! | 0x18 | flags (u32), format (i32) |
//! | 0x20 | width, height, depth, levels (u32 each) |
//! | 0x30 | the stride of a linear texture (u32) |
//! | 0x34 | target, samples, stencil mode, `has_layout` (u8 each) |
//! | 0x38 | the packaged texture layout (8 bytes) |
//! | 0x40 | the guest address of the packaged texture data (u64) |
//! | 0x48 | the swizzle (4 bytes), then 1 when it was set |
//!
//! Setters and getters touch only the guest object. No setter takes a lock or allocates. [`Built::load`] reads the whole
//! builder with one guest read, for `nvnTextureInitialize` and for the size queries.

use nxkit_nvn::layout::{FLAG_SPARSE, Target, TextureDesc};

use crate::nvn::abi::{AbiResult, Obj, guard, other, size, table};

const DEV: u64 = 0;
const POOL: u64 = 8;
const OFF: u64 = 0x10;
const FLAGS: u64 = 0x18;
const FORMAT: u64 = 0x1C;
const WIDTH: u64 = 0x20;
const HEIGHT: u64 = 0x24;
const DEPTH: u64 = 0x28;
const LEVELS: u64 = 0x2C;
const STRIDE: u64 = 0x30;
const TARGET: u64 = 0x34;
const SAMPLES: u64 = 0x35;
const STENCIL: u64 = 0x36;
const HAS_LAYOUT: u64 = 0x37;
const LAYOUT: u64 = 0x38;
const PACKAGED: u64 = 0x40;
const SWIZZLE: u64 = 0x48;
const HAS_SWIZZLE: u64 = 0x4C;
/// The bytes that [`Built::load`] reads.
const USED: usize = 0x50;

/// `NVN_TEXTURE_TARGET_2D`.
const TARGET_2D: u8 = 1;
/// `NVN_TEXTURE_DEPTH_STENCIL_MODE_STENCIL`.
const DEPTH_STENCIL_MODE_STENCIL: i32 = 1;
/// The channels in order R, G, B, A when no swizzle was set (`NVN_TEXTURE_SWIZZLE_R` and so on).
pub(super) const IDENTITY_SWIZZLE: [u8; 4] = [2, 3, 4, 5];

/// What a builder holds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Built {
    /// The texture as the game described it.
    pub(super) desc: TextureDesc,
    /// The guest address of the `NVNmemoryPool`, 0 when no storage was set.
    pub(super) pool: u64,
    /// The offset in the memory pool.
    pub(super) off: i64,
    /// True for `NVN_TEXTURE_DEPTH_STENCIL_MODE_STENCIL`.
    pub(super) stencil: bool,
    /// The swizzle, the identity when none was set.
    pub(super) swizzle: [u8; 4],
    /// The guest address of the packaged texture data, 0 when there is none.
    pub(super) packaged_data: u64,
}

fn word(raw: &[u8; USED], at: u64) -> u32 {
    let at = at as usize;
    u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]])
}

fn quad(raw: &[u8; USED], at: u64) -> u64 {
    let at = at as usize;
    let mut b = [0u8; 8];
    b.copy_from_slice(&raw[at..at + 8]);
    u64::from_le_bytes(b)
}

impl Built {
    /// Reads the builder at `b`.
    pub(super) fn load(b: u64) -> AbiResult<Self> {
        let mut raw = [0u8; USED];
        Obj(b).read(0, &mut raw)?;
        let packaged_block = (raw[HAS_LAYOUT as usize] != 0).then_some(raw[LAYOUT as usize]);
        Ok(Self {
            desc: TextureDesc {
                target: Target::from_raw(raw[TARGET as usize])?,
                format: word(&raw, FORMAT) as i32,
                flags: word(&raw, FLAGS),
                samples: raw[SAMPLES as usize],
                width: word(&raw, WIDTH),
                height: word(&raw, HEIGHT),
                depth: word(&raw, DEPTH),
                levels: word(&raw, LEVELS),
                stride: word(&raw, STRIDE),
                packaged_block,
            },
            pool: quad(&raw, POOL),
            off: quad(&raw, OFF) as i64,
            stencil: raw[STENCIL as usize] != 0,
            swizzle: if raw[HAS_SWIZZLE as usize] != 0 {
                [
                    raw[SWIZZLE as usize],
                    raw[SWIZZLE as usize + 1],
                    raw[SWIZZLE as usize + 2],
                    raw[SWIZZLE as usize + 3],
                ]
            } else {
                IDENTITY_SWIZZLE
            },
            packaged_data: quad(&raw, PACKAGED),
        })
    }
}

/// The description of the texture that a builder holds, for the queries that run before the texture exists.
fn desc_of(b: u64) -> AbiResult<TextureDesc> {
    Ok(Built::load(b)?.desc)
}

/// `NVNtextureSparseTileLayout`: the tiled levels and the tile size, then 16 reserved integers.
pub(super) const SPARSE_TILE_LAYOUT_BYTES: u64 = 32;

/// The sparse tile layout of a texture. A texture that is not sparse has no tiled levels and no tile size. nxkit has no
/// layout for a sparse texture, so a sparse one stops the game here instead of getting a made-up size.
pub(super) fn sparse_tile_layout(desc: &TextureDesc, out: u64) -> AbiResult<()> {
    if desc.flags & FLAG_SPARSE != 0 && desc.target != Target::Buffer {
        return Err(other("a sparse texture has no layout in nxkit"));
    }
    Obj(out).zero(SPARSE_TILE_LAYOUT_BYTES)
}

/// The bytes of a ZCull save area. The host runs no ZCull, but a game allocates the area from its own pool, so the size
/// grows with the texture: 8 bytes for each 32 by 32 tile of each layer, rounded up to 256, at least 256.
pub(super) fn zcull_size(desc: &TextureDesc) -> u64 {
    let tiles = u64::from(desc.width.div_ceil(32))
        * u64::from(desc.height.div_ceil(32))
        * u64::from(desc.depth.max(1));
    if tiles == 0 {
        0x100
    } else {
        (tiles * 8 + 0xff) & !0xff
    }
}

/// Writes the four swizzle channels to the out-pointers that are not null.
pub(super) fn out_swizzle(sw: [u8; 4], out: [u64; 4]) -> AbiResult<()> {
    for (o, v) in out.iter().zip(sw) {
        if *o != 0 {
            Obj(*o).set_i32(0, i32::from(v))?;
        }
    }
    Ok(())
}

extern "C" fn set_defaults(b: u64) {
    guard("nvnTextureBuilderSetDefaults", || {
        let o = Obj(b);
        let dev = o.u64(DEV)?;
        o.zero(size::TEXTURE_BUILDER)?;
        o.set_u64(DEV, dev)?;
        o.write(TARGET, &[TARGET_2D])?;
        o.set_u32(WIDTH, 1)?;
        o.set_u32(HEIGHT, 1)?;
        o.set_u32(DEPTH, 1)?;
        o.set_u32(LEVELS, 1)
    });
}

extern "C" fn set_device(b: u64, dev: u64) {
    guard("nvnTextureBuilderSetDevice", || Obj(b).set_u64(DEV, dev));
}

extern "C" fn set_flags(b: u64, v: i32) {
    guard("nvnTextureBuilderSetFlags", || Obj(b).set_i32(FLAGS, v));
}

extern "C" fn set_target(b: u64, v: i32) {
    guard("nvnTextureBuilderSetTarget", || Obj(b).write(TARGET, &[v as u8]));
}

extern "C" fn set_width(b: u64, v: i32) {
    guard("nvnTextureBuilderSetWidth", || Obj(b).set_i32(WIDTH, v));
}

extern "C" fn set_height(b: u64, v: i32) {
    guard("nvnTextureBuilderSetHeight", || Obj(b).set_i32(HEIGHT, v));
}

extern "C" fn set_depth(b: u64, v: i32) {
    guard("nvnTextureBuilderSetDepth", || Obj(b).set_i32(DEPTH, v));
}

extern "C" fn set_size_1d(b: u64, w: i32) {
    guard("nvnTextureBuilderSetSize1D", || {
        let o = Obj(b);
        o.set_i32(WIDTH, w)?;
        o.set_u32(HEIGHT, 1)?;
        o.set_u32(DEPTH, 1)
    });
}

extern "C" fn set_size_2d(b: u64, w: i32, h: i32) {
    guard("nvnTextureBuilderSetSize2D", || {
        let o = Obj(b);
        o.set_i32(WIDTH, w)?;
        o.set_i32(HEIGHT, h)?;
        o.set_u32(DEPTH, 1)
    });
}

extern "C" fn set_size_3d(b: u64, w: i32, h: i32, d: i32) {
    guard("nvnTextureBuilderSetSize3D", || {
        let o = Obj(b);
        o.set_i32(WIDTH, w)?;
        o.set_i32(HEIGHT, h)?;
        o.set_i32(DEPTH, d)
    });
}

extern "C" fn set_levels(b: u64, v: i32) {
    guard("nvnTextureBuilderSetLevels", || Obj(b).set_i32(LEVELS, v));
}

extern "C" fn set_format(b: u64, v: i32) {
    guard("nvnTextureBuilderSetFormat", || Obj(b).set_i32(FORMAT, v));
}

extern "C" fn set_samples(b: u64, v: i32) {
    guard("nvnTextureBuilderSetSamples", || Obj(b).write(SAMPLES, &[v as u8]));
}

extern "C" fn set_stride(b: u64, v: i64) {
    guard("nvnTextureBuilderSetStride", || Obj(b).set_u32(STRIDE, v as u32));
}

extern "C" fn set_storage(b: u64, pool: u64, off: i64) {
    guard("nvnTextureBuilderSetStorage", || {
        let o = Obj(b);
        o.set_u64(POOL, pool)?;
        o.set_i64(OFF, off)
    });
}

extern "C" fn set_packaged_layout(b: u64, layout: u64) {
    guard("nvnTextureBuilderSetPackagedTextureLayout", || {
        let o = Obj(b);
        if layout == 0 {
            return o.write(HAS_LAYOUT, &[0]);
        }
        let mut l = [0u8; 8];
        Obj(layout).read(0, &mut l)?;
        o.write(LAYOUT, &l)?;
        o.write(HAS_LAYOUT, &[1])
    });
}

extern "C" fn set_packaged_data(b: u64, data: u64) {
    guard("nvnTextureBuilderSetPackagedTextureData", || {
        Obj(b).set_u64(PACKAGED, data)
    });
}

extern "C" fn set_swizzle(b: u64, r: i32, g: i32, bl: i32, a: i32) {
    guard("nvnTextureBuilderSetSwizzle", || {
        let o = Obj(b);
        o.write(SWIZZLE, &[r as u8 & 7, g as u8 & 7, bl as u8 & 7, a as u8 & 7])?;
        o.write(HAS_SWIZZLE, &[1])
    });
}

extern "C" fn set_depth_stencil_mode(b: u64, m: i32) {
    guard("nvnTextureBuilderSetDepthStencilMode", || {
        Obj(b).write(STENCIL, &[u8::from(m == DEPTH_STENCIL_MODE_STENCIL)])
    });
}

extern "C" fn get_flags(b: u64) -> i32 {
    guard("nvnTextureBuilderGetFlags", || Obj(b).i32(FLAGS))
}

extern "C" fn get_target(b: u64) -> i32 {
    guard("nvnTextureBuilderGetTarget", || {
        let mut t = [0u8];
        Obj(b).read(TARGET, &mut t)?;
        Ok(i32::from(t[0]))
    })
}

extern "C" fn get_width(b: u64) -> i32 {
    guard("nvnTextureBuilderGetWidth", || Obj(b).i32(WIDTH))
}

extern "C" fn get_height(b: u64) -> i32 {
    guard("nvnTextureBuilderGetHeight", || Obj(b).i32(HEIGHT))
}

extern "C" fn get_depth(b: u64) -> i32 {
    guard("nvnTextureBuilderGetDepth", || Obj(b).i32(DEPTH))
}

extern "C" fn get_levels(b: u64) -> i32 {
    guard("nvnTextureBuilderGetLevels", || Obj(b).i32(LEVELS))
}

extern "C" fn get_format(b: u64) -> i32 {
    guard("nvnTextureBuilderGetFormat", || Obj(b).i32(FORMAT))
}

extern "C" fn get_samples(b: u64) -> i32 {
    guard("nvnTextureBuilderGetSamples", || {
        let mut s = [0u8];
        Obj(b).read(SAMPLES, &mut s)?;
        Ok(i32::from(s[0]))
    })
}

extern "C" fn get_stride(b: u64) -> i64 {
    guard("nvnTextureBuilderGetStride", || {
        Ok(i64::from(Obj(b).u32(STRIDE)?))
    })
}

extern "C" fn get_memory_pool(b: u64) -> u64 {
    guard("nvnTextureBuilderGetMemoryPool", || Obj(b).u64(POOL))
}

extern "C" fn get_memory_offset(b: u64) -> i64 {
    guard("nvnTextureBuilderGetMemoryOffset", || Obj(b).i64(OFF))
}

extern "C" fn get_swizzle(b: u64, r: u64, g: u64, bl: u64, a: u64) {
    guard("nvnTextureBuilderGetSwizzle", || {
        out_swizzle(Built::load(b)?.swizzle, [r, g, bl, a])
    });
}

extern "C" fn get_storage_class(b: u64) -> i32 {
    guard("nvnTextureBuilderGetStorageClass", || {
        Obj(b).u64(DEV)?;
        Ok(0)
    })
}

extern "C" fn get_raw_storage_class(b: u64) -> i32 {
    guard("nvnTextureBuilderGetRawStorageClass", || {
        Obj(b).u64(DEV)?;
        Ok(1)
    })
}

extern "C" fn get_storage_size(b: u64) -> u64 {
    guard("nvnTextureBuilderGetStorageSize", || {
        Ok(desc_of(b)?.layout()?.size)
    })
}

extern "C" fn get_storage_alignment(b: u64) -> u64 {
    guard("nvnTextureBuilderGetStorageAlignment", || {
        Ok(u64::from(desc_of(b)?.layout()?.align))
    })
}

extern "C" fn get_zcull_storage_size(b: u64) -> u64 {
    guard("nvnTextureBuilderGetZCullStorageSize", || {
        Ok(zcull_size(&desc_of(b)?))
    })
}

extern "C" fn get_sparse_tile_layout(b: u64, out: u64) {
    guard("nvnTextureBuilderGetSparseTileLayout", || {
        sparse_tile_layout(&desc_of(b)?, out)
    });
}

extern "C" fn get_view_offset(b: u64, view: u64) -> i64 {
    guard("nvnTextureBuilderGetViewOffset", || {
        super::view::offset(&desc_of(b)?, view)
    })
}

/// The functions of the builder.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnTextureBuilderSetDefaults" => set_defaults as extern "C" fn(u64),
        "nvnTextureBuilderSetDevice" => set_device as extern "C" fn(u64, u64),
        "nvnTextureBuilderSetFlags" => set_flags as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetTarget" => set_target as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetWidth" => set_width as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetHeight" => set_height as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetDepth" => set_depth as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetSize1D" => set_size_1d as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetSize2D" => set_size_2d as extern "C" fn(u64, i32, i32),
        "nvnTextureBuilderSetSize3D" => set_size_3d as extern "C" fn(u64, i32, i32, i32),
        "nvnTextureBuilderSetLevels" => set_levels as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetFormat" => set_format as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetSamples" => set_samples as extern "C" fn(u64, i32),
        "nvnTextureBuilderSetStride" => set_stride as extern "C" fn(u64, i64),
        "nvnTextureBuilderSetStorage" => set_storage as extern "C" fn(u64, u64, i64),
        "nvnTextureBuilderSetPackagedTextureLayout" => set_packaged_layout as extern "C" fn(u64, u64),
        "nvnTextureBuilderSetPackagedTextureData" => set_packaged_data as extern "C" fn(u64, u64),
        "nvnTextureBuilderSetSwizzle" => set_swizzle as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnTextureBuilderSetDepthStencilMode" => set_depth_stencil_mode as extern "C" fn(u64, i32),
        "nvnTextureBuilderGetFlags" => get_flags as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetTarget" => get_target as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetWidth" => get_width as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetHeight" => get_height as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetDepth" => get_depth as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetLevels" => get_levels as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetFormat" => get_format as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetSamples" => get_samples as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetStride" => get_stride as extern "C" fn(u64) -> i64,
        "nvnTextureBuilderGetMemoryPool" => get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnTextureBuilderGetMemoryOffset" => get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnTextureBuilderGetSwizzle" => get_swizzle as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnTextureBuilderGetStorageClass" => get_storage_class as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetRawStorageClass" => get_raw_storage_class as extern "C" fn(u64) -> i32,
        "nvnTextureBuilderGetStorageSize" => get_storage_size as extern "C" fn(u64) -> u64,
        "nvnTextureBuilderGetStorageAlignment" => get_storage_alignment as extern "C" fn(u64) -> u64,
        "nvnTextureBuilderGetZCullStorageSize" => get_zcull_storage_size as extern "C" fn(u64) -> u64,
        "nvnTextureBuilderGetSparseTileLayout" => get_sparse_tile_layout as extern "C" fn(u64, u64),
        "nvnTextureBuilderGetViewOffset" => get_view_offset as extern "C" fn(u64, u64) -> i64,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn the_defaults_make_a_one_texel_2d_texture_and_keep_the_device() {
        let b = testing::alloc(0x80);
        Obj(b).set_u64(DEV, 0xD0).expect("device");
        set_width(b, 99);
        set_defaults(b);
        let built = Built::load(b).expect("load");
        assert_eq!(Obj(b).u64(DEV).expect("device"), 0xD0);
        assert_eq!(built.desc.target, Target::D2);
        assert_eq!(
            (built.desc.width, built.desc.height, built.desc.depth, built.desc.levels),
            (1, 1, 1, 1)
        );
        assert_eq!(built.swizzle, IDENTITY_SWIZZLE);
        assert_eq!(built.pool, 0);
    }

    #[test]
    fn the_setters_fill_the_description() {
        let b = testing::alloc(0x80);
        set_defaults(b);
        set_size_3d(b, 8, 4, 2);
        set_target(b, 2);
        set_format(b, 0x25);
        set_levels(b, 3);
        set_flags(b, 0x10);
        set_stride(b, 256);
        set_samples(b, 4);
        set_storage(b, 0xAA00, 0x200);
        set_swizzle(b, 5, 4, 3, 2);
        set_depth_stencil_mode(b, 1);
        let l = testing::alloc(8);
        Obj(l).write(0, &[0x23, 0, 0, 0, 0, 0, 0, 0]).expect("layout");
        set_packaged_layout(b, l);
        let built = Built::load(b).expect("load");
        assert_eq!(built.desc.target, Target::D3);
        assert_eq!(
            (built.desc.width, built.desc.height, built.desc.depth, built.desc.levels),
            (8, 4, 2, 3)
        );
        assert_eq!((built.desc.format, built.desc.flags, built.desc.stride), (0x25, 0x10, 256));
        assert_eq!(built.desc.samples, 4);
        assert_eq!((built.pool, built.off), (0xAA00, 0x200));
        assert_eq!(built.swizzle, [5, 4, 3, 2]);
        assert!(built.stencil);
        assert_eq!(built.desc.packaged_block, Some(0x23));
        set_packaged_layout(b, 0);
        assert_eq!(Built::load(b).expect("load").desc.packaged_block, None);
    }

    #[test]
    fn size_1d_and_2d_reset_the_dimensions_that_they_do_not_have() {
        let b = testing::alloc(0x80);
        set_defaults(b);
        set_size_3d(b, 9, 9, 9);
        set_size_2d(b, 5, 6);
        assert_eq!((get_width(b), get_height(b), get_depth(b)), (5, 6, 1));
        set_size_1d(b, 7);
        assert_eq!((get_width(b), get_height(b), get_depth(b)), (7, 1, 1));
    }

    #[test]
    fn the_swizzle_getter_skips_null_outputs() {
        let b = testing::alloc(0x80);
        set_defaults(b);
        let r = testing::alloc(4);
        get_swizzle(b, r, 0, 0, 0);
        assert_eq!(Obj(r).i32(0).expect("r"), 2);
    }

    #[test]
    fn the_zcull_size_grows_with_the_texture_and_is_at_least_256() {
        let mut d = Built::load({
            let b = testing::alloc(0x80);
            set_defaults(b);
            b
        })
        .expect("load")
        .desc;
        assert_eq!(zcull_size(&d), 0x100);
        d.width = 1024;
        d.height = 1024;
        assert_eq!(zcull_size(&d), 32 * 32 * 8);
        d.width = 0;
        assert_eq!(zcull_size(&d), 0x100);
    }

    #[test]
    fn a_texture_that_is_not_sparse_has_no_tiled_levels() {
        let b = testing::alloc(0x80);
        set_defaults(b);
        let out = testing::alloc(0x20);
        Obj(out).set_u32(0, 7).expect("dirty");
        get_sparse_tile_layout(b, out);
        assert_eq!(Obj(out).u32(0).expect("levels"), 0);
    }
}
