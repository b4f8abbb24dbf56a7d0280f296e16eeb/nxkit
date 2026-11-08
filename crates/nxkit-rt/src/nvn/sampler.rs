//! The sampler group: `nvnSampler*`, the sampler builder and the sampler pool.
//!
//! A sampler builder is 0x60 bytes in the headers and a sampler is 0x30 bytes. A game keeps a builder in a stack slot of
//! at most 0x48 bytes. The host writes the first 0x30 bytes of a builder and the first 0x2C bytes of a sampler. Both hold
//! the same 40 bytes of parameters, at offset 8 in the builder (the device is at 0) and at offset 4 in the sampler (the
//! magic `SMP\0` is at 0):
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | min filter, mag filter, wrap S, wrap T, wrap R, compare mode, compare func, reduction (one byte each) |
//! | 8 | lod min, lod max, lod bias, max anisotropy (f32) |
//! | 24 | border colour (4 x f32) |
//!
//! The values are the `NVN*` enum values, which is what [`Tsc`] holds. A registration reads the 40 bytes once, builds
//! the [`Tsc`], writes its 32 bytes into the pool memory of the game and hands it to the front end and the backend.
//! No registration allocates. The pool objects are in [`super::texture::pool`].
//!
//! The front end cannot drop a sampler slot. A bit for each id says which slots hold a sampler, so a slot that is
//! registered again, and a pool that is finalised, tell the backend `sampler_released` for the slots that were live.
//! The bits are atomics, so a registration takes no lock besides the one of the front end.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

use nxkit_abi::guest;
use nxkit_nvn::tic::Tsc;

use super::abi::{AbiResult, Obj, guard, magic, table, used};
use super::ctx::gpu;
use super::texture::pool::{self, DescPools};

/// The magic of an initialised sampler.
const SMP: u32 = magic(b'S', b'M', b'P', 0);
/// The magic of an initialised sampler pool.
const SPL: u32 = magic(b'S', b'P', b'L', 0);

const B_DEV: u64 = 0;
/// The offset of the parameters in the builder and in the sampler.
const B_PARAMS: u64 = 8;
const S_PARAMS: u64 = 4;
/// The bytes of the parameters.
const PARAMS: usize = 40;

const P_MIN: usize = 0;
const P_MAG: usize = 1;
const P_WRAP_S: usize = 2;
const P_WRAP_T: usize = 3;
const P_WRAP_R: usize = 4;
const P_CMP_MODE: usize = 5;
const P_CMP_FUNC: usize = 6;
const P_REDUCTION: usize = 7;
const P_LOD_MIN: usize = 8;
const P_LOD_MAX: usize = 12;
const P_LOD_BIAS: usize = 16;
const P_ANISO: usize = 20;
const P_BORDER: usize = 24;

/// `NVN_MIN_FILTER_NEAREST_MIPMAP_LINEAR`, the default of the builder.
const MIN_NEAREST_MIPMAP_LINEAR: u8 = 4;
/// `NVN_MAG_FILTER_LINEAR`.
const MAG_LINEAR: u8 = 1;
/// `NVN_WRAP_MODE_REPEAT`.
const WRAP_REPEAT: u8 = 1;
/// `NVN_COMPARE_FUNC_LESS`.
const COMPARE_LESS: u8 = 2;

static POOLS: LazyLock<DescPools> = LazyLock::new(|| DescPools::new(SPL));

/// The largest id of a sampler (12 bits).
const IDS: usize = 4096;
static LIVE: [AtomicU64; IDS / 64] = [const { AtomicU64::new(0) }; IDS / 64];

/// Sets the bit of `id`. True when it was set before.
fn live_set(id: u32) -> bool {
    LIVE.get(id as usize / 64).is_some_and(|w| {
        let bit = 1u64 << (id % 64);
        w.fetch_or(bit, Ordering::Relaxed) & bit != 0
    })
}

/// Clears the bit of `id`. True when it was set.
fn live_clear(id: u32) -> bool {
    LIVE.get(id as usize / 64).is_some_and(|w| {
        let bit = 1u64 << (id % 64);
        w.fetch_and(!bit, Ordering::Relaxed) & bit != 0
    })
}

/// The first id of the sampler pool at `pool` in the id space of the samplers. The command buffer adds it to the
/// sampler ids that it records after `nvnCommandBufferSetSamplerPool`.
pub(crate) fn pool_base(pool: u64) -> AbiResult<u32> {
    POOLS.base(pool)
}

fn load(addr: u64, off: u64) -> AbiResult<[u8; PARAMS]> {
    let mut p = [0u8; PARAMS];
    Obj(addr).read(off, &mut p)?;
    Ok(p)
}

fn f32_at(p: &[u8; PARAMS], at: usize) -> f32 {
    f32::from_le_bytes([p[at], p[at + 1], p[at + 2], p[at + 3]])
}

fn put_f32(p: &mut [u8; PARAMS], at: usize, v: f32) {
    p[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn tsc_of(p: &[u8; PARAMS]) -> Tsc {
    Tsc {
        min_filter: p[P_MIN],
        mag_filter: p[P_MAG],
        wrap_s: p[P_WRAP_S],
        wrap_t: p[P_WRAP_T],
        wrap_r: p[P_WRAP_R],
        compare_mode: p[P_CMP_MODE],
        compare_func: p[P_CMP_FUNC],
        reduction: p[P_REDUCTION],
        lod_min: f32_at(p, P_LOD_MIN),
        lod_max: f32_at(p, P_LOD_MAX),
        lod_bias: f32_at(p, P_LOD_BIAS),
        aniso: f32_at(p, P_ANISO),
        border: [
            f32_at(p, P_BORDER),
            f32_at(p, P_BORDER + 4),
            f32_at(p, P_BORDER + 8),
            f32_at(p, P_BORDER + 12),
        ],
    }
}

/// Changes one field of the parameters in guest memory.
fn edit(addr: u64, off: u64, f: impl FnOnce(&mut [u8; PARAMS])) -> AbiResult<()> {
    let mut p = load(addr, off)?;
    f(&mut p);
    Obj(addr).write(off, &p)
}

extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnSamplerBuilderSetDefaults", || {
        let o = Obj(b);
        let dev = o.u64(B_DEV)?;
        o.zero(used::SAMPLER_BUILDER)?;
        o.set_u64(B_DEV, dev)?;
        edit(b, B_PARAMS, |p| {
            p[P_MIN] = MIN_NEAREST_MIPMAP_LINEAR;
            p[P_MAG] = MAG_LINEAR;
            p[P_WRAP_S] = WRAP_REPEAT;
            p[P_WRAP_T] = WRAP_REPEAT;
            p[P_WRAP_R] = WRAP_REPEAT;
            p[P_CMP_FUNC] = COMPARE_LESS;
            put_f32(p, P_LOD_MIN, -1000.0);
            put_f32(p, P_LOD_MAX, 1000.0);
            put_f32(p, P_ANISO, 1.0);
        })
    });
}

extern "C" fn builder_set_device(b: u64, dev: u64) {
    guard("nvnSamplerBuilderSetDevice", || Obj(b).set_u64(B_DEV, dev));
}

extern "C" fn builder_set_min_mag_filter(b: u64, min: i32, mag: i32) {
    guard("nvnSamplerBuilderSetMinMagFilter", || {
        edit(b, B_PARAMS, |p| {
            p[P_MIN] = min as u8;
            p[P_MAG] = mag as u8;
        })
    });
}

extern "C" fn builder_set_wrap_mode(b: u64, s: i32, t: i32, r: i32) {
    guard("nvnSamplerBuilderSetWrapMode", || {
        edit(b, B_PARAMS, |p| {
            p[P_WRAP_S] = s as u8;
            p[P_WRAP_T] = t as u8;
            p[P_WRAP_R] = r as u8;
        })
    });
}

extern "C" fn builder_set_lod_clamp(b: u64, lo: f32, hi: f32) {
    guard("nvnSamplerBuilderSetLodClamp", || {
        edit(b, B_PARAMS, |p| {
            put_f32(p, P_LOD_MIN, lo);
            put_f32(p, P_LOD_MAX, hi);
        })
    });
}

extern "C" fn builder_set_lod_bias(b: u64, v: f32) {
    guard("nvnSamplerBuilderSetLodBias", || {
        edit(b, B_PARAMS, |p| put_f32(p, P_LOD_BIAS, v))
    });
}

extern "C" fn builder_set_compare(b: u64, mode: i32, func: i32) {
    guard("nvnSamplerBuilderSetCompare", || {
        edit(b, B_PARAMS, |p| {
            p[P_CMP_MODE] = mode as u8;
            p[P_CMP_FUNC] = func as u8;
        })
    });
}

extern "C" fn builder_set_border_color(b: u64, c: u64) {
    guard("nvnSamplerBuilderSetBorderColor", || {
        let mut v = [0u8; 16];
        Obj(c).read(0, &mut v)?;
        edit(b, B_PARAMS, |p| p[P_BORDER..P_BORDER + 16].copy_from_slice(&v))
    });
}

extern "C" fn builder_set_border_colori(b: u64, c: u64) {
    guard("nvnSamplerBuilderSetBorderColori", || {
        let o = Obj(c);
        let v = [o.i32(0)? as f32, o.i32(4)? as f32, o.i32(8)? as f32, o.i32(12)? as f32];
        edit(b, B_PARAMS, |p| {
            v.iter().enumerate().for_each(|(i, x)| put_f32(p, P_BORDER + 4 * i, *x));
        })
    });
}

extern "C" fn builder_set_border_colorui(b: u64, c: u64) {
    guard("nvnSamplerBuilderSetBorderColorui", || {
        let o = Obj(c);
        let v = [o.u32(0)? as f32, o.u32(4)? as f32, o.u32(8)? as f32, o.u32(12)? as f32];
        edit(b, B_PARAMS, |p| {
            v.iter().enumerate().for_each(|(i, x)| put_f32(p, P_BORDER + 4 * i, *x));
        })
    });
}

extern "C" fn builder_set_max_anisotropy(b: u64, v: f32) {
    guard("nvnSamplerBuilderSetMaxAnisotropy", || {
        edit(b, B_PARAMS, |p| put_f32(p, P_ANISO, v))
    });
}

/// The snap of the level of detail has no counterpart in a Metal sampler. The call changes nothing.
extern "C" fn builder_set_lod_snap(_b: u64, _v: f32) {}

extern "C" fn builder_set_reduction_filter(b: u64, r: i32) {
    guard("nvnSamplerBuilderSetReductionFilter", || {
        edit(b, B_PARAMS, |p| p[P_REDUCTION] = r as u8)
    });
}

fn out_min_mag(p: &[u8; PARAMS], min: u64, mag: u64) -> AbiResult<()> {
    Obj(min).set_i32(0, i32::from(p[P_MIN]))?;
    Obj(mag).set_i32(0, i32::from(p[P_MAG]))
}

fn out_wrap(p: &[u8; PARAMS], out: [u64; 3]) -> AbiResult<()> {
    for (o, at) in out.iter().zip([P_WRAP_S, P_WRAP_T, P_WRAP_R]) {
        Obj(*o).set_i32(0, i32::from(p[at]))?;
    }
    Ok(())
}

fn out_lod_clamp(p: &[u8; PARAMS], lo: u64, hi: u64) -> AbiResult<()> {
    Obj(lo).set_f32(0, f32_at(p, P_LOD_MIN))?;
    Obj(hi).set_f32(0, f32_at(p, P_LOD_MAX))
}

extern "C" fn builder_get_min_mag_filter(b: u64, min: u64, mag: u64) {
    guard("nvnSamplerBuilderGetMinMagFilter", || {
        out_min_mag(&load(b, B_PARAMS)?, min, mag)
    });
}

extern "C" fn builder_get_wrap_mode(b: u64, s: u64, t: u64, r: u64) {
    guard("nvnSamplerBuilderGetWrapMode", || {
        out_wrap(&load(b, B_PARAMS)?, [s, t, r])
    });
}

extern "C" fn builder_get_lod_clamp(b: u64, lo: u64, hi: u64) {
    guard("nvnSamplerBuilderGetLodClamp", || {
        out_lod_clamp(&load(b, B_PARAMS)?, lo, hi)
    });
}

extern "C" fn sampler_initialize(s: u64, b: u64) -> u32 {
    guard("nvnSamplerInitialize", || {
        let p = load(b, B_PARAMS)?;
        let o = Obj(s);
        o.zero(used::SAMPLER)?;
        o.set_u32(0, SMP)?;
        o.write(S_PARAMS, &p)?;
        Ok(1)
    })
}

extern "C" fn sampler_finalize(s: u64) {
    guard("nvnSamplerFinalize", || Obj(s).set_u32(0, 0));
}

extern "C" fn sampler_set_debug_label(_s: u64, _label: u64) {}

extern "C" fn sampler_compare(a: u64, b: u64) -> u32 {
    guard("nvnSamplerCompare", || {
        Ok(u32::from(load(a, S_PARAMS)? == load(b, S_PARAMS)?))
    })
}

extern "C" fn sampler_get_min_mag_filter(s: u64, min: u64, mag: u64) {
    guard("nvnSamplerGetMinMagFilter", || {
        out_min_mag(&load(s, S_PARAMS)?, min, mag)
    });
}

extern "C" fn sampler_get_wrap_mode(s: u64, a: u64, b: u64, c: u64) {
    guard("nvnSamplerGetWrapMode", || {
        out_wrap(&load(s, S_PARAMS)?, [a, b, c])
    });
}

extern "C" fn sampler_get_lod_clamp(s: u64, lo: u64, hi: u64) {
    guard("nvnSamplerGetLodClamp", || {
        out_lod_clamp(&load(s, S_PARAMS)?, lo, hi)
    });
}

extern "C" fn sampler_get_lod_bias(s: u64) -> f32 {
    guard("nvnSamplerGetLodBias", || {
        Ok(f32_at(&load(s, S_PARAMS)?, P_LOD_BIAS))
    })
}

extern "C" fn sampler_get_compare(s: u64, mode: u64, func: u64) {
    guard("nvnSamplerGetCompare", || {
        let p = load(s, S_PARAMS)?;
        Obj(mode).set_i32(0, i32::from(p[P_CMP_MODE]))?;
        Obj(func).set_i32(0, i32::from(p[P_CMP_FUNC]))
    });
}

extern "C" fn sampler_get_border_color(s: u64, out: u64) {
    guard("nvnSamplerGetBorderColor", || {
        let p = load(s, S_PARAMS)?;
        Obj(out).write(0, &p[P_BORDER..P_BORDER + 16])
    });
}

extern "C" fn sampler_get_max_anisotropy(s: u64) -> f32 {
    guard("nvnSamplerGetMaxAnisotropy", || {
        Ok(f32_at(&load(s, S_PARAMS)?, P_ANISO))
    })
}

extern "C" fn sampler_get_reduction_filter(s: u64) -> i32 {
    guard("nvnSamplerGetReductionFilter", || {
        Ok(i32::from(load(s, S_PARAMS)?[P_REDUCTION]))
    })
}

extern "C" fn pool_initialize(sp: u64, memory_pool: u64, off: i64, n: i32) -> u32 {
    guard("nvnSamplerPoolInitialize", || {
        POOLS.initialize(
            "nvnSamplerPoolInitialize",
            sp,
            (memory_pool, off),
            n,
            |base, count, host| {
                gpu("nvnSamplerPoolInitialize")
                    .with_front(|f| f.add_sampler_pool(base, count, host))
            },
        )
    })
}

extern "C" fn pool_finalize(sp: u64) {
    guard("nvnSamplerPoolFinalize", || {
        if let Some((base, count)) = POOLS.finalize(sp)? {
            let backend = gpu("nvnSamplerPoolFinalize").backend();
            for id in base..base.saturating_add(count) {
                if live_clear(id) {
                    backend.sampler_released(id);
                }
            }
        }
        Ok(())
    });
}

/// Registers `params` in the slot `id` of the pool at `sp`.
fn register(name: &str, sp: u64, id: i32, params: &[u8; PARAMS]) -> AbiResult<()> {
    let slot = POOLS.slot(sp, id)?;
    let tsc = tsc_of(params);
    let header = tsc.to_bytes()?;
    let g = gpu(name);
    g.with_front(|f| f.register_sampler(slot.id, tsc))?;
    guest::write(slot.header, &header)?;
    if live_set(slot.id) {
        g.backend().sampler_released(slot.id);
    }
    g.backend().sampler_registered(slot.id, &tsc);
    Ok(())
}

extern "C" fn pool_register_sampler(sp: u64, id: i32, s: u64) {
    guard("nvnSamplerPoolRegisterSampler", || {
        register("nvnSamplerPoolRegisterSampler", sp, id, &load(s, S_PARAMS)?)
    });
}

extern "C" fn pool_register_sampler_builder(sp: u64, id: i32, b: u64) {
    guard("nvnSamplerPoolRegisterSamplerBuilder", || {
        register(
            "nvnSamplerPoolRegisterSamplerBuilder",
            sp,
            id,
            &load(b, B_PARAMS)?,
        )
    });
}

extern "C" fn pool_get_memory_pool(sp: u64) -> u64 {
    guard("nvnSamplerPoolGetMemoryPool", || pool::memory_pool_of(sp))
}

extern "C" fn pool_get_memory_offset(sp: u64) -> i64 {
    guard("nvnSamplerPoolGetMemoryOffset", || pool::memory_offset_of(sp))
}

extern "C" fn pool_get_size(sp: u64) -> i32 {
    guard("nvnSamplerPoolGetSize", || pool::size_of(sp))
}

extern "C" fn pool_set_debug_label(_sp: u64, _label: u64) {}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnSamplerBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnSamplerBuilderSetDevice" => builder_set_device as extern "C" fn(u64, u64),
        "nvnSamplerBuilderSetMinMagFilter" => builder_set_min_mag_filter as extern "C" fn(u64, i32, i32),
        "nvnSamplerBuilderSetWrapMode" => builder_set_wrap_mode as extern "C" fn(u64, i32, i32, i32),
        "nvnSamplerBuilderSetLodClamp" => builder_set_lod_clamp as extern "C" fn(u64, f32, f32),
        "nvnSamplerBuilderSetLodBias" => builder_set_lod_bias as extern "C" fn(u64, f32),
        "nvnSamplerBuilderSetCompare" => builder_set_compare as extern "C" fn(u64, i32, i32),
        "nvnSamplerBuilderSetBorderColor" => builder_set_border_color as extern "C" fn(u64, u64),
        "nvnSamplerBuilderSetBorderColori" => builder_set_border_colori as extern "C" fn(u64, u64),
        "nvnSamplerBuilderSetBorderColorui" => builder_set_border_colorui as extern "C" fn(u64, u64),
        "nvnSamplerBuilderSetMaxAnisotropy" => builder_set_max_anisotropy as extern "C" fn(u64, f32),
        "nvnSamplerBuilderSetLodSnap" => builder_set_lod_snap as extern "C" fn(u64, f32),
        "nvnSamplerBuilderSetReductionFilter" => builder_set_reduction_filter as extern "C" fn(u64, i32),
        "nvnSamplerBuilderGetMinMagFilter" => builder_get_min_mag_filter as extern "C" fn(u64, u64, u64),
        "nvnSamplerBuilderGetWrapMode" => builder_get_wrap_mode as extern "C" fn(u64, u64, u64, u64),
        "nvnSamplerBuilderGetLodClamp" => builder_get_lod_clamp as extern "C" fn(u64, u64, u64),
        "nvnSamplerInitialize" => sampler_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnSamplerFinalize" => sampler_finalize as extern "C" fn(u64),
        "nvnSamplerSetDebugLabel" => sampler_set_debug_label as extern "C" fn(u64, u64),
        "nvnSamplerCompare" => sampler_compare as extern "C" fn(u64, u64) -> u32,
        "nvnSamplerGetMinMagFilter" => sampler_get_min_mag_filter as extern "C" fn(u64, u64, u64),
        "nvnSamplerGetWrapMode" => sampler_get_wrap_mode as extern "C" fn(u64, u64, u64, u64),
        "nvnSamplerGetLodClamp" => sampler_get_lod_clamp as extern "C" fn(u64, u64, u64),
        "nvnSamplerGetLodBias" => sampler_get_lod_bias as extern "C" fn(u64) -> f32,
        "nvnSamplerGetCompare" => sampler_get_compare as extern "C" fn(u64, u64, u64),
        "nvnSamplerGetBorderColor" => sampler_get_border_color as extern "C" fn(u64, u64),
        "nvnSamplerGetMaxAnisotropy" => sampler_get_max_anisotropy as extern "C" fn(u64) -> f32,
        "nvnSamplerGetReductionFilter" => sampler_get_reduction_filter as extern "C" fn(u64) -> i32,
        "nvnSamplerPoolInitialize" => pool_initialize as extern "C" fn(u64, u64, i64, i32) -> u32,
        "nvnSamplerPoolFinalize" => pool_finalize as extern "C" fn(u64),
        "nvnSamplerPoolRegisterSampler" => pool_register_sampler as extern "C" fn(u64, i32, u64),
        "nvnSamplerPoolRegisterSamplerBuilder" => pool_register_sampler_builder as extern "C" fn(u64, i32, u64),
        "nvnSamplerPoolGetMemoryPool" => pool_get_memory_pool as extern "C" fn(u64) -> u64,
        "nvnSamplerPoolGetMemoryOffset" => pool_get_memory_offset as extern "C" fn(u64) -> i64,
        "nvnSamplerPoolGetSize" => pool_get_size as extern "C" fn(u64) -> i32,
        "nvnSamplerPoolSetDebugLabel" => pool_set_debug_label as extern "C" fn(u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn the_defaults_of_a_builder_are_the_nvn_defaults() {
        let b = testing::alloc(0x60);
        Obj(b).set_u64(B_DEV, 0xD0).expect("device");
        builder_set_defaults(b);
        assert_eq!(Obj(b).u64(B_DEV).expect("device"), 0xD0);
        let t = tsc_of(&load(b, B_PARAMS).expect("params"));
        assert_eq!(
            (t.min_filter, t.mag_filter, t.wrap_s, t.wrap_t, t.wrap_r),
            (4, 1, 1, 1, 1)
        );
        assert_eq!((t.compare_mode, t.compare_func, t.reduction), (0, 2, 0));
        assert_eq!((t.lod_min, t.lod_max, t.lod_bias, t.aniso), (-1000.0, 1000.0, 0.0, 1.0));
        assert_eq!(t.border, [0.0; 4]);
    }

    #[test]
    fn a_sampler_keeps_what_the_builder_set_and_the_getters_read_it_back() {
        let (b, s) = (testing::alloc(0x60), testing::alloc(0x30));
        builder_set_defaults(b);
        builder_set_min_mag_filter(b, 5, 1);
        builder_set_wrap_mode(b, 2, 3, 4);
        builder_set_lod_clamp(b, 0.5, 7.0);
        builder_set_lod_bias(b, -0.25);
        builder_set_compare(b, 1, 3);
        builder_set_max_anisotropy(b, 8.0);
        builder_set_reduction_filter(b, 2);
        let ints = testing::alloc(0x10);
        for (i, v) in [-1i32, 2, 3, 4].into_iter().enumerate() {
            Obj(ints).set_i32(4 * i as u64, v).expect("int");
        }
        builder_set_border_colori(b, ints);
        assert_eq!(sampler_initialize(s, b), 1);
        assert_eq!(Obj(s).u32(0).expect("magic"), SMP);
        let t = tsc_of(&load(s, S_PARAMS).expect("params"));
        assert_eq!((t.min_filter, t.mag_filter), (5, 1));
        assert_eq!((t.wrap_s, t.wrap_t, t.wrap_r), (2, 3, 4));
        assert_eq!((t.compare_mode, t.compare_func, t.reduction), (1, 3, 2));
        assert_eq!((t.lod_min, t.lod_max, t.lod_bias, t.aniso), (0.5, 7.0, -0.25, 8.0));
        assert_eq!(t.border, [-1.0, 2.0, 3.0, 4.0]);
        assert_eq!(sampler_get_lod_bias(s), -0.25);
        assert_eq!(sampler_get_reduction_filter(s), 2);
        let out = testing::alloc(0x10);
        sampler_get_border_color(s, out);
        assert_eq!(Obj(out).f32(12).expect("alpha"), 4.0);
        assert_eq!(sampler_compare(s, s), 1);
        sampler_finalize(s);
        assert_eq!(Obj(s).u32(0).expect("magic"), 0);
    }

    #[test]
    fn the_live_bits_report_a_slot_that_is_taken_again() {
        assert!(!live_set(4000));
        assert!(live_set(4000));
        assert!(live_clear(4000));
        assert!(!live_clear(4000));
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "sampler")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
