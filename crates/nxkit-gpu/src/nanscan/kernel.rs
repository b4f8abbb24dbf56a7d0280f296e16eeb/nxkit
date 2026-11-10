//! The counting kernels of `nanscan` and the layout of a record.
//!
//! A record is `WORDS` 32-bit words in the shared buffer:
//!
//! | words | content |
//! |---|---|
//! | 0 | the texels with a non-finite channel |
//! | 1 to 4 | the texels with a non-finite R, G, B and A |
//! | 5 to 7 | texels with a non-finite RGB and an alpha that is 0 (+0 or -0), finite and not 0, non-finite |
//! | 8 to 31 | x, y and slice of the first 8 texels the kernel found |
//! | 32 to 287 | the texels per slice (layered kinds only), for the first `SLICES` slices |
//!
//! All tests are on the bits of the float that `read()` gives: a half, a 32-bit float or a field of `Rg11B10Float`
//! with an all-ones exponent is a float with exponent 255. The compiler may assume that a float is finite, so no
//! `isnan` or `isinf` is used.

/// The words of one record.
pub(super) const WORDS: usize = 288;
/// The word of the first coordinate triple.
pub(super) const FIRST_AT: usize = 8;
/// The coordinate triples of a record.
pub(super) const FIRST: usize = 8;
/// The word of the count of slice 0.
pub(super) const SLICE_AT: usize = 32;
/// The slices a record counts.
pub(super) const SLICES: usize = 256;

pub(super) const LIBRARY: &str = "nxkit-nanscan";

/// The entry for a 2D texture.
pub(super) const ENTRY_2D: &str = "nxkit_nanscan_2d";
/// The entry for a 2D array texture (also a cube or cube array seen as one).
pub(super) const ENTRY_ARRAY: &str = "nxkit_nanscan_array";
/// The entry for a 3D texture.
pub(super) const ENTRY_3D: &str = "nxkit_nanscan_3d";

pub(super) const MSL: &str = r"
#include <metal_stdlib>
using namespace metal;

constant uint FIRST_AT = 8u;
constant uint SLICE_AT = 32u;
constant uint SLICES = 256u;

static void scan(float4 c, uint3 p, bool layered, device atomic_uint* rec)
{
    uint4 bits = as_type<uint4>(c);
    bool4 bad = (bits & 0x7f800000u) == 0x7f800000u;
    if (!any(bad)) {
        return;
    }
    uint n = atomic_fetch_add_explicit(&rec[0], 1u, memory_order_relaxed);
    for (uint i = 0u; i < 4u; i++) {
        if (bad[i]) {
            atomic_fetch_add_explicit(&rec[1u + i], 1u, memory_order_relaxed);
        }
    }
    if (any(bad.xyz)) {
        uint alpha = (bits.w & 0x7fffffffu) == 0u ? 5u : (bad.w ? 7u : 6u);
        atomic_fetch_add_explicit(&rec[alpha], 1u, memory_order_relaxed);
    }
    if (n < 8u) {
        atomic_store_explicit(&rec[FIRST_AT + 3u * n], p.x, memory_order_relaxed);
        atomic_store_explicit(&rec[FIRST_AT + 3u * n + 1u], p.y, memory_order_relaxed);
        atomic_store_explicit(&rec[FIRST_AT + 3u * n + 2u], p.z, memory_order_relaxed);
    }
    if (layered && p.z < SLICES) {
        atomic_fetch_add_explicit(&rec[SLICE_AT + p.z], 1u, memory_order_relaxed);
    }
}

kernel void nxkit_nanscan_2d(
    texture2d<float, access::read> t [[texture(0)]],
    device atomic_uint* rec [[buffer(0)]],
    uint3 gid [[thread_position_in_grid]])
{
    if (gid.x >= t.get_width() || gid.y >= t.get_height()) {
        return;
    }
    scan(t.read(gid.xy), uint3(gid.xy, 0u), false, rec);
}

kernel void nxkit_nanscan_array(
    texture2d_array<float, access::read> t [[texture(0)]],
    device atomic_uint* rec [[buffer(0)]],
    uint3 gid [[thread_position_in_grid]])
{
    if (gid.x >= t.get_width() || gid.y >= t.get_height() || gid.z >= t.get_array_size()) {
        return;
    }
    scan(t.read(gid.xy, gid.z), gid, true, rec);
}

kernel void nxkit_nanscan_3d(
    texture3d<float, access::read> t [[texture(0)]],
    device atomic_uint* rec [[buffer(0)]],
    uint3 gid [[thread_position_in_grid]])
{
    if (gid.x >= t.get_width() || gid.y >= t.get_height() || gid.z >= t.get_depth()) {
        return;
    }
    scan(t.read(gid), gid, true, rec);
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_of_the_record_matches_the_kernel() {
        assert!(MSL.contains(&format!("FIRST_AT = {FIRST_AT}u")));
        assert!(MSL.contains(&format!("SLICE_AT = {SLICE_AT}u")));
        assert!(MSL.contains(&format!("SLICES = {SLICES}u")));
        const { assert!(FIRST_AT + 3 * FIRST <= SLICE_AT) };
        assert_eq!(SLICE_AT + SLICES, WORDS);
        for entry in [ENTRY_2D, ENTRY_ARRAY, ENTRY_3D] {
            assert!(MSL.contains(&format!("kernel void {entry}(")));
        }
    }
}
