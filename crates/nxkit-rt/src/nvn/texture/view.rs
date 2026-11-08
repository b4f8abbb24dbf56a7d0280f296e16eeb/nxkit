//! The texture view: `nvnTextureView*`.
//!
//! An `NVNtextureView` is 16 bytes in newer SDKs and a game keeps it in a stack slot of that size. The host uses the
//! first 14 bytes of it as plain data and writes no byte past them. The layout is private to nxkit:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | bits: levels set (1), layers set (2), format set (4), target set (8), swizzle set (0x10), mode set (0x20), the depth stencil mode (bits 6 and 7) |
//! | 1 | target (u8) |
//! | 2 | base level (u8), number of levels (u8) |
//! | 4 | format (u16) |
//! | 6 | first layer (u16), number of layers (u16) |
//! | 10 | swizzle (4 bytes) |
//!
//! [`View::load`] reads the 14 bytes once. [`texture_view`] turns them into the [`TextureView`] that the front end
//! registers. A view that the game passes as null shows the whole texture.

use nxkit_nvn::layout::{Target, TextureDesc};
use nxkit_nvn::tic::TextureView;

use crate::nvn::abi::{AbiResult, Obj, guard, ret_bool, table, used};

use super::builder::{IDENTITY_SWIZZLE, out_swizzle};

const SET_LEVELS: u8 = 1;
const SET_LAYERS: u8 = 2;
const SET_FORMAT: u8 = 4;
const SET_TARGET: u8 = 8;
const SET_SWIZZLE: u8 = 0x10;
const SET_MODE: u8 = 0x20;
const MODE_SHIFT: u32 = 6;
const MODE_STENCIL: u8 = 1;
/// The bytes of an `NVNtextureView` that the host uses.
const BYTES: u64 = used::TEXTURE_VIEW;
const TARGET: u64 = 1;
const LEVELS: u64 = 2;
const FORMAT: u64 = 4;
const LAYERS: u64 = 6;
const SWIZZLE: u64 = 10;

/// What a view holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct View {
    bits: u8,
    target: u8,
    base_level: u8,
    levels: u8,
    format: u16,
    first_layer: u16,
    layers: u16,
    swizzle: [u8; 4],
}

impl View {
    /// Reads the view at `addr`. A null address gives a view that sets nothing.
    pub(super) fn load(addr: u64) -> AbiResult<Self> {
        if addr == 0 {
            return Ok(Self::default());
        }
        let mut r = [0u8; BYTES as usize];
        Obj(addr).read(0, &mut r)?;
        Ok(Self {
            bits: r[0],
            target: r[TARGET as usize],
            base_level: r[LEVELS as usize],
            levels: r[LEVELS as usize + 1],
            format: u16::from_le_bytes([r[FORMAT as usize], r[FORMAT as usize + 1]]),
            first_layer: u16::from_le_bytes([r[LAYERS as usize], r[LAYERS as usize + 1]]),
            layers: u16::from_le_bytes([r[LAYERS as usize + 2], r[LAYERS as usize + 3]]),
            swizzle: [
                r[SWIZZLE as usize],
                r[SWIZZLE as usize + 1],
                r[SWIZZLE as usize + 2],
                r[SWIZZLE as usize + 3],
            ],
        })
    }

    /// The level that a read or a write of texels starts at.
    pub(super) fn level(&self) -> u32 {
        if self.has(SET_LEVELS) {
            u32::from(self.base_level)
        } else {
            0
        }
    }

    /// The layer that a read or a write of texels starts at.
    pub(super) fn first_layer(&self) -> u32 {
        if self.has(SET_LAYERS) {
            u32::from(self.first_layer)
        } else {
            0
        }
    }

    /// True when the view reads the stencil of a depth stencil texture.
    pub(super) fn stencil(&self) -> bool {
        self.has(SET_MODE) && self.bits >> MODE_SHIFT == MODE_STENCIL
    }

    fn has(&self, bit: u8) -> bool {
        self.bits & bit != 0
    }

    fn store(&self, addr: u64) -> AbiResult<()> {
        let mut r = [0u8; BYTES as usize];
        r[0] = self.bits;
        r[TARGET as usize] = self.target;
        r[LEVELS as usize] = self.base_level;
        r[LEVELS as usize + 1] = self.levels;
        r[FORMAT as usize..FORMAT as usize + 2].copy_from_slice(&self.format.to_le_bytes());
        r[LAYERS as usize..LAYERS as usize + 2].copy_from_slice(&self.first_layer.to_le_bytes());
        r[LAYERS as usize + 2..LAYERS as usize + 4].copy_from_slice(&self.layers.to_le_bytes());
        r[SWIZZLE as usize..SWIZZLE as usize + 4].copy_from_slice(&self.swizzle);
        Obj(addr).write(0, &r)
    }

    /// True when both views select the same thing: the same fields are set and every field that is set has the same
    /// value.
    fn same(&self, o: &Self) -> bool {
        let both = |bit: u8, eq: bool| self.has(bit) == o.has(bit) && (!self.has(bit) || eq);
        both(SET_LEVELS, (self.base_level, self.levels) == (o.base_level, o.levels))
            && both(SET_LAYERS, (self.first_layer, self.layers) == (o.first_layer, o.layers))
            && both(SET_FORMAT, self.format == o.format)
            && both(SET_TARGET, self.target == o.target)
            && both(SET_SWIZZLE, self.swizzle == o.swizzle)
            && both(SET_MODE, self.bits >> MODE_SHIFT == o.bits >> MODE_SHIFT)
    }
}

/// The view that the front end registers for the view at `addr`. A field that the view does not set comes from the
/// texture: `swizzle` is the swizzle that the texture was built with.
pub(super) fn texture_view(addr: u64, swizzle: [u8; 4]) -> AbiResult<TextureView> {
    let v = View::load(addr)?;
    let mut t = TextureView::identity();
    t.swizzle = swizzle;
    if v.has(SET_LEVELS) {
        t.base_level = v.base_level;
        t.levels = v.levels;
    }
    if v.has(SET_LAYERS) {
        t.base_layer = v.first_layer;
        t.layers = v.layers;
    }
    if v.has(SET_FORMAT) {
        t.format = Some(i32::from(v.format));
    }
    if v.has(SET_TARGET) {
        t.target = Some(Target::from_raw(v.target)?);
    }
    if v.has(SET_SWIZZLE) {
        t.swizzle = v.swizzle;
    }
    Ok(t)
}

/// The byte offset of the first level and layer that the view at `addr` shows, from the start of the storage of the
/// texture. A null view gives 0.
pub(super) fn offset(desc: &TextureDesc, addr: u64) -> AbiResult<i64> {
    let v = View::load(addr)?;
    let mut off = 0u64;
    if v.has(SET_LEVELS) {
        off += desc.level_offset(u32::from(v.base_level))?;
    }
    if v.has(SET_LAYERS) {
        off += u64::from(v.first_layer) * desc.layout()?.layer_size;
    }
    Ok(off as i64)
}

fn edit(addr: u64, f: impl FnOnce(&mut View)) -> AbiResult<()> {
    let mut v = View::load(addr)?;
    f(&mut v);
    v.store(addr)
}

extern "C" fn set_defaults(v: u64) {
    guard("nvnTextureViewSetDefaults", || Obj(v).zero(BYTES));
}

extern "C" fn set_levels(v: u64, base: i32, n: i32) {
    guard("nvnTextureViewSetLevels", || {
        edit(v, |w| {
            w.bits |= SET_LEVELS;
            w.base_level = base as u8;
            w.levels = n as u8;
        })
    });
}

extern "C" fn set_layers(v: u64, first: i32, n: i32) {
    guard("nvnTextureViewSetLayers", || {
        edit(v, |w| {
            w.bits |= SET_LAYERS;
            w.first_layer = first as u16;
            w.layers = n as u16;
        })
    });
}

extern "C" fn set_format(v: u64, f: i32) {
    guard("nvnTextureViewSetFormat", || {
        edit(v, |w| {
            w.bits |= SET_FORMAT;
            w.format = f as u16;
        })
    });
}

extern "C" fn set_target(v: u64, t: i32) {
    guard("nvnTextureViewSetTarget", || {
        edit(v, |w| {
            w.bits |= SET_TARGET;
            w.target = t as u8;
        })
    });
}

extern "C" fn set_depth_stencil_mode(v: u64, m: i32) {
    guard("nvnTextureViewSetDepthStencilMode", || {
        edit(v, |w| {
            w.bits = (w.bits & 0x1f) | SET_MODE | ((m as u8 & 3) << MODE_SHIFT);
        })
    });
}

extern "C" fn set_swizzle(v: u64, r: i32, g: i32, b: i32, a: i32) {
    guard("nvnTextureViewSetSwizzle", || {
        edit(v, |w| {
            w.bits |= SET_SWIZZLE;
            w.swizzle = [r as u8 & 7, g as u8 & 7, b as u8 & 7, a as u8 & 7];
        })
    });
}

extern "C" fn compare(a: u64, b: u64) -> u32 {
    guard("nvnTextureViewCompare", || {
        Ok(ret_bool(View::load(a)?.same(&View::load(b)?)))
    })
}

fn write_pair(first: u64, second: u64, a: i32, b: i32) -> AbiResult<()> {
    Obj(first).set_i32(0, a)?;
    Obj(second).set_i32(0, b)
}

extern "C" fn get_levels(v: u64, base: u64, n: u64) -> u32 {
    guard("nvnTextureViewGetLevels", || {
        let w = View::load(v)?;
        write_pair(base, n, i32::from(w.base_level), i32::from(w.levels))?;
        Ok(ret_bool(w.has(SET_LEVELS)))
    })
}

extern "C" fn get_layers(v: u64, first: u64, n: u64) -> u32 {
    guard("nvnTextureViewGetLayers", || {
        let w = View::load(v)?;
        write_pair(first, n, i32::from(w.first_layer), i32::from(w.layers))?;
        Ok(ret_bool(w.has(SET_LAYERS)))
    })
}

extern "C" fn get_target(v: u64, t: u64) -> u32 {
    guard("nvnTextureViewGetTarget", || {
        let w = View::load(v)?;
        Obj(t).set_i32(0, i32::from(w.target))?;
        Ok(ret_bool(w.has(SET_TARGET)))
    })
}

extern "C" fn get_format(v: u64, f: u64) -> u32 {
    guard("nvnTextureViewGetFormat", || {
        let w = View::load(v)?;
        if f != 0 {
            Obj(f).set_i32(0, i32::from(w.format))?;
        }
        Ok(ret_bool(w.has(SET_FORMAT)))
    })
}

extern "C" fn get_depth_stencil_mode(v: u64, m: u64) -> u32 {
    guard("nvnTextureViewGetDepthStencilMode", || {
        let w = View::load(v)?;
        if m != 0 {
            Obj(m).set_i32(0, i32::from(w.bits >> MODE_SHIFT))?;
        }
        Ok(ret_bool(w.has(SET_MODE)))
    })
}

extern "C" fn get_swizzle(v: u64, r: u64, g: u64, b: u64, a: u64) -> u32 {
    guard("nvnTextureViewGetSwizzle", || {
        let w = View::load(v)?;
        let sw = if w.has(SET_SWIZZLE) {
            w.swizzle
        } else {
            IDENTITY_SWIZZLE
        };
        out_swizzle(sw, [r, g, b, a])?;
        Ok(ret_bool(w.has(SET_SWIZZLE)))
    })
}

/// The functions of the view.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnTextureViewSetDefaults" => set_defaults as extern "C" fn(u64),
        "nvnTextureViewSetLevels" => set_levels as extern "C" fn(u64, i32, i32),
        "nvnTextureViewSetLayers" => set_layers as extern "C" fn(u64, i32, i32),
        "nvnTextureViewSetFormat" => set_format as extern "C" fn(u64, i32),
        "nvnTextureViewSetTarget" => set_target as extern "C" fn(u64, i32),
        "nvnTextureViewSetDepthStencilMode" => set_depth_stencil_mode as extern "C" fn(u64, i32),
        "nvnTextureViewSetSwizzle" => set_swizzle as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnTextureViewCompare" => compare as extern "C" fn(u64, u64) -> u32,
        "nvnTextureViewGetLevels" => get_levels as extern "C" fn(u64, u64, u64) -> u32,
        "nvnTextureViewGetLayers" => get_layers as extern "C" fn(u64, u64, u64) -> u32,
        "nvnTextureViewGetTarget" => get_target as extern "C" fn(u64, u64) -> u32,
        "nvnTextureViewGetFormat" => get_format as extern "C" fn(u64, u64) -> u32,
        "nvnTextureViewGetDepthStencilMode" => get_depth_stencil_mode as extern "C" fn(u64, u64) -> u32,
        "nvnTextureViewGetSwizzle" => get_swizzle as extern "C" fn(u64, u64, u64, u64, u64) -> u32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::{arg_bool, testing};

    fn set(v: u32) -> bool {
        arg_bool(v)
    }

    #[test]
    fn a_view_remembers_what_was_set_and_reports_what_was_not() {
        let v = testing::alloc(16);
        set_defaults(v);
        let (a, b) = (testing::alloc(8), testing::alloc(8));
        assert!(!set(get_levels(v, a, b)));
        set_levels(v, 2, 3);
        set_layers(v, 4, 5);
        set_format(v, 0x25);
        set_target(v, 4);
        assert!(set(get_levels(v, a, b)));
        assert_eq!((Obj(a).i32(0).expect("a"), Obj(b).i32(0).expect("b")), (2, 3));
        assert!(set(get_layers(v, a, b)));
        assert_eq!((Obj(a).i32(0).expect("a"), Obj(b).i32(0).expect("b")), (4, 5));
        assert!(set(get_format(v, a)));
        assert_eq!(Obj(a).i32(0).expect("format"), 0x25);
        assert!(set(get_target(v, a)));
        assert_eq!(Obj(a).i32(0).expect("target"), 4);
        assert!(!set(get_depth_stencil_mode(v, a)));
    }

    #[test]
    fn the_swizzle_of_a_view_without_one_reads_as_the_identity() {
        let v = testing::alloc(16);
        set_defaults(v);
        let out = testing::alloc(0x10);
        assert!(!set(get_swizzle(v, out, out + 4, out + 8, out + 12)));
        assert_eq!(Obj(out).i32(12).expect("a"), 5);
        set_swizzle(v, 5, 4, 3, 2);
        assert!(set(get_swizzle(v, out, 0, 0, out + 12)));
        assert_eq!((Obj(out).i32(0).expect("r"), Obj(out).i32(12).expect("a")), (5, 2));
    }

    #[test]
    fn the_depth_stencil_mode_does_not_touch_the_other_bits() {
        let v = testing::alloc(16);
        set_defaults(v);
        set_levels(v, 1, 1);
        set_depth_stencil_mode(v, 1);
        let w = View::load(v).expect("load");
        assert!(w.stencil());
        assert_eq!(w.level(), 1);
        let m = testing::alloc(4);
        assert!(set(get_depth_stencil_mode(v, m)));
        assert_eq!(Obj(m).i32(0).expect("mode"), 1);
    }

    #[test]
    fn two_views_are_the_same_when_what_they_set_is_the_same() {
        let (a, b) = (testing::alloc(16), testing::alloc(16));
        set_defaults(a);
        set_defaults(b);
        assert!(set(compare(a, b)));
        set_levels(a, 1, 2);
        assert!(!set(compare(a, b)));
        set_levels(b, 1, 2);
        assert!(set(compare(a, b)));
        set_layers(b, 0, 1);
        assert!(!set(compare(a, b)));
    }

    #[test]
    fn a_view_turns_into_the_view_of_the_front_end() {
        let v = testing::alloc(16);
        set_defaults(v);
        set_levels(v, 1, 2);
        set_layers(v, 3, 4);
        set_format(v, 0x25);
        set_target(v, 1);
        let t = texture_view(v, [5, 4, 3, 2]).expect("view");
        assert_eq!((t.base_level, t.levels, t.base_layer, t.layers), (1, 2, 3, 4));
        assert_eq!(t.format, Some(0x25));
        assert_eq!(t.target, Some(Target::D2));
        assert_eq!(t.swizzle, [5, 4, 3, 2]);
        set_swizzle(v, 0, 1, 2, 3);
        assert_eq!(texture_view(v, [5, 4, 3, 2]).expect("view").swizzle, [0, 1, 2, 3]);
        assert_eq!(texture_view(0, [2, 3, 4, 5]).expect("view"), TextureView::identity());
    }
}
