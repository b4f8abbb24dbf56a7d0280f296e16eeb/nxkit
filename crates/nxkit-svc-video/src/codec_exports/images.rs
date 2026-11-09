//! The image preferences, the pools and the reads of decoded pictures.

use super::*;

// ---------- image preferences and pools ----------

pub(super) extern "C" fn pref(w: u64, h: u64, fmt: u32, mode: u32) -> Pref {
    crate::codec::trace(format_args!("pref {w}x{h} format {fmt} mode {mode}"));
    Pref::new(u64::from(w as u16), u64::from(h as u16), fmt, mode)
}

pub(super) extern "C" fn pool_size(size: u64, align: u64, pref_at: u64, n: u32) -> u32 {
    let Some(p) = Pref::read(pref_at) else {
        return BAD;
    };
    bad_if(
        guest::write_u64(size, pool_bytes(&p, n)).is_ok()
            && guest::write_u64(align, POOL_ALIGN).is_ok(),
    )
}

pub(super) extern "C" fn pool_with_mode(
    t: u64,
    mem: u64,
    size: u64,
    pref_at: u64,
    n: u32,
    _mode: u32,
) -> u32 {
    crate::codec::trace(format_args!(
        "pool {t:#x} mem {mem:#x} size {size:#x} n {n} mode {_mode}"
    ));
    pool_no_mode(t, mem, size, pref_at, n)
}

pub(super) extern "C" fn pool_no_mode(t: u64, mem: u64, size: u64, pref_at: u64, n: u32) -> u32 {
    crate::codec::trace(format_args!(
        "pool {t:#x} mem {mem:#x} size {size:#x} n {n}"
    ));
    match Pref::read(pref_at) {
        Some(p) => pool_init(t, mem, size, &p, n),
        None => BAD,
    }
}

pub(super) extern "C" fn pool_finalize(t: u64) {
    pool_fin(t);
}

pub(super) extern "C" fn native_image_handle(t: u64, i: u32) -> u32 {
    crate::codec::trace(format_args!("native image handle pool {t:#x} picture {i}"));
    pool_of(t).map_or(0, |p| native_handle(&p, i))
}

pub(super) extern "C" fn read_rgba_hdr(
    _t: u64,
    _i: u32,
    _x: u64,
    _y: u64,
    _w: u64,
    _h: u64,
    dst: u64,
    _stride: u64,
    size: u64,
) -> u32 {
    static ONCE: AtomicBool = AtomicBool::new(false);
    red_once(
        &ONCE,
        "nn::codec::ReadDecoderOutputImageRgbaHdr: HDR pictures are not served, black",
    );
    let _ = guest::fill(dst, size, 0);
    0
}

pub(super) extern "C" fn read_rgba(
    t: u64,
    i: u32,
    x: u64,
    y: u64,
    w: u64,
    h: u64,
    dst: u64,
    stride: u64,
    size: u64,
) -> u32 {
    crate::codec::trace(format_args!(
        "read rgba picture {i} {w}x{h} at {x},{y} stride {stride} size {size}"
    ));
    static ONCE: AtomicBool = AtomicBool::new(false);
    let (x, y, w, h) = (x as u16, y as u16, u64::from(w as u16), u64::from(h as u16));
    let Some(frame) = pool_frame(t, i) else {
        let _ = guest::fill(dst, size, 0);
        return 0;
    };
    if x != 0 || y != 0 || stride != w * 4 || size < w * h * 4 {
        red_once(
            &ONCE,
            &format!(
                "nn::codec::ReadDecoderOutputImageRgba: region {x},{y} stride {stride}: only whole tight pictures are served"
            ),
        );
        return 0;
    }
    match frame.to_rgba(w as u32, h as u32) {
        Ok(rgba) => {
            let _ = guest::write(dst, &rgba);
            crate::guest_wrote(dst, rgba.len() as u64);
        }
        Err(e) => red(&format!("nn::codec::ReadDecoderOutputImageRgba: {e}")),
    }
    0
}

/// The picture as a tight Y plane of `w * h` bytes and a tight UV plane of `w * h / 2` bytes. What the picture does not
/// cover is black.
pub(super) fn tight_nv12(
    frame: &crate::Frame,
    w: usize,
    h: usize,
) -> Result<(Vec<u8>, Vec<u8>), String> {
    let planes = frame.to_nv12()?;
    let mut y = vec![16u8; w * h];
    let mut uv = vec![128u8; w * h / 2];
    let (fw, fh) = (frame.width() as usize, frame.height() as usize);
    for row in 0..h.min(fh) {
        let n = w.min(fw);
        if let (Some(src), Some(dst)) = (
            planes
                .y
                .get(row * planes.y_stride..row * planes.y_stride + n),
            y.get_mut(row * w..row * w + n),
        ) {
            dst.copy_from_slice(src);
        }
    }
    for row in 0..(h / 2).min(fh.div_ceil(2)) {
        let n = w.min(fw + (fw & 1));
        if let (Some(src), Some(dst)) = (
            planes
                .uv
                .get(row * planes.uv_stride..row * planes.uv_stride + n),
            uv.get_mut(row * w..row * w + n),
        ) {
            dst.copy_from_slice(src);
        }
    }
    Ok((y, uv))
}

#[allow(clippy::too_many_arguments)]
pub(super) extern "C" fn read_nv12(
    t: u64,
    i: u32,
    x: u64,
    y: u64,
    w: u64,
    h: u64,
    ydst: u64,
    ystride: u64,
    ysize: u64,
    uvdst: u64,
    uvstride: u64,
    uvsize: u64,
) -> u32 {
    crate::codec::trace(format_args!(
        "read nv12 picture {i} {w}x{h} y {ydst:#x} uv {uvdst:#x}"
    ));
    static ONCE: AtomicBool = AtomicBool::new(false);
    let (x, y, w, h) = (x as u16, y as u16, u64::from(w as u16), u64::from(h as u16));
    if x != 0 || y != 0 || ystride != w || uvstride != w || ysize < w * h || uvsize < w * h / 2 {
        red_once(
            &ONCE,
            &format!(
                "nn::codec::ReadDecoderOutputImageNv12: region {x},{y} strides {ystride} / {uvstride}: only whole tight pictures are served"
            ),
        );
        return 0;
    }
    let Some(frame) = pool_frame(t, i) else {
        let _ = guest::fill(ydst, ysize, 16);
        let _ = guest::fill(uvdst, uvsize, 128);
        return 0;
    };
    match tight_nv12(&frame, w as usize, h as usize) {
        Ok((yp, uvp)) => {
            let _ = guest::write(ydst, &yp);
            let _ = guest::write(uvdst, &uvp);
            // The game samples these planes with its own shader: the GPU must see that the host wrote them.
            crate::guest_wrote(ydst, yp.len() as u64);
            crate::guest_wrote(uvdst, uvp.len() as u64);
            crate::picture::read_nv12(frame.id(), ydst, uvdst, w as u32, h as u32);
        }
        Err(e) => red(&format!("nn::codec::ReadDecoderOutputImageNv12: {e}")),
    }
    0
}
