//! Vector maths on `f32` slices. Accelerate (vDSP) does the work on Apple platforms, a plain loop elsewhere.

#![allow(unsafe_code)]

#[cfg(target_vendor = "apple")]
mod acc {
    #[link(name = "Accelerate", kind = "framework")]
    unsafe extern "C" {
        pub(super) fn vDSP_vsma(
            a: *const f32,
            ia: isize,
            b: *const f32,
            c: *const f32,
            ic: isize,
            d: *mut f32,
            id: isize,
            n: usize,
        );
        pub(super) fn vDSP_vclip(
            a: *const f32,
            ia: isize,
            lo: *const f32,
            hi: *const f32,
            d: *mut f32,
            id: isize,
            n: usize,
        );
        pub(super) fn vDSP_vsmul(a: *const f32, ia: isize, b: *const f32, c: *mut f32, ic: isize, n: usize);
    }
}

/// `dst[i] += src[i] * k`.
pub fn madd(dst: &mut [f32], src: &[f32], k: f32) {
    let n = dst.len().min(src.len());
    if n == 0 {
        return;
    }
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: both pointers cover `n` floats and do not overlap, because they come from a `&mut` and a `&`. The output
        // may alias the addend `c`, which vDSP allows for an exact overlap.
        unsafe { acc::vDSP_vsma(src.as_ptr(), 1, &k, dst.as_ptr(), 1, dst.as_mut_ptr(), 1, n) };
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        for i in 0..n {
            dst[i] += src[i] * k;
        }
    }
}

/// Limits every value to `lo..=hi`.
pub fn clip(buf: &mut [f32], lo: f32, hi: f32) {
    let n = buf.len();
    if n == 0 {
        return;
    }
    #[cfg(target_vendor = "apple")]
    {
        let p = buf.as_mut_ptr();
        // SAFETY: the pointer covers `n` floats. Input and output are the same range, which vDSP allows.
        unsafe { acc::vDSP_vclip(p.cast_const(), 1, &lo, &hi, p, 1, n) };
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        for v in buf.iter_mut() {
            *v = v.clamp(lo, hi);
        }
    }
}

/// Multiplies every value by `k`.
pub fn scale(buf: &mut [f32], k: f32) {
    let n = buf.len();
    if n == 0 {
        return;
    }
    #[cfg(target_vendor = "apple")]
    {
        let p = buf.as_mut_ptr();
        // SAFETY: the pointer covers `n` floats. Input and output are the same range, which vDSP allows.
        unsafe { acc::vDSP_vsmul(p.cast_const(), 1, &k, p, 1, n) };
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        for v in buf.iter_mut() {
            *v *= k;
        }
    }
}
