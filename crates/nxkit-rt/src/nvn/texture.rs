//! The texture group: `nvnTexture*`, the texture builder, the texture views, the texel access and the texture pool.
//!
//! The parts are in the modules below: the builder in [`builder`], the views in [`view`], the texel access in [`io`],
//! the descriptor pools in [`pool`] and the texture pool in [`tpool`]. This module has the texture itself.
//!
//! An `NVNtexture` is 0xC0 bytes in the headers and the game treats it as opaque. nxkit keeps its own copy of what
//! it needs in the first 0x5C bytes, so every getter reads the guest object and nothing else. No getter takes a lock
//! and no registration looks a texture up by address:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `TEX\0` |
//! | 4 | the `TexId` of the front end |
//! | 8 | memory pool (u64), 0x10 offset (i64) |
//! | 0x18 | GPU address of the storage, 0x20 host address of the storage, 0x28 size, 0x30 size of a layer (u64) |
//! | 0x38 | flags, format, width, height, depth, levels, stride (u32 each) |
//! | 0x54 | target, samples, stencil mode, packaged block (u8 each, 0xFF for none) |
//! | 0x58 | swizzle (4 x u8) |
//!
//! The game may copy the object by value. The copy names the same texture, because the id is in the object. The front
//! end only knows the address that `nvnTextureInitialize` got, so a finalize of a copy is ignored.

pub(in crate::nvn) mod pool;

mod builder;
mod io;
mod tpool;
mod view;

use nxkit_abi::guest;
use nxkit_nvn::layout::{Target, TextureDesc};
use nxkit_nvn::resource::TexId;

use super::abi::{AbiResult, Obj, guard, magic, other, ret_bool, table};
use super::ctx::gpu;
use super::pool as memory_pool;
use builder::{Built, out_swizzle, sparse_tile_layout, zcull_size};

pub(crate) use tpool::pool_base;

/// The magic of an initialised texture.
const TEX: u32 = magic(b'T', b'E', b'X', 0);

const MAGIC: u64 = 0;
const ID: u64 = 4;
const POOL: u64 = 8;
const OFF: u64 = 0x10;
const ADDRESS: u64 = 0x18;
const HOST: u64 = 0x20;
const SIZE: u64 = 0x28;
const LAYER_SIZE: u64 = 0x30;
const FLAGS: u64 = 0x38;
const FORMAT: u64 = 0x3C;
const WIDTH: u64 = 0x40;
const HEIGHT: u64 = 0x44;
const DEPTH: u64 = 0x48;
const LEVELS: u64 = 0x4C;
const STRIDE: u64 = 0x50;
const TARGET: u64 = 0x54;
const SAMPLES: u64 = 0x55;
const STENCIL: u64 = 0x56;
const PACKAGED: u64 = 0x57;
const SWIZZLE: u64 = 0x58;
/// The bytes that nxkit uses of the object.
const USED: usize = 0x5C;
/// The part of the object that nxkit clears. The game may use no more than this of an `NVNtexture`.
const CLEARED: u64 = 0x80;
const NO_PACKAGED_BLOCK: u8 = 0xFF;

/// What a texture object holds.
#[derive(Clone, Copy, Debug)]
pub(super) struct Tex {
    pub(super) id: TexId,
    pool: u64,
    off: i64,
    pub(super) host: u64,
    pub(super) size: u64,
    pub(super) layer_size: u64,
    pub(super) desc: TextureDesc,
    pub(super) stencil: bool,
    pub(super) swizzle: [u8; 4],
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

impl Tex {
    /// Reads the texture at `addr`. A texture that was never initialised is an error.
    pub(super) fn load(addr: u64) -> AbiResult<Self> {
        let mut raw = [0u8; USED];
        Obj(addr).read(0, &mut raw)?;
        if word(&raw, MAGIC) != TEX {
            return Err(other(format!("{addr:#x} is not an initialised texture")));
        }
        let packaged = raw[PACKAGED as usize];
        Ok(Self {
            id: TexId(word(&raw, ID)),
            pool: quad(&raw, POOL),
            off: quad(&raw, OFF) as i64,
            host: quad(&raw, HOST),
            size: quad(&raw, SIZE),
            layer_size: quad(&raw, LAYER_SIZE),
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
                packaged_block: (packaged != NO_PACKAGED_BLOCK).then_some(packaged),
            },
            stencil: raw[STENCIL as usize] != 0,
            swizzle: [
                raw[SWIZZLE as usize],
                raw[SWIZZLE as usize + 1],
                raw[SWIZZLE as usize + 2],
                raw[SWIZZLE as usize + 3],
            ],
        })
    }

    /// Writes the texture to the object at `addr`. `address` is the GPU address of the storage.
    fn store(&self, addr: u64, address: u64) -> AbiResult<()> {
        let mut raw = [0u8; USED];
        let mut put = |at: u64, bytes: &[u8]| {
            raw[at as usize..at as usize + bytes.len()].copy_from_slice(bytes);
        };
        put(MAGIC, &TEX.to_le_bytes());
        put(ID, &self.id.0.to_le_bytes());
        put(POOL, &self.pool.to_le_bytes());
        put(OFF, &self.off.to_le_bytes());
        put(ADDRESS, &address.to_le_bytes());
        put(HOST, &self.host.to_le_bytes());
        put(SIZE, &self.size.to_le_bytes());
        put(LAYER_SIZE, &self.layer_size.to_le_bytes());
        put(FLAGS, &self.desc.flags.to_le_bytes());
        put(FORMAT, &(self.desc.format as u32).to_le_bytes());
        put(WIDTH, &self.desc.width.to_le_bytes());
        put(HEIGHT, &self.desc.height.to_le_bytes());
        put(DEPTH, &self.desc.depth.to_le_bytes());
        put(LEVELS, &self.desc.levels.to_le_bytes());
        put(STRIDE, &self.desc.stride.to_le_bytes());
        put(
            TARGET,
            &[
                self.desc.target as u8,
                self.desc.samples,
                u8::from(self.stencil),
                self.desc.packaged_block.unwrap_or(NO_PACKAGED_BLOCK),
            ],
        );
        put(SWIZZLE, &self.swizzle);
        let o = Obj(addr);
        o.zero(CLEARED)?;
        o.write(0, &raw)
    }
}

/// Tells the front end and the backend that the texture at `addr` is gone, when `nvnTextureInitialize` made it there.
fn release(name: &str, addr: u64) -> AbiResult<()> {
    let g = gpu(name);
    if let Some(id) = g.textures.remove(addr) {
        g.with_front(|f| f.finalize_texture(id))?;
        g.backend().texture_finalized(id);
    }
    Ok(())
}

extern "C" fn initialize(t: u64, b: u64) -> u32 {
    guard("nvnTextureInitialize", || {
        let name = "nvnTextureInitialize";
        let built = Built::load(b)?;
        let o = Obj(t);
        if o.u32(MAGIC)? == TEX {
            release(name, t)?;
        }
        if built.pool == 0 {
            eprintln!("[RED] NVN {name}: the builder has no memory pool");
            return Ok(0);
        }
        let p = memory_pool::view(built.pool)?;
        let layout = built.desc.layout()?;
        if p.mem == 0 {
            eprintln!(
                "[RED] NVN {name}: the memory pool has no storage (pool {:#x}, flags {:#x})",
                built.pool, p.flags
            );
            return Ok(0);
        }
        if built.off < 0
            || (built.off as u64)
                .checked_add(layout.size)
                .is_none_or(|end| end > p.size)
        {
            eprintln!(
                "[RED] NVN {name}: a texture of {:#x} bytes at offset {:#x} does not fit the memory pool of {:#x} bytes",
                layout.size, built.off, p.size
            );
            return Ok(0);
        }
        let host = p.mem + built.off as u64;
        let g = gpu(name);
        let (id, address) = g.with_front(|f| {
            let id = f.create_texture(built.desc, host)?;
            let record = f.resources().get(id)?;
            g.backend().texture_created(record);
            Ok((id, record.address.get()))
        })?;
        g.textures.insert(t, id);
        if built.desc.packaged_block.is_some() && built.packaged_data != 0 {
            guest::with_slice(built.packaged_data, layout.size, |data| {
                guest::write(host, data)
            })??;
            g.with_front(|f| f.guest_wrote(host, layout.size).map(|_| ()))?;
        }
        Tex {
            id,
            pool: built.pool,
            off: built.off,
            host,
            size: layout.size,
            layer_size: layout.layer_size,
            desc: built.desc,
            stencil: built.stencil,
            swizzle: built.swizzle,
        }
        .store(t, address)?;
        Ok(1)
    })
}

extern "C" fn finalize(t: u64) {
    guard("nvnTextureFinalize", || {
        release("nvnTextureFinalize", t)?;
        Obj(t).set_u32(MAGIC, 0)
    });
}

extern "C" fn compare(a: u64, b: u64) -> u32 {
    guard("nvnTextureCompare", || {
        let (x, y) = (Tex::load(a)?, Tex::load(b)?);
        Ok(ret_bool(
            x.desc == y.desc && x.host == y.host && x.swizzle == y.swizzle,
        ))
    })
}

/// Reads one field of the texture, without decoding the rest of the object.
fn field<T>(t: u64, read: impl FnOnce(Obj) -> AbiResult<T>) -> AbiResult<T> {
    let o = Obj(t);
    if o.u32(MAGIC)? != TEX {
        return Err(other(format!("{t:#x} is not an initialised texture")));
    }
    read(o)
}

macro_rules! getter {
    ($f:ident, $name:literal, $ret:ty, $at:expr, $read:ident, $conv:expr) => {
        extern "C" fn $f(t: u64) -> $ret {
            guard($name, || field(t, |o| o.$read($at)).map($conv))
        }
    };
}

getter!(get_flags, "nvnTextureGetFlags", i32, FLAGS, u32, |v| v as i32);
getter!(get_width, "nvnTextureGetWidth", i32, WIDTH, u32, |v| v as i32);
getter!(get_height, "nvnTextureGetHeight", i32, HEIGHT, u32, |v| v as i32);
getter!(get_depth, "nvnTextureGetDepth", i32, DEPTH, u32, |v| v as i32);
getter!(get_levels, "nvnTextureGetLevels", i32, LEVELS, u32, |v| v as i32);
getter!(get_format, "nvnTextureGetFormat", i32, FORMAT, i32, |v| v);
getter!(get_stride, "nvnTextureGetStride", i64, STRIDE, u32, i64::from);
getter!(get_memory_pool, "nvnTextureGetMemoryPool", u64, POOL, u64, |v| v);
getter!(get_memory_offset, "nvnTextureGetMemoryOffset", i64, OFF, i64, |v| v);
getter!(get_texture_address, "nvnTextureGetTextureAddress", u64, ADDRESS, u64, |v| v);
getter!(get_storage_size, "nvnTextureGetStorageSize", i32, SIZE, u64, |v| v as i32);

fn byte(t: u64, at: u64) -> AbiResult<i32> {
    field(t, |o| {
        let mut b = [0u8; 1];
        o.read(at, &mut b)?;
        Ok(i32::from(b[0]))
    })
}

extern "C" fn get_target(t: u64) -> i32 {
    guard("nvnTextureGetTarget", || byte(t, TARGET))
}

extern "C" fn get_samples(t: u64) -> i32 {
    guard("nvnTextureGetSamples", || byte(t, SAMPLES))
}

extern "C" fn get_depth_stencil_mode(t: u64) -> i32 {
    guard("nvnTextureGetDepthStencilMode", || byte(t, STENCIL))
}

extern "C" fn get_storage_class(t: u64) -> i32 {
    guard("nvnTextureGetStorageClass", || field(t, |_| Ok(0)))
}

extern "C" fn get_raw_storage_class(t: u64) -> i32 {
    guard("nvnTextureGetRawStorageClass", || field(t, |_| Ok(1)))
}

extern "C" fn get_swizzle(t: u64, r: u64, g: u64, b: u64, a: u64) {
    guard("nvnTextureGetSwizzle", || {
        out_swizzle(Tex::load(t)?.swizzle, [r, g, b, a])
    });
}

extern "C" fn get_zcull_storage_size(t: u64) -> i32 {
    guard("nvnTextureGetZCullStorageSize", || {
        Ok(zcull_size(&Tex::load(t)?.desc) as i32)
    })
}

extern "C" fn get_sparse_tile_layout(t: u64, out: u64) {
    guard("nvnTextureGetSparseTileLayout", || {
        sparse_tile_layout(&Tex::load(t)?.desc, out)
    });
}

extern "C" fn get_view_offset(t: u64, v: u64) -> i64 {
    guard("nvnTextureGetViewOffset", || {
        view::offset(&Tex::load(t)?.desc, v)
    })
}

extern "C" fn set_debug_label(_t: u64, _label: u64) {}

/// The id that the front end gave the texture at `addr`, for the command buffer.
pub(crate) fn id_of(addr: u64) -> AbiResult<u32> {
    field(addr, |o| o.u32(ID))
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    let mut all = builder::procs();
    all.extend(view::procs());
    all.extend(io::procs());
    all.extend(tpool::procs());
    all.extend(table![
        "nvnTextureInitialize" => initialize as extern "C" fn(u64, u64) -> u32,
        "nvnTextureFinalize" => finalize as extern "C" fn(u64),
        "nvnTextureCompare" => compare as extern "C" fn(u64, u64) -> u32,
        "nvnTextureGetFlags" => get_flags as extern "C" fn(u64) -> i32,
        "nvnTextureGetWidth" => get_width as extern "C" fn(u64) -> i32,
        "nvnTextureGetHeight" => get_height as extern "C" fn(u64) -> i32,
        "nvnTextureGetDepth" => get_depth as extern "C" fn(u64) -> i32,
        "nvnTextureGetLevels" => get_levels as extern "C" fn(u64) -> i32,
        "nvnTextureGetFormat" => get_format as extern "C" fn(u64) -> i32,
        "nvnTextureGetTarget" => get_target as extern "C" fn(u64) -> i32,
        "nvnTextureGetSamples" => get_samples as extern "C" fn(u64) -> i32,
        "nvnTextureGetStride" => get_stride as extern "C" fn(u64) -> i64,
        "nvnTextureGetStorageSize" => get_storage_size as extern "C" fn(u64) -> i32,
        "nvnTextureGetMemoryPool" => get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnTextureGetMemoryOffset" => get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnTextureGetTextureAddress" => get_texture_address as extern "C" fn(u64) -> u64,
        "nvnTextureGetSwizzle" => get_swizzle as extern "C" fn(u64, u64, u64, u64, u64),
        "nvnTextureGetDepthStencilMode" => get_depth_stencil_mode as extern "C" fn(u64) -> i32,
        "nvnTextureGetStorageClass" => get_storage_class as extern "C" fn(u64) -> i32,
        "nvnTextureGetRawStorageClass" => get_raw_storage_class as extern "C" fn(u64) -> i32,
        "nvnTextureGetZCullStorageSize" => get_zcull_storage_size as extern "C" fn(u64) -> i32,
        "nvnTextureGetSparseTileLayout" => get_sparse_tile_layout as extern "C" fn(u64, u64),
        "nvnTextureGetViewOffset" => get_view_offset as extern "C" fn(u64, u64) -> i64,
        "nvnTextureSetDebugLabel" => set_debug_label as extern "C" fn(u64, u64),
    ]);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::{size, testing};

    fn desc() -> TextureDesc {
        TextureDesc {
            target: Target::D2,
            format: 0x25,
            flags: 0x10,
            samples: 0,
            width: 64,
            height: 32,
            depth: 1,
            levels: 1,
            stride: 512,
            packaged_block: None,
        }
    }

    fn tex() -> Tex {
        Tex {
            id: TexId(9),
            pool: 0x1000,
            off: 0x40,
            host: 0x2000,
            size: 0x4000,
            layer_size: 0x4000,
            desc: desc(),
            stencil: true,
            swizzle: [5, 4, 3, 2],
        }
    }

    #[test]
    fn the_object_keeps_what_the_texture_was_built_with() {
        let o = testing::alloc(size::TEXTURE);
        tex().store(o, 0x7000_0040).expect("store");
        let t = Tex::load(o).expect("load");
        assert_eq!(t.id, TexId(9));
        assert_eq!(t.desc, desc());
        assert_eq!((t.host, t.size, t.layer_size), (0x2000, 0x4000, 0x4000));
        assert_eq!(t.swizzle, [5, 4, 3, 2]);
        assert!(t.stencil);
    }

    #[test]
    fn the_getters_read_the_object() {
        let o = testing::alloc(size::TEXTURE);
        tex().store(o, 0x7000_0040).expect("store");
        assert_eq!(get_width(o), 64);
        assert_eq!(get_height(o), 32);
        assert_eq!(get_flags(o), 0x10);
        assert_eq!(get_stride(o), 512);
        assert_eq!(get_format(o), 0x25);
        assert_eq!(get_target(o), 1);
        assert_eq!(get_memory_pool(o), 0x1000);
        assert_eq!(get_memory_offset(o), 0x40);
        assert_eq!(get_texture_address(o), 0x7000_0040);
        assert_eq!(get_storage_size(o), 0x4000);
        assert_eq!(get_depth_stencil_mode(o), 1);
        assert_eq!(id_of(o).expect("id"), 9);
    }

    #[test]
    fn a_copy_of_the_object_is_the_same_texture() {
        let a = testing::alloc(size::TEXTURE);
        let b = testing::alloc(size::TEXTURE);
        tex().store(a, 0x7000_0040).expect("store");
        let mut raw = [0u8; CLEARED as usize];
        Obj(a).read(0, &mut raw).expect("read");
        Obj(b).write(0, &raw).expect("write");
        assert_eq!(compare(a, b), 1);
        assert_eq!(id_of(b).expect("id"), 9);
    }

    #[test]
    fn compare_tells_a_moved_texture_apart() {
        let a = testing::alloc(size::TEXTURE);
        let b = testing::alloc(size::TEXTURE);
        tex().store(a, 0x7000_0040).expect("store");
        Tex {
            host: 0x3000,
            ..tex()
        }
        .store(b, 0x7000_0040)
        .expect("store");
        assert_eq!(compare(a, b), 0);
    }

    #[test]
    fn the_swizzle_getter_skips_null_outputs() {
        let o = testing::alloc(size::TEXTURE);
        tex().store(o, 0x7000_0040).expect("store");
        let r = testing::alloc(4);
        get_swizzle(o, r, 0, 0, 0);
        assert_eq!(Obj(r).i32(0).expect("r"), 5);
    }

    #[test]
    fn a_texture_that_was_never_initialised_is_an_error() {
        let o = testing::alloc(size::TEXTURE);
        assert!(Tex::load(o).is_err());
        assert!(id_of(o).is_err());
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let mut have: Vec<&str> = procs().iter().map(|(n, _)| *n).collect();
        have.sort_unstable();
        let mut dup = have.clone();
        dup.dedup();
        assert_eq!(dup.len(), have.len(), "a name is listed twice");
        let mut want: Vec<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "texture")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        want.sort_unstable();
        assert_eq!(have, want);
    }
}
