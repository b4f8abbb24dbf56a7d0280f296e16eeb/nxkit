
// ---------------------------------------------------------------------------------------------------------------------
// The resampler 4f41ccf2 (ndve-native.md section 8): the picture 1920x1080 -> 2560x1440.
//
// Luma pass. The network reads the luma of the RGB10A2 picture as f16. It is computed once per picture pixel into a
// buffer (4 MB) instead of 7 times in the windows. 0.03 ms.
// ---------------------------------------------------------------------------------------------------------------------
kernel void ndve_luma(device half *luma [[buffer(0)]], texture2d<float, access::read> pic [[texture(0)]],
                      uint2 g [[thread_position_in_grid]]) {
    if (g.x >= 1920 || g.y >= 1080) return;
    const float3 c = pic.read(g).xyz;
    const float t = c.y * 0.715200006961822509765625f;
    luma[g.y * 1920 + g.x] = half(fma(c.z, 0.072200000286102294921875f, fma(c.x, 0.2125999927520751953125f, t)));
}

// The network of one 8x8 output block works on a 16x16 patch X (rows = picture rows, columns = picture columns) of luma:
// six blocks of
//   H_g = W1_g X          g = 0, 1   (16x16 times 16x16: 2 x 2 output tiles, one K = 16 step each)
//   Y_j = H_0 W2_0j + H_1 W2_1j      (a chain of two steps, j = 0, 1: the columns 0-7 / 8-15)
//   X = max(Y, 0)                    (not in the last block)
// and the last block only needs the rows 0-7 and columns 0-7 of Y: the 8x8 delta of the block. Each product is rounded
// to f16 (a K = 16 step; the second step of the chain starts from the rounded first). Cost of a block: 172 products
// of 8x8x8 = 88 k MAC; the picture has 57600 blocks: 5.1 GMAC, the same as 4.3 of the 3x3 layers.
//
// Tiling. A threadgroup of RS_NSG = 8 simdgroups stages the 12 KB of weights in threadgroup memory once and loops
// RS_ITER = 2 times over RS_NSG * RS_NB blocks of a row; a simdgroup takes RS_NB = 2 blocks side by side, so every
// weight tile it loads serves 2 blocks. Per round and block: 172 products (86 K = 16 steps). The accumulators and the
// stage-1 results stay in registers, nothing goes to device memory between the stages, and the luma window and the
// delta share one threadgroup array (windows 8 KB + weights 12 KB = 20 KB). The arithmetic is the network's: 5.1 GMAC.
// Measured on an M2 Max, in ms of GPU time: 1.27 with 4 simdgroups of 4 blocks, 1.15 with 8 of 2 (more simdgroups hide
// the latency of the window loads and of the barrier; 16 of 2 is 1.13). The window loads, the sampler and the stores
// cost nothing measurable: with each of them removed the time stays 1.14 to 1.15, so the products are the limit (the
// same kernel without the f16 rounding is not faster). Only fewer MACs would help.
constant constexpr int RS_NB = 2;
constant constexpr int RS_NSG = 8;
constant constexpr int RS_ITER = 2;   // 10 threadgroups * 2 * 8 * 2 = 320 blocks a row

// One block of the network on the patches X[b][k tile (rows)][n tile (columns)], in place.
static inline void rs_block(thread hmat (&X)[RS_NB][2][2], threadgroup const half *wb) {
    hmat H[RS_NB][2][2][2];   // [b][g][row tile][column tile]
    _Pragma("clang loop unroll(full)")
    for (int g = 0; g < 2; g++) {
        hmat A[2][2];   // W1_g tile [row tile mi][k tile ki], stored [ki][mi]
        _Pragma("clang loop unroll(full)")
        for (int mi = 0; mi < 2; mi++) {
            _Pragma("clang loop unroll(full)")
            for (int ki = 0; ki < 2; ki++) simdgroup_load(A[mi][ki], wb + 256 * g + 128 * ki + 64 * mi, 8);
        }
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < RS_NB; b++) {
            _Pragma("clang loop unroll(full)")
            for (int mi = 0; mi < 2; mi++) {
                _Pragma("clang loop unroll(full)")
                for (int ni = 0; ni < 2; ni++) {
                    fmat acc = zero_acc();
                    hstep(acc, A[mi][0], A[mi][1], X[b][0][ni], X[b][1][ni]);
                    H[b][g][mi][ni] = to_half(acc);
                }
            }
        }
    }
    _Pragma("clang loop unroll(full)")
    for (int j = 0; j < 2; j++) {
        hmat B0[2], B1[2];   // W2_gj: two 8x8 sub-tiles (k 0-7, k 8-15)
        _Pragma("clang loop unroll(full)")
        for (int g = 0; g < 2; g++) {
            simdgroup_load(B0[g], wb + 512 + 256 * g + 128 * j, 8);
            simdgroup_load(B1[g], wb + 512 + 256 * g + 128 * j + 64, 8);
        }
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < RS_NB; b++) {
            _Pragma("clang loop unroll(full)")
            for (int mi = 0; mi < 2; mi++) {
                fmat acc = zero_acc();
                hstep(acc, H[b][0][mi][0], H[b][0][mi][1], B0[0], B1[0]);
                hstep(acc, H[b][1][mi][0], H[b][1][mi][1], B0[1], B1[1]);
                X[b][mi][j] = relu_half(acc);   // stage 1 lives in H, so X may be overwritten
            }
        }
    }
}

// The last block: rows 0-7 and columns 0-7 of Y only, no ReLU.
static inline void rs_last(thread hmat (&X)[RS_NB][2][2], threadgroup const half *wb, thread hmat (&D)[RS_NB]) {
    hmat H[RS_NB][2][2];   // [b][g][column tile]
    _Pragma("clang loop unroll(full)")
    for (int g = 0; g < 2; g++) {
        hmat A0, A1;
        simdgroup_load(A0, wb + 256 * g, 8);
        simdgroup_load(A1, wb + 256 * g + 128, 8);
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < RS_NB; b++) {
            _Pragma("clang loop unroll(full)")
            for (int ni = 0; ni < 2; ni++) {
                fmat acc = zero_acc();
                hstep(acc, A0, A1, X[b][0][ni], X[b][1][ni]);
                H[b][g][ni] = to_half(acc);
            }
        }
    }
    hmat B0[2], B1[2];
    _Pragma("clang loop unroll(full)")
    for (int g = 0; g < 2; g++) {
        simdgroup_load(B0[g], wb + 512 + 256 * g, 8);
        simdgroup_load(B1[g], wb + 512 + 256 * g + 64, 8);
    }
    _Pragma("clang loop unroll(full)")
    for (int b = 0; b < RS_NB; b++) {
        fmat acc = zero_acc();
        hstep(acc, H[b][0][0], H[b][0][1], B0[0], B1[0]);
        hstep(acc, H[b][1][0], H[b][1][1], B0[1], B1[1]);
        D[b] = to_half(acc);
    }
}

// Push words (84 bytes): 2, 3 the largest x / y of the luma window; 10, 11 floats: the output pixel size in uv;
// 18, 19: the row to flip about and the flip on; 20: quantise and dither on. The window of the output block (bx, by)
// starts at (6 bx - 5, 6 by - 5) in the picture, 16x16, coordinates clamped to the picture (ndve-native.md, inferred).
// The output pixel (8 bx + col, 8 by + row) is the sample of the picture at its centre (linear, clamp to edge) plus the
// delta of the network, dithered and rounded to 8 bits when w20 is set.
kernel void ndve_resample(device const half *luma [[buffer(0)]], device const half *wsrc [[buffer(1)]],
                          constant uint *pu [[buffer(2)]], texture2d<half, access::sample> pic [[texture(0)]],
                          texture2d<float, access::read> dith [[texture(1)]],
                          texture2d<float, access::write> dst [[texture(2)]], uint2 tg [[threadgroup_position_in_grid]],
                          uint sg [[simdgroup_index_in_threadgroup]], uint lane [[thread_index_in_simdgroup]],
                          uint tid [[thread_index_in_threadgroup]]) {
    // BEST GUESS: the game's sampler is not bound by the hook; linear, clamp to edge, normalized.
    constexpr sampler smp(coord::normalized, address::clamp_to_edge, filter::linear);
    threadgroup uint4 wq[768];   // the 6144 halves of the six blocks
    threadgroup half win[RS_NSG][RS_NB][256];   // the luma windows; after the products the 8x8 deltas reuse the front
    for (uint i = tid; i < 768; i += RS_NSG * 32) wq[i] = ((device const uint4 *)wsrc)[i];
    threadgroup_barrier(mem_flags::mem_threadgroup);
    threadgroup const half *wt = (threadgroup const half *)wq;
    const int maxx = int(pu[2]), maxy = int(pu[3]);
    const float2 pxs = float2(as_type<float>(pu[10]), as_type<float>(pu[11]));
    const bool quant = pu[20] != 0, flip = pu[19] != 0;
    const int flip_row = int(pu[18]);
    const int by = int(tg.y);
    for (int it = 0; it < RS_ITER; it++) {
        const int bx0 = ((int(tg.x) * RS_ITER + it) * RS_NSG + int(sg)) * RS_NB;
        for (int b = 0; b < RS_NB; b++) {
            for (int k = 0; k < 8; k++) {
                const int idx = int(lane) + 32 * k, r = idx >> 4, c = idx & 15;
                const int yy = clamp(6 * by - 5 + r, 0, maxy), xx = clamp(6 * (bx0 + b) - 5 + c, 0, maxx);
                win[sg][b][idx] = luma[yy * 1920 + xx];
            }
        }
        simdgroup_barrier(mem_flags::mem_threadgroup);
        hmat X[RS_NB][2][2];   // [b][k tile (rows)][n tile (columns)]
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < RS_NB; b++) {
            _Pragma("clang loop unroll(full)")
            for (int a = 0; a < 2; a++) {
                _Pragma("clang loop unroll(full)")
                for (int c = 0; c < 2; c++) simdgroup_load(X[b][a][c], &win[sg][b][a * 128 + c * 8], 16);
            }
        }
        _Pragma("clang loop unroll(full)")
        for (int blk = 0; blk < 5; blk++) rs_block(X, wt + 1024 * blk);
        hmat D[RS_NB];
        rs_last(X, wt + 1024 * 5, D);
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < RS_NB; b++) simdgroup_store(D[b], &win[sg][b][0], 8);
        simdgroup_barrier(mem_flags::mem_threadgroup);
        for (int b = 0; b < RS_NB; b++) {
            for (int e = 0; e < 2; e++) {
                const int i = int(lane) + 32 * e, row = i >> 3, col = i & 7;
                const uint x = uint(8 * (bx0 + b) + col), y = uint(8 * by + row);
                const float delta = float(win[sg][b][i]);
                const float2 uv = (float2(float(x), float(y)) + 0.5f) * pxs;
                const float3 s = float3(pic.sample(smp, uv).xyz);
                float3 v = -0.0f + (s + delta);
                if (quant) {
                    const float3 dv = dith.read(uint2(x & 511, y & 511)).xyz;
                    v = rint(fma(v, 255.0f, fma(dv, 2.0f, -1.0f))) * 0.0039215688593685626983642578125f;
                }
                const uint yo = flip ? uint(flip_row - int(y)) : y;
                dst.write(float4(v, 1.0f), uint2(x, yo));
            }
        }
        simdgroup_barrier(mem_flags::mem_threadgroup);
    }
}
