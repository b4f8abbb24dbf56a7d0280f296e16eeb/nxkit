// nn::ndve's movie network (docs/plans/ndve-native.md) as Metal kernels for Apple GPUs.
//
// Arithmetic (ndve-native.md sections 5 to 8): every HMMA.16816.F16 of the game is a K = 16 step whose result D is
// rounded to f16 before the next step. Here a step is two simdgroup 8x8x8 products on an f32 accumulator, then the
// accumulator is rounded to f16 and widened again (hstep). A chain runs over the taps, then the 16-channel blocks,
// from C = 0 (or from the skip tensor in the up layers). The epilogue is relu(f16(D + bias)) in f16.
//
// Nothing here depends on how a lane holds the elements of an 8x8 matrix: rounding and the epilogue work on
// thread_elements() of matrices in the same layout, the bias is a matrix loaded with each row equal to the bias
// vector, and results leave through simdgroup_store, so the code holds on any Apple GPU family.
//
// Safe math keeps the additions of the final layer and the colour conversion in the game's order.
#pragma METAL fp math_mode(safe)
#include <metal_stdlib>
using namespace metal;

typedef simdgroup_half8x8 hmat;
typedef simdgroup_float8x8 fmat;

static inline fmat zero_acc() { return make_filled_simdgroup_matrix<float, 8, 8>(0.0f); }

// The two elements a lane holds of a matrix, as a vector (the same two positions in every matrix of a type).
static inline float2 elems(fmat m) { return float2(m.thread_elements()[0], m.thread_elements()[1]); }
static inline float2 elems(hmat m) { return float2(float(m.thread_elements()[0]), float(m.thread_elements()[1])); }
static inline half2 helems(hmat m) { return half2(m.thread_elements()[0], m.thread_elements()[1]); }
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
// relu(D) of the resampler's blocks.
static inline hmat relu_half(fmat a) {
    hmat h;
    set_elems(h, max(half2(elems(a)), half2(0.0h)));
    return h;
}
// relu(f16(D + bias)) of the layers 1 to 11: HFMA2.RELU with a multiplier of 1.0 is one rounding of the sum.
static inline hmat bias_relu(fmat a, hmat bias) {
    hmat h;
    set_elems(h, max(half2(elems(a)) + helems(bias), half2(0.0h)));
    return h;
}

// The colour conversion 49dff010 (NV12 to RGBA8) of one picture pixel, as the 8-bit unorm value the game's image holds
// (the write rounds to nearest even, the read gives c / 255). Push words: matrix rows at 0-2, 4-6, 8-10, offsets at
// 12-14. The chroma plane is sampled bilinearly at the pixel centre (the sampler of the game is not known to us:
// linear, clamp to edge).
static inline float3 convert_px(texture2d<float, access::read> luma, texture2d<float, access::sample> chroma,
                                constant float *cp, uint2 p) {
    // BEST GUESS: the hook cannot bind the game's sampler; linear, clamp to edge, normalized as section 7 implies.
    constexpr sampler smp(coord::normalized, address::clamp_to_edge, filter::linear);
    const float2 size = float2(luma.get_width(), luma.get_height());
    const float yv = luma.read(p).x + cp[12];
    const float2 c = chroma.sample(smp, (float2(p) + 0.5f) / size).xy;
    const float uv = c.x + cp[13], vv = c.y + cp[14];
    const float r = fma(vv, cp[2], fma(yv, cp[0], uv * cp[1]));
    const float g = fma(vv, cp[6], fma(yv, cp[4], uv * cp[5]));
    const float b = fma(vv, cp[10], fma(yv, cp[8], uv * cp[9]));
    return rint(clamp(float3(r, g, b), 0.0f, 1.0f) * 255.0f) / 255.0f;
}
