//! The window group: `nvnWindow*` and the window builder.
//!
//! A window is a swap chain of up to four textures. The `NVNwindow` is 0x180 bytes in guest memory. The host uses the
//! first 0x50 bytes as plain data and writes no more:
//!
//! | offset | field |
//! | --- | --- |
//! | 0 | magic `WIND` while initialised |
//! | 8 | the native window handle |
//! | 16 | the number of textures (i32) |
//! | 20 | the present interval (i32) |
//! | 24 | the index of the next texture to acquire (u32) |
//! | 28 | the crop, four i32: x, y, width, height. Zero width and height mean no crop |
//! | 48 | the guest addresses of the textures, four u64 |
//!
//! The window builder is 0x40 bytes in the headers. The host writes the first 0x38 bytes: device at 0, native window at 8, number of textures at 16, present interval at 20
//! (default 1), the texture addresses at 24.
//!
//! `nvnWindowAcquireTexture` hands out the textures in a ring. The backend paces the game there
//! ([`Backend::acquire`](nxkit_nvn::backend::Backend::acquire) may block until the display can take a frame).
//! The window does not create or hold the textures. Their lifetime is the game's.
//!
//! A window without a crop presents the crop of the layer that the game set with `nn::vi::SetLayerCrop`, or the whole
//! texture when there is none ([`crate::gfx::vi::present_crop`]).

use nxkit_nvn::backend::Rect;

use super::abi::{AbiResult, Obj, guard, log_once, magic, table, used};
use super::ctx::gpu;
use super::sync;

/// The magic of an initialised window.
const WIND: u32 = magic(b'W', b'I', b'N', b'D');
/// Most textures of a swap chain.
const MAX_TEXTURES: i32 = 4;

const OFF_NATIVE: u64 = 8;
const OFF_NTEX: u64 = 16;
const OFF_INTERVAL: u64 = 20;
const OFF_NEXT: u64 = 24;
const OFF_CROP: u64 = 28;
const OFF_TEX: u64 = 48;
const B_DEV: u64 = 0;
const B_NATIVE: u64 = 8;
const B_NTEX: u64 = 16;
const B_INTERVAL: u64 = 20;
const B_TEX: u64 = 24;

/// What the queue needs of a window to present one of its textures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PresentView {
    /// The guest address of the texture at the index.
    pub(super) texture: u64,
    /// The present interval.
    pub(super) interval: i32,
    /// The crop of the window. Zero width and height mean the whole texture.
    pub(super) crop: Rect,
}

/// The window at `w` as a present of its texture `index`. None when `index` is not a texture of the window.
pub(super) fn present_view(w: u64, index: i32) -> AbiResult<Option<PresentView>> {
    let o = Obj(w);
    let n = o.i32(OFF_NTEX)?;
    if index < 0 || index >= n || index >= MAX_TEXTURES {
        return Ok(None);
    }
    Ok(Some(PresentView {
        texture: o.u64(OFF_TEX + 8 * index as u64)?,
        interval: o.i32(OFF_INTERVAL)?,
        crop: crop(o)?,
    }))
}

fn crop(o: Obj) -> AbiResult<Rect> {
    Ok(Rect {
        x: o.i32(OFF_CROP)?,
        y: o.i32(OFF_CROP + 4)?,
        width: o.i32(OFF_CROP + 8)?,
        height: o.i32(OFF_CROP + 12)?,
    })
}

/// Takes the next texture of the window at `w`. Writes the index to `idx` and marks `sync_obj` (when not null) as signalled.
/// The result is the index.
pub(super) fn acquire(name: &'static str, w: u64, sync_obj: u64, idx: u64) -> AbiResult<u32> {
    let o = Obj(w);
    let n = o.i32(OFF_NTEX)?;
    let index = if n > 0 {
        let next = o.u32(OFF_NEXT)?;
        o.set_u32(OFF_NEXT, next.wrapping_add(1))?;
        next % n as u32
    } else {
        0
    };
    // The backend may block here until the display can take another frame. The lock is not held.
    let g = gpu(name);
    let start = (g.settings().clock_ns)();
    g.backend().acquire(w, index);
    let waited = (g.settings().clock_ns)().saturating_sub(start);
    // The frame step takes the first acquire after a present as the start of the frame, and the wait off its cost.
    // Later acquires of the same frame are ignored there. `Err` means the pacing service is not running.
    let _ = nxkit_svc_pacing::frame_started_waited(start, waited);
    if sync_obj != 0 {
        sync::set_acquire_fence(Obj(sync_obj))?;
    }
    Obj(idx).set_i32(0, index as i32)?;
    Ok(index)
}

extern "C" fn builder_set_defaults(b: u64) {
    guard("nvnWindowBuilderSetDefaults", || {
        let o = Obj(b);
        let dev = o.u64(B_DEV)?;
        o.zero(used::WINDOW_BUILDER)?;
        o.set_u64(B_DEV, dev)?;
        o.set_i32(B_INTERVAL, 1)
    });
}

extern "C" fn builder_set_device(b: u64, dev: u64) {
    guard("nvnWindowBuilderSetDevice", || Obj(b).set_u64(B_DEV, dev));
}

extern "C" fn builder_set_native_window(b: u64, native: u64) {
    guard("nvnWindowBuilderSetNativeWindow", || {
        Obj(b).set_u64(B_NATIVE, native)
    });
}

extern "C" fn builder_set_textures(b: u64, n: i32, textures: u64) {
    guard("nvnWindowBuilderSetTextures", || {
        let n = if (0..=MAX_TEXTURES).contains(&n) {
            n
        } else {
            eprintln!(
                "nxkit-rt: warning: NVN nvnWindowBuilderSetTextures: the count {n} is not in 0..=4"
            );
            n.clamp(0, MAX_TEXTURES)
        };
        let o = Obj(b);
        o.set_i32(B_NTEX, n)?;
        for i in 0..n as u64 {
            o.set_u64(B_TEX + 8 * i, Obj(textures).u64(8 * i)?)?;
        }
        Ok(())
    });
}

extern "C" fn builder_set_num_active_textures(_b: u64, n: i32) {
    log_once(
        "nvnWindowBuilderSetNumActiveTextures",
        format_args!(
            "WindowBuilderSetNumActiveTextures {n} (the host rotates through the SetTextures count)"
        ),
    );
}

extern "C" fn builder_set_present_interval(b: u64, v: i32) {
    guard("nvnWindowBuilderSetPresentInterval", || {
        Obj(b).set_i32(B_INTERVAL, v)
    });
}

extern "C" fn builder_get_present_interval(b: u64) -> i32 {
    guard("nvnWindowBuilderGetPresentInterval", || {
        Obj(b).i32(B_INTERVAL)
    })
}

extern "C" fn window_initialize(w: u64, b: u64) -> u32 {
    guard("nvnWindowInitialize", || {
        let (src, o) = (Obj(b), Obj(w));
        let ntex = src.i32(B_NTEX)?;
        o.zero(used::WINDOW)?;
        o.set_u32(0, WIND)?;
        o.set_u64(OFF_NATIVE, src.u64(B_NATIVE)?)?;
        o.set_i32(OFF_NTEX, ntex)?;
        o.set_i32(OFF_INTERVAL, src.i32(B_INTERVAL)?)?;
        for i in 0..MAX_TEXTURES as u64 {
            o.set_u64(OFF_TEX + 8 * i, src.u64(B_TEX + 8 * i)?)?;
        }
        let first = o.u64(OFF_TEX)?;
        let (width, height) = gpu("nvnWindowInitialize")
            .texture_extent(first)
            .unwrap_or((0, 0));
        log_once(
            "nvnWindowInitialize",
            format_args!(
                "WindowInitialize swap chain of {ntex} textures, interval {}, first {width}x{height}",
                o.i32(OFF_INTERVAL)?
            ),
        );
        Ok(1)
    })
}

extern "C" fn window_set_debug_label(_w: u64, _label: u64) {}

extern "C" fn window_acquire_texture(w: u64, sync: u64, idx: u64) -> u32 {
    guard("nvnWindowAcquireTexture", || {
        super::frametime::sub(4, || acquire("nvnWindowAcquireTexture", w, sync, idx))?;
        Ok(0)
    })
}

extern "C" fn window_set_num_active_textures(_w: u64, n: i32) {
    log_once(
        "nvnWindowSetNumActiveTextures",
        format_args!(
            "WindowSetNumActiveTextures {n} (the host rotates through the SetTextures count)"
        ),
    );
}

extern "C" fn window_set_present_interval(w: u64, v: i32) {
    guard("nvnWindowSetPresentInterval", || {
        Obj(w).set_i32(OFF_INTERVAL, v)
    });
}

extern "C" fn window_get_present_interval(w: u64) -> i32 {
    guard("nvnWindowGetPresentInterval", || Obj(w).i32(OFF_INTERVAL))
}

extern "C" fn window_set_crop(w: u64, x: i32, y: i32, width: i32, height: i32) {
    guard("nvnWindowSetCrop", || {
        let o = Obj(w);
        for (i, v) in [x, y, width, height].into_iter().enumerate() {
            o.set_i32(OFF_CROP + 4 * i as u64, v)?;
        }
        Ok(())
    });
}

/// The crop that `nvnWindowGetCrop` reports: the window's crop, or the whole first texture while no crop is set.
fn reported_crop(c: Rect, first_texture: Option<(u32, u32)>) -> Rect {
    if c.width > 0 && c.height > 0 {
        return c;
    }
    let (width, height) = first_texture.unwrap_or((0, 0));
    Rect {
        x: 0,
        y: 0,
        width: i32::try_from(width).unwrap_or(i32::MAX),
        height: i32::try_from(height).unwrap_or(i32::MAX),
    }
}

extern "C" fn window_get_crop(w: u64, rect: u64) {
    guard("nvnWindowGetCrop", || {
        let o = Obj(w);
        let first = if o.i32(OFF_NTEX)? > 0 {
            gpu("nvnWindowGetCrop").texture_extent(o.u64(OFF_TEX)?)
        } else {
            None
        };
        let r = reported_crop(crop(o)?, first);
        let out = Obj(rect);
        out.set_i32(0, r.x)?;
        out.set_i32(4, r.y)?;
        out.set_i32(8, r.width)?;
        out.set_i32(12, r.height)
    });
}

/// The functions of the group.
pub(super) fn procs() -> Vec<(&'static str, usize)> {
    table![
        "nvnWindowBuilderSetDevice" => builder_set_device as extern "C" fn(u64, u64),
        "nvnWindowBuilderSetDefaults" => builder_set_defaults as extern "C" fn(u64),
        "nvnWindowBuilderSetNativeWindow" => builder_set_native_window as extern "C" fn(u64, u64),
        "nvnWindowBuilderSetTextures" => builder_set_textures as extern "C" fn(u64, i32, u64),
        "nvnWindowBuilderSetNumActiveTextures" => builder_set_num_active_textures as extern "C" fn(u64, i32),
        "nvnWindowBuilderSetPresentInterval" => builder_set_present_interval as extern "C" fn(u64, i32),
        "nvnWindowBuilderGetPresentInterval" => builder_get_present_interval as extern "C" fn(u64) -> i32,
        "nvnWindowInitialize" => window_initialize as extern "C" fn(u64, u64) -> u32,
        "nvnWindowSetDebugLabel" => window_set_debug_label as extern "C" fn(u64, u64),
        "nvnWindowAcquireTexture" => window_acquire_texture as extern "C" fn(u64, u64, u64) -> u32,
        "nvnWindowSetNumActiveTextures" => window_set_num_active_textures as extern "C" fn(u64, i32),
        "nvnWindowSetPresentInterval" => window_set_present_interval as extern "C" fn(u64, i32),
        "nvnWindowGetPresentInterval" => window_get_present_interval as extern "C" fn(u64) -> i32,
        "nvnWindowSetCrop" => window_set_crop as extern "C" fn(u64, i32, i32, i32, i32),
        "nvnWindowGetCrop" => window_get_crop as extern "C" fn(u64, u64),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvn::abi::testing;

    #[test]
    fn a_crop_that_is_not_set_reports_the_first_texture() {
        let none = Rect::default();
        assert_eq!(
            reported_crop(none, Some((1280, 720))),
            Rect {
                x: 0,
                y: 0,
                width: 1280,
                height: 720
            }
        );
        assert_eq!(reported_crop(none, None), none);
        let set = Rect {
            x: 4,
            y: 8,
            width: 100,
            height: 50,
        };
        assert_eq!(reported_crop(set, Some((1280, 720))), set);
    }

    #[test]
    fn the_builder_copies_up_to_four_textures_and_defaults_to_interval_one() {
        let (b, list) = (
            testing::alloc(crate::nvn::abi::size::WINDOW_BUILDER),
            testing::alloc(32),
        );
        builder_set_defaults(b);
        assert_eq!(builder_get_present_interval(b), 1);
        for i in 0..3u64 {
            Obj(list).set_u64(8 * i, 0x1000 * (i + 1)).expect("texture");
        }
        builder_set_textures(b, 3, list);
        assert_eq!(Obj(b).i32(B_NTEX).expect("n"), 3);
        assert_eq!(Obj(b).u64(B_TEX + 16).expect("t2"), 0x3000);
    }

    #[test]
    fn a_present_view_exists_only_for_an_index_inside_the_swap_chain() {
        let w = testing::alloc(crate::nvn::abi::size::WINDOW);
        Obj(w).set_i32(OFF_NTEX, 2).expect("n");
        Obj(w).set_i32(OFF_INTERVAL, 2).expect("interval");
        Obj(w).set_u64(OFF_TEX + 8, 0xABC0).expect("tex");
        let v = present_view(w, 1).expect("view").expect("some");
        assert_eq!((v.texture, v.interval), (0xABC0, 2));
        assert_eq!(present_view(w, 2).expect("view"), None);
        assert_eq!(present_view(w, -1).expect("view"), None);
    }

    #[test]
    fn the_group_exports_every_name_of_its_row() {
        let got: std::collections::HashSet<&str> = procs().iter().map(|(n, _)| *n).collect();
        let want: std::collections::HashSet<&str> = nxkit_nvn::entries::FFI_GROUPS
            .iter()
            .filter(|(g, _)| *g == "window")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();
        assert_eq!(got, want);
    }
}
