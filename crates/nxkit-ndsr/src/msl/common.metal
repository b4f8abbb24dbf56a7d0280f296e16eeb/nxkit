// agl's NDSR upscaler (docs/plans/ndsr-native.md) as a Metal kernel for Apple GPUs.
//
// The arithmetic pieces are those of nn::ndve's resampler (crates/nxkit-ndve/src/msl/common.metal, copied: a kernel
// source is one text per crate): every HMMA.16816.F16 of the game is a K = 16 step whose result is rounded to f16 before
// the next step. Here a step is two simdgroup 8x8x8 products on an f32 accumulator, then the accumulator is rounded to
// f16 and widened again (hstep).
//
// Nothing here depends on how a lane holds the elements of an 8x8 matrix: rounding works on thread_elements() of
// matrices in the same layout and results leave through simdgroup_store, so the code holds on any Apple GPU family.
//
// Safe math keeps the additions of the epilogue in the game's order; the fused multiply-adds the game uses are written
// as fma() and no others are made.
#pragma METAL fp math_mode(safe)
#pragma METAL fp contract(off)
#include <metal_stdlib>
using namespace metal;

typedef simdgroup_half8x8 hmat;
typedef simdgroup_float8x8 fmat;

static inline fmat zero_acc() { return make_filled_simdgroup_matrix<float, 8, 8>(0.0f); }

// The two elements a lane holds of a matrix, as a vector (the same two positions in every matrix of a type).
static inline float2 elems(fmat m) { return float2(m.thread_elements()[0], m.thread_elements()[1]); }
static inline void set_elems(thread fmat &m, float2 v) { m.thread_elements()[0] = v.x; m.thread_elements()[1] = v.y; }
static inline void set_elems(thread hmat &m, half2 v) { m.thread_elements()[0] = v.x; m.thread_elements()[1] = v.y; }

// One HMMA of K = 16: acc = f16(acc + a0 * b0 + a1 * b1), with a0 / a1 the inputs 0-7 / 8-15 of the block and b0 / b1
// the weight rows 0-7 / 8-15. The accumulator holds f16 values between steps, as the hardware's D does.
static inline void hstep(thread fmat &acc, hmat a0, hmat a1, hmat b0, hmat b1) {
    simdgroup_multiply_accumulate(acc, a0, b0, acc);
    simdgroup_multiply_accumulate(acc, a1, b1, acc);
    set_elems(acc, float2(half2(elems(acc))));
}

// The accumulator (already f16 valued) as an f16 matrix.
static inline hmat to_half(fmat a) {
    hmat h;
    set_elems(h, half2(elems(a)));
    return h;
}
// relu(D) of the network's layers 0 to 4.
static inline hmat relu_half(fmat a) {
    hmat h;
    set_elems(h, max(half2(elems(a)), half2(0.0h)));
    return h;
}
