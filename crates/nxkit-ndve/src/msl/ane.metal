
// ---------------------------------------------------------------------------------------------------------------------
// The Neural Engine path (docs/plans/ndve-ane.md). The model takes the picture as planar f16 and gives `final` and
// `delta`; these three kernels are the GPU's share. The tensors are R16Float textures over IOSurfaces: a tensor
// [1, C, H, W] is W wide and C * H rows high, plane c at rows c * H .. c * H + H - 1.
// ---------------------------------------------------------------------------------------------------------------------

// Step 1: the colour conversion of the game (`convert_bytes`) written as the model's input x [1, 4, 1080, 1920]:
// R, G, B as k / 255 in f16, plane 3 = 1.0. Buffer 0 is the conversion's push block, the planes are the textures 0 and
// 1, the tensor is the texture 2.
kernel void ndve_ane_input(constant float *cp [[buffer(0)]], texture2d<float, access::read> luma [[texture(0)]],
                           texture2d<float, access::sample> chroma [[texture(1)]],
                           texture2d<half, access::write> x [[texture(2)]], uint2 g [[thread_position_in_grid]]) {
    if (g.x >= 1920 || g.y >= 1080) return;
    const float3 c = convert_bytes(luma, chroma, cp, g) / 255.0f;
    x.write(half4(half(c.x)), g);
    x.write(half4(half(c.y)), uint2(g.x, g.y + 1080));
    x.write(half4(half(c.z)), uint2(g.x, g.y + 2160));
    x.write(half4(1.0h), uint2(g.x, g.y + 3240));
}

// Step 2: `final` [1, 3, 1080, 1920] (texture 0) to the RGB10A2 picture the game's layer 12 writes (texture 1). The
// image write quantises to 10 bits, which is the model's missing round(final * 1023) / 1023.
kernel void ndve_ane_quant(texture2d<half, access::read> fin [[texture(0)]],
                           texture2d<float, access::write> pic [[texture(1)]], uint2 g [[thread_position_in_grid]]) {
    if (g.x >= 1920 || g.y >= 1080) return;
    const float3 v = float3(float(fin.read(g).x), float(fin.read(uint2(g.x, g.y + 1080)).x),
                            float(fin.read(uint2(g.x, g.y + 2160)).x));
    pic.write(float4(clamp(v, 0.0f, 1.0f), 1.0f), g);
}

// Steps 3 to 5: the linear sample of the picture (bilinear 4/3, f16), the add of `delta` [1, 1, 1440, 2560] (texture 3),
// the game's dither and the flip, as ndve_resample's epilogue. Buffer 0 is the resampler's push block (84 bytes):
// 10, 11 floats: the output pixel size in uv; 18, 19: the row to flip about and the flip on; 20: quantise and dither on.
kernel void ndve_ane_epilogue(constant uint *pu [[buffer(0)]], texture2d<half, access::sample> pic [[texture(0)]],
                              texture2d<float, access::read> dith [[texture(1)]],
                              texture2d<float, access::write> dst [[texture(2)]],
                              texture2d<half, access::read> delta [[texture(3)]], uint2 g [[thread_position_in_grid]]) {
    if (g.x >= 2560 || g.y >= 1440) return;
    constexpr sampler smp(coord::normalized, address::clamp_to_edge, filter::linear);
    const float2 pxs = float2(as_type<float>(pu[10]), as_type<float>(pu[11]));
    const float2 uv = (float2(g) + 0.5f) * pxs;
    const half d = delta.read(g).x;
    float3 v = float3(pic.sample(smp, uv).xyz + d);
    if (pu[20] != 0) {
        const float3 dv = dith.read(uint2(g.x & 511, g.y & 511)).xyz;
        v = rint(fma(v, 255.0f, fma(dv, 2.0f, -1.0f))) * 0.0039215688593685626983642578125f;
    }
    const uint yo = pu[19] != 0 ? uint(int(pu[18]) - int(g.y)) : g.y;
    dst.write(float4(v, 1.0f), uint2(g.x, yo));
}
