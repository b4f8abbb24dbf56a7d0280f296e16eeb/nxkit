
// ---------------------------------------------------------------------------------------------------------------------
// The twelve convolution layers (layers 1 + 2 and 11 + 12 are fused: see ndve_l01_l02 and ndve_l11_l12 below).
//
// Data. Activations are NHWC f16 in device buffers, element (x, y, c) at (y * W + x) * C + c, no border. The weights of
// a layer arrive packed by pack.rs: 8x8 tiles of the game's [tap][cin][cout] matrix, tile (tap, k8, n8) at index
// (tap * (CIN / 8) + k8) * (COUT / 8) + n8, 64 halves a tile (row = input channel, 8 halves a row), followed by the
// bias as COUT / 8 tiles whose 8 rows each hold the 8 biases. So one simdgroup_load with a row stride of 8 reads an
// operand, and the bias comes as a matrix in the layout of the accumulator without knowing that layout.
//
// Implicit GEMM. A = output pixels x (taps x CIN), B = weights, C = pixels x COUT. An m-tile is 8 pixels of one row, so
// an A tile is a simdgroup_load of 8 pixels (row stride = the pixel pitch) by 8 channels, and the 3x3 taps are plain
// pointer offsets into the staged input.
//
// Costs, on an M2 Max (30 cores, 128 FMA lanes each, 1.4 GHz: 5.4 TMAC/s peak). A 3x3 layer is 1.19 GMAC at every
// level, 2.3 M simdgroup products. The estimates below assume 40 % of peak (about 2.2 TMAC/s), the rate of the
// 8x8 simdgroup_matrix kernels with an f16 round after every second product, so about 0.55 ms for each 3x3 layer and
// 0.12 ms for each 2x2 layer (0.27 GMAC). Memory is not the limit: the biggest activation tensor is 16.6 MB.
// ---------------------------------------------------------------------------------------------------------------------

// A 3x3 stride-1 layer with zero padding. A threadgroup computes a TW x TH tile of output pixels for all COUT channels.
// It stages the (TW + 2) x (TH + 2) input pixels once in threadgroup memory (out-of-image pixels are zeros: that is the
// padding), then every simdgroup runs the 9 taps x CIN / 16 steps on its own part of the tile: MT m-tiles and COUT / 8 / NG
// n-tiles, so MT * NT accumulators. The B tiles of a step are loaded once and used for all MT m-tiles, an A tile for all
// NT n-tiles: per step MT * 2 A loads and NT * 2 B loads feed MT * NT * 2 products. The pixel pitch of the staged
// tile is CIN + 8 halves so that the 8 rows of an A load fall in different banks. The halo makes the staged tile
// 1.4 to 1.7 times the pixels of the tile; it is read from L2, not the limit.
template <int CIN, int COUT, int TW, int TH, int MT, int NG, int W, int H>
struct Conv3 {
    static constant constexpr int TILE_W = TW;
    static constant constexpr int TILE_H = TH;
    static constant constexpr int M_TILES = MT;
    static constant constexpr int MS = TW / 8;            // m-tiles in a tile row
    static constant constexpr int MG = MS * TH / MT;      // simdgroups along m
    static constant constexpr int NSG = MG * NG;
    static constant constexpr int NT = COUT / 8 / NG;     // n-tiles of a simdgroup
    static constant constexpr int PPX = CIN + 8;          // pixel pitch of the staged tile, halves
    static constant constexpr int SW = TW + 2;
    static constant constexpr int SH = TH + 2;
    static constant constexpr int STAGED = SW * SH * PPX; // halves
    static constant constexpr int THREADS = NSG * 32;
    static constant constexpr int WEIGHTS = 9 * CIN * COUT;   // halves; the bias tiles follow

    // The input tile from a buffer, 16 bytes at a time, zeros outside the picture.
    static inline void stage_buf(threadgroup half *st, device const half *in, uint tid, int tx0, int ty0) {
        constexpr int CH = CIN / 8;
        for (int i = int(tid); i < SW * SH * CH; i += THREADS) {
            const int c = i % CH, p = i / CH;
            const int gx = tx0 - 1 + p % SW, gy = ty0 - 1 + p / SW;
            uint4 v = uint4(0);
            if (gx >= 0 && gx < W && gy >= 0 && gy < H) {
                v = ((device const uint4 *)(in + (long(gy) * W + gx) * CIN))[c];
            }
            ((threadgroup uint4 *)(st + p * PPX))[c] = v;
        }
    }

    // The chain of the layer for the simdgroup's tiles: taps in order, 16-channel blocks within a tap, from C = 0.
    static inline void gemm(threadgroup const half *st, device const half *w, uint sg, thread fmat (&acc)[MT][NT]) {
        const uint mg = sg / NG, ng = sg % NG;
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < MT; i++) {
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) acc[i][j] = zero_acc();
        }
        for (int t = 0; t < 9; t++) {
            const int dy = t / 3, dx = t % 3;
            for (int kb = 0; kb < CIN / 16; kb++) {
                hmat b0[NT], b1[NT];
                device const half *bp = w + ((t * (CIN / 8) + kb * 2) * (COUT / 8) + int(ng) * NT) * 64;
                _Pragma("clang loop unroll(full)")
                for (int j = 0; j < NT; j++) {
                    simdgroup_load(b0[j], bp + j * 64, 8);
                    simdgroup_load(b1[j], bp + (COUT / 8 + j) * 64, 8);
                }
                _Pragma("clang loop unroll(full)")
                for (int i = 0; i < MT; i++) {
                    const int m = int(mg) * MT + i;
                    threadgroup const half *ap = st + ((m / MS + dy) * SW + (m % MS) * 8 + dx) * PPX + kb * 16;
                    hmat a0, a1;
                    simdgroup_load(a0, ap, PPX);
                    simdgroup_load(a1, ap + 8, PPX);
                    _Pragma("clang loop unroll(full)")
                    for (int j = 0; j < NT; j++) hstep(acc[i][j], a0, a1, b0[j], b1[j]);
                }
            }
        }
    }

    // relu(f16(D + bias)) of the tiles, stored to the output tensor.
    static inline void store_relu(device half *out, device const half *bias, uint sg, int tx0, int ty0,
                                  thread fmat (&acc)[MT][NT]) {
        const uint mg = sg / NG, ng = sg % NG;
        hmat bm[NT];
        _Pragma("clang loop unroll(full)")
        for (int j = 0; j < NT; j++) simdgroup_load(bm[j], bias + (int(ng) * NT + j) * 64, 8);
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < MT; i++) {
            const int m = int(mg) * MT + i;
            const int y = ty0 + m / MS, x = tx0 + (m % MS) * 8;
            if (y >= H) continue;   // the simdgroup's whole tile row: uniform
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) {
                simdgroup_store(bias_relu(acc[i][j], bm[j]), out + (long(y) * W + x) * COUT + (int(ng) * NT + j) * 8, COUT);
            }
        }
    }
};

// Layers 4, 6, 7, 9: input from a buffer, ReLU epilogue.
template <class C>
static inline void conv3_buf(device const half *in, device half *out, device const half *wb, threadgroup half *st,
                             uint2 tg, uint sg, uint tid) {
    const int tx0 = int(tg.x) * C::TILE_W, ty0 = int(tg.y) * C::TILE_H;
    C::stage_buf(st, in, tid, tx0, ty0);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    fmat acc[C::M_TILES][C::NT];
    C::gemm(st, wb, sg, acc);
    C::store_relu(out, wb + C::WEIGHTS, sg, tx0, ty0, acc);
}

// Layers 4 and 9: 32 -> 32 at 480x270. Tile 32 x 4, 4 simdgroups of one tile row (MT = 4, NT = 4): 16 products per
// 16 loads, 32 accumulator registers. 15 x 68 threadgroups (the last row of tiles is half empty), staged tile 16 KB.
// 0.5 ms each.
kernel void ndve_l04(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint2 tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]], uint tid [[thread_index_in_threadgroup]]) {
    typedef Conv3<32, 32, 32, 4, 4, 1, 480, 270> C;
    threadgroup uint4 stq[C::STAGED / 8];
    conv3_buf<C>(in, out, wb, (threadgroup half *)stq, tg, sg, tid);
}

kernel void ndve_l09(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint2 tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]], uint tid [[thread_index_in_threadgroup]]) {
    typedef Conv3<32, 32, 32, 4, 4, 1, 480, 270> C;
    threadgroup uint4 stq[C::STAGED / 8];
    conv3_buf<C>(in, out, wb, (threadgroup half *)stq, tg, sg, tid);
}

// Layers 6 and 7: 64 -> 64 at 240x135. Tile 16 x 4 = 64 pixels; the 4 simdgroups are 2 (rows of 2 tile rows) x 2 (halves
// of the 64 output channels), each MT = 4, NT = 4. The two halves load the same A tiles. 15 x 34 = 510 threadgroups of
// 15.5 KB. 0.5 ms each.
kernel void ndve_l06(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint2 tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]], uint tid [[thread_index_in_threadgroup]]) {
    typedef Conv3<64, 64, 16, 4, 4, 2, 240, 135> C;
    threadgroup uint4 stq[C::STAGED / 8];
    conv3_buf<C>(in, out, wb, (threadgroup half *)stq, tg, sg, tid);
}

kernel void ndve_l07(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint2 tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]], uint tid [[thread_index_in_threadgroup]]) {
    typedef Conv3<64, 64, 16, 4, 4, 2, 240, 135> C;
    threadgroup uint4 stq[C::STAGED / 8];
    conv3_buf<C>(in, out, wb, (threadgroup half *)stq, tg, sg, tid);
}

// A 2x2 stride-2 layer. A task is MT m-tiles (8 * MT output pixels) of one output row for all COUT channels, one
// simdgroup a task. The inputs are not shared between taps or neighbours, so the A tiles come straight from the device
// (row stride = 2 pixels) and no staging is needed; they are read once per tap and n-tile group from L1.
template <int CIN, int COUT, int MT, int OW, int OH>
static inline void down_body(device const half *in, device half *out, device const half *wb, uint task) {
    constexpr int NT = COUT / 8, SEGS = OW / (8 * MT), IW = 2 * OW;
    const int seg = int(task) % SEGS, y = int(task) / SEGS;
    if (y >= OH) return;
    const int x0 = seg * 8 * MT;
    fmat acc[MT][NT];
    _Pragma("clang loop unroll(full)")
    for (int i = 0; i < MT; i++) {
        _Pragma("clang loop unroll(full)")
        for (int j = 0; j < NT; j++) acc[i][j] = zero_acc();
    }
    for (int t = 0; t < 4; t++) {
        const int dy = t >> 1, dx = t & 1;
        for (int kb = 0; kb < CIN / 16; kb++) {
            hmat b0[NT], b1[NT];
            device const half *bp = wb + ((t * (CIN / 8) + kb * 2) * NT) * 64;
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) {
                simdgroup_load(b0[j], bp + j * 64, 8);
                simdgroup_load(b1[j], bp + (NT + j) * 64, 8);
            }
            _Pragma("clang loop unroll(full)")
            for (int i = 0; i < MT; i++) {
                device const half *ap = in + ((long(2 * y + dy) * IW + 2 * (x0 + 8 * i) + dx) * CIN + kb * 16);
                hmat a0, a1;
                simdgroup_load(a0, ap, 2 * CIN);
                simdgroup_load(a1, ap + 8, 2 * CIN);
                _Pragma("clang loop unroll(full)")
                for (int j = 0; j < NT; j++) hstep(acc[i][j], a0, a1, b0[j], b1[j]);
            }
        }
    }
    device const half *bias = wb + 4 * CIN * COUT;
    _Pragma("clang loop unroll(full)")
    for (int j = 0; j < NT; j++) {
        hmat bm;
        simdgroup_load(bm, bias + j * 64, 8);
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < MT; i++) {
            simdgroup_store(bias_relu(acc[i][j], bm), out + (long(y) * OW + x0 + 8 * i) * COUT + j * 8, COUT);
        }
    }
}

// Layer 3: 16 -> 32, 960x540 -> 480x270. Tasks of 16 pixels (MT = 2, NT = 4), 8100 tasks, 4 a threadgroup. 0.12 ms.
kernel void ndve_l03(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]]) {
    down_body<16, 32, 2, 480, 270>(in, out, wb, tg * 4 + sg);
}

// Layer 5: 32 -> 64, 480x270 -> 240x135. Tasks of 16 pixels (MT = 2, NT = 8: 16 accumulators), 2025 tasks. 0.12 ms.
kernel void ndve_l05(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], uint tg [[threadgroup_position_in_grid]],
                     uint sg [[simdgroup_index_in_threadgroup]]) {
    down_body<32, 64, 2, 240, 135>(in, out, wb, tg * 4 + sg);
}

// A 2x2 stride-2 transposed layer with the skip tensor as the chain's C. A task is MT m-tiles of input pixels in one
// row. For each tap (dy, dx) the accumulators start as the skip tile at output pixels (2x + dx, 2y + dy) (f16 values:
// a load with a row stride of 2 pixels), CIN / 16 steps add the input, the epilogue is relu(f16(D + bias)) and the
// result goes to the same output pixels. The skip is read and the output written at the same places, so they may not
// overlap other tensors; the plan keeps them apart.
template <int CIN, int COUT, int MT, int IW, int IH>
static inline void up_body(device const half *in, device const half *skip, device half *out, device const half *wb,
                           uint task) {
    constexpr int NT = COUT / 8, SEGS = IW / (8 * MT), OW = 2 * IW;
    const int seg = int(task) % SEGS, y = int(task) / SEGS;
    if (y >= IH) return;
    const int x0 = seg * 8 * MT;
    device const half *bias = wb + 4 * CIN * COUT;
    hmat bm[NT];
    _Pragma("clang loop unroll(full)")
    for (int j = 0; j < NT; j++) simdgroup_load(bm[j], bias + j * 64, 8);
    for (int t = 0; t < 4; t++) {
        const int dy = t >> 1, dx = t & 1;
        fmat acc[MT][NT];
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < MT; i++) {
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) {
                hmat sk;
                simdgroup_load(sk, skip + (long(2 * y + dy) * OW + 2 * (x0 + 8 * i) + dx) * COUT + j * 8, 2 * COUT);
                set_elems(acc[i][j], elems(sk));
            }
        }
        for (int kb = 0; kb < CIN / 16; kb++) {
            hmat b0[NT], b1[NT];
            device const half *bp = wb + ((t * (CIN / 8) + kb * 2) * NT) * 64;
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) {
                simdgroup_load(b0[j], bp + j * 64, 8);
                simdgroup_load(b1[j], bp + (NT + j) * 64, 8);
            }
            _Pragma("clang loop unroll(full)")
            for (int i = 0; i < MT; i++) {
                device const half *ap = in + (long(y) * IW + x0 + 8 * i) * CIN + kb * 16;
                hmat a0, a1;
                simdgroup_load(a0, ap, CIN);
                simdgroup_load(a1, ap + 8, CIN);
                _Pragma("clang loop unroll(full)")
                for (int j = 0; j < NT; j++) hstep(acc[i][j], a0, a1, b0[j], b1[j]);
            }
        }
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < MT; i++) {
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < NT; j++) {
                simdgroup_store(bias_relu(acc[i][j], bm[j]),
                                out + (long(2 * y + dy) * OW + 2 * (x0 + 8 * i) + dx) * COUT + j * 8, 2 * COUT);
            }
        }
    }
}

// Layer 8: 64 -> 32, 240x135 -> 480x270, skip = layer 4's output. Tasks of 16 input pixels (MT = 2, NT = 4), 2025 tasks.
// 0.15 ms.
kernel void ndve_l08(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], device const half *skip [[buffer(3)]],
                     uint tg [[threadgroup_position_in_grid]], uint sg [[simdgroup_index_in_threadgroup]]) {
    up_body<64, 32, 2, 240, 135>(in, skip, out, wb, tg * 4 + sg);
}

// Layer 10: 32 -> 16, 480x270 -> 960x540, skip = layer 2's output. Tasks of 32 input pixels (MT = 4, NT = 2), 4050
// tasks. 0.12 ms.
kernel void ndve_l10(device const half *in [[buffer(0)]], device half *out [[buffer(1)]],
                     device const half *wb [[buffer(2)]], device const half *skip [[buffer(3)]],
                     uint tg [[threadgroup_position_in_grid]], uint sg [[simdgroup_index_in_threadgroup]]) {
    up_body<32, 16, 4, 480, 270>(in, skip, out, wb, tg * 4 + sg);
}

// ---------------------------------------------------------------------------------------------------------------------
// The fused pairs: layers 1 + 2 and 11 + 12 (16 -> 16 -> 16 at 960x540).
//
// A threadgroup computes a TW x TH = 30 x 12 tile of the second layer's output. It needs the first layer's output on the
// tile plus a 1 pixel halo (MW x MH = 32 x 14, exactly 4 m-tiles of 8 pixels a row) and so the first layer's input on
// IW x IH = 34 x 16. The input is staged once (layer 1: converted from the NV12 planes, layer 11: from the buffer),
// the first layer runs on the halo tile, its f16 results wait in registers until every simdgroup is done reading the
// input, then they replace the input in the same threadgroup memory (the union is 26 KB), and the second layer reads
// them from there. The 960x540x16 tensor between the layers never goes to device memory: 16.6 MB written and read
// less, and one dispatch less. The price is the halo: the first layer is computed on 14 / 12 of the rows and the second
// on 32 / 30 of the columns (the last m-tile of a row is shifted to start at 22, so it recomputes pixels 22 and 23 with
// identical results), together 1.12 times the MACs of the two separate layers. 32 x 45 threadgroups cover the image
// exactly (960 = 32 * 30, 540 = 45 * 12).
//
// Padding. The first layer's input outside the image is zeros (as before). Its output outside the image is also zeros:
// the second layer pads with zeros, not with what the first layer would compute there, so those mid pixels (the column
// left of the image, the column right, the row above, the row below) are cleared after the store. Inside the image the
// halo holds real first-layer results.
//
// Rounding is that of the separate layers: f16 after every K = 16 step, relu(f16(D + bias)) as the mid value.
// Work split: 4 simdgroups, simdgroup s takes rows s, s + 4, ... of the first layer's 14 rows and of the second's 12,
// one row (4 m-tiles x 2 n-tiles = 8 accumulators) at a time, so B tiles serve 4 products and A tiles 2.
// Expected, for the pair at the measured clock: the MAC time of the two layers (2 x 0.57 ms) times 1.12 plus the colour
// conversion (about 0.27 ms) and the last layer's per-pixel epilogue: about 1.45 ms for layers 1 + 2 (was 1.48) and
// 1.45 ms for 11 + 12 (was 1.50). The gain is the avoided traffic and the 4 m-tile rows sharing their B loads, not
// fewer MACs, so it is small.
// ---------------------------------------------------------------------------------------------------------------------
struct Pair {
    static constant constexpr int TW = 30;
    static constant constexpr int TH = 12;
    static constant constexpr int W = 960;
    static constant constexpr int H = 540;
    static constant constexpr int IW = TW + 4;
    static constant constexpr int IH = TH + 4;
    static constant constexpr int MW = TW + 2;
    static constant constexpr int MH = TH + 2;
    static constant constexpr int PPX = 24;   // pixel pitch, halves (16 channels + 8: the 8 rows of an A load use other banks)
    static constant constexpr int STAGED = (IW * IH > MW * MH ? IW * IH : MW * MH) * PPX;   // halves
    static constant constexpr int THREADS = 128;
    static constant constexpr int WEIGHTS = 9 * 16 * 16;   // halves; the bias tiles follow
    static constant constexpr int LAST_X = TW - 8;         // start of the last m-tile of an output row

    // The first layer's input from a buffer: the IW x IH pixels at (tx0 - 2, ty0 - 2), zeros outside the picture.
    static inline void stage_buf(threadgroup half *st, device const half *in, uint tid, int tx0, int ty0) {
        for (int i = int(tid); i < IW * IH * 2; i += THREADS) {
            const int c = i & 1, p = i >> 1;
            const int gx = tx0 - 2 + p % IW, gy = ty0 - 2 + p / IW;
            uint4 v = uint4(0);
            if (gx >= 0 && gx < W && gy >= 0 && gy < H) v = ((device const uint4 *)(in + (long(gy) * W + gx) * 16))[c];
            ((threadgroup uint4 *)(st + p * PPX))[c] = v;
        }
    }

    // The same from the NV12 planes: the colour conversion and the 2x2 space-to-depth in one. Packed pixel (X, Y)
    // channel 4 * (2 sy + sx) + c is picture pixel (2 X + sx, 2 Y + sy) component c, with alpha 1.
    static inline void stage_pic(threadgroup half *st, texture2d<float, access::read> luma,
                                 texture2d<float, access::sample> chroma, constant float *cp, uint tid, int tx0, int ty0) {
        for (int i = int(tid); i < IW * IH * 4; i += THREADS) {
            const int s = i & 3, p = i >> 2;
            const int gx = tx0 - 2 + p % IW, gy = ty0 - 2 + p / IW;
            half4 v = half4(0.0h);
            if (gx >= 0 && gx < W && gy >= 0 && gy < H) {
                const float3 c = convert_px(luma, chroma, cp, uint2(2 * gx + (s & 1), 2 * gy + (s >> 1)));
                v = half4(half3(c), 1.0h);
            }
            *((threadgroup half4 *)(st + p * PPX + 4 * s)) = v;
        }
    }

    // One row of output of a 3x3 layer: 4 m-tiles of 8 pixels at x = min(8 i, xmax) of row r, from `src` whose rows are
    // `rowpx` pixels apart (the output pixel (x, r) reads the source pixels (x + dx, r + dy)). Taps in order, one
    // K = 16 step each, from C = 0.
    static inline void gemm_row(threadgroup const half *src, device const half *w, int r, int rowpx, int xmax,
                                thread fmat (&acc)[4][2]) {
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < 4; i++) {
            acc[i][0] = zero_acc();
            acc[i][1] = zero_acc();
        }
        for (int t = 0; t < 9; t++) {
            const int dy = t / 3, dx = t % 3;
            hmat b0[2], b1[2];
            device const half *bp = w + t * 256;
            _Pragma("clang loop unroll(full)")
            for (int j = 0; j < 2; j++) {
                simdgroup_load(b0[j], bp + j * 64, 8);
                simdgroup_load(b1[j], bp + (2 + j) * 64, 8);
            }
            _Pragma("clang loop unroll(full)")
            for (int i = 0; i < 4; i++) {
                const int x = min(8 * i, xmax);
                threadgroup const half *ap = src + ((r + dy) * rowpx + x + dx) * PPX;
                hmat a0, a1;
                simdgroup_load(a0, ap, PPX);
                simdgroup_load(a1, ap + 8, PPX);
                hstep(acc[i][0], a0, a1, b0[0], b1[0]);
                hstep(acc[i][1], a0, a1, b0[1], b1[1]);
            }
        }
    }

    // The first layer on the MH x MW halo tile; relu(f16(D + bias)) stays in registers, keep[b] holds row sg + 4 b.
    static inline void mid_compute(threadgroup const half *st, device const half *w, uint sg, thread hmat (&keep)[4][4][2]) {
        hmat bm[2];
        simdgroup_load(bm[0], w + WEIGHTS, 8);
        simdgroup_load(bm[1], w + WEIGHTS + 64, 8);
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < 4; b++) {
            const int r = int(sg) + 4 * b;
            if (r < MH) {   // uniform per simdgroup
                fmat acc[4][2];
                gemm_row(st, w, r, IW, 24, acc);
                _Pragma("clang loop unroll(full)")
                for (int i = 0; i < 4; i++) {
                    keep[b][i][0] = bias_relu(acc[i][0], bm[0]);
                    keep[b][i][1] = bias_relu(acc[i][1], bm[1]);
                }
            }
        }
    }

    // Writes the first layer's results over the staged input and clears the pixels outside the image. The caller has
    // put a barrier between mid_compute and this (every simdgroup is done reading the input) and puts one after.
    static inline void mid_store(threadgroup half *st, thread hmat (&keep)[4][4][2], uint sg, uint tid, uint2 tg) {
        _Pragma("clang loop unroll(full)")
        for (int b = 0; b < 4; b++) {
            const int r = int(sg) + 4 * b;
            if (r < MH) {
                _Pragma("clang loop unroll(full)")
                for (int i = 0; i < 4; i++) {
                    simdgroup_store(keep[b][i][0], st + (r * MW + 8 * i) * PPX, PPX);
                    simdgroup_store(keep[b][i][1], st + (r * MW + 8 * i) * PPX + 8, PPX);
                }
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        // the stores of one simdgroup are complete for all after the barrier; now the border pixels
        const bool left = tg.x == 0, right = int(tg.x) == W / TW - 1, top = tg.y == 0, bottom = int(tg.y) == H / TH - 1;
        if (left || right || top || bottom) {
            for (int i = int(tid); i < MW * MH; i += THREADS) {
                const int cx = i % MW, cy = i / MW;
                if ((left && cx == 0) || (right && cx == MW - 1) || (top && cy == 0) || (bottom && cy == MH - 1)) {
                    threadgroup uint4 *z = (threadgroup uint4 *)(st + i * PPX);
                    z[0] = uint4(0);
                    z[1] = uint4(0);
                }
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }
};

// Layers 1 + 2. Buffer 1 is layer 2's output (the skip of layer 10 and the input of layer 3), 2 the packed weights of
// layer 1, 5 those of layer 2, 4 the conversion's push block, the planes are the textures 0 and 1.
kernel void ndve_l01_l02(device half *out [[buffer(1)]], device const half *w1 [[buffer(2)]],
                         constant float *cp [[buffer(4)]], device const half *w2 [[buffer(5)]],
                         texture2d<float, access::read> luma [[texture(0)]],
                         texture2d<float, access::sample> chroma [[texture(1)]], uint2 tg [[threadgroup_position_in_grid]],
                         uint sg [[simdgroup_index_in_threadgroup]], uint tid [[thread_index_in_threadgroup]]) {
    typedef Pair P;
    threadgroup uint4 stq[P::STAGED / 8];
    threadgroup half *st = (threadgroup half *)stq;
    const int tx0 = int(tg.x) * P::TW, ty0 = int(tg.y) * P::TH;
    P::stage_pic(st, luma, chroma, cp, tid, tx0, ty0);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    hmat keep[4][4][2];
    P::mid_compute(st, w1, sg, keep);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    P::mid_store(st, keep, sg, tid, tg);
    hmat bm[2];
    simdgroup_load(bm[0], w2 + P::WEIGHTS, 8);
    simdgroup_load(bm[1], w2 + P::WEIGHTS + 64, 8);
    for (int r = int(sg); r < P::TH; r += 4) {
        fmat acc[4][2];
        P::gemm_row(st, w2, r, P::MW, P::LAST_X, acc);
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < 4; i++) {
            device half *o = out + (long(ty0 + r) * P::W + tx0 + min(8 * i, P::LAST_X)) * 16;
            simdgroup_store(bias_relu(acc[i][0], bm[0]), o, 16);
            simdgroup_store(bias_relu(acc[i][1], bm[1]), o + 8, 16);
        }
    }
}

// Layers 11 + 12. Buffer 0 is layer 10's output, 2 and 5 the packed weights of layers 11 and 12, 4 the push block of the
// conversion; layer 12 has no ReLU, then depth-to-space and the sum with the picture. D of an output row goes to
// threadgroup memory as f16 (the rounded D of the last step); a lane then takes one output pixel of the 1920x1080
// picture, converts the picture pixel again (the picture is not stored), and computes per component, in f16 as the game
// does, ((f16(picture) + D[n]) + bias[n]) + -0.0 with n = 2s, 2s + 1, 8 + 2s, 9 + 2s for sub-pixel s = 2 sy + sx.
// The RGB10A2 image is written with the float of that sum.
kernel void ndve_l11_l12(device const half *in [[buffer(0)]], device const half *w1 [[buffer(2)]],
                         constant float *cp [[buffer(4)]], device const half *w2 [[buffer(5)]],
                         texture2d<float, access::read> luma [[texture(0)]],
                         texture2d<float, access::sample> chroma [[texture(1)]],
                         texture2d<float, access::write> dst [[texture(2)]], uint2 tg [[threadgroup_position_in_grid]],
                         uint sg [[simdgroup_index_in_threadgroup]], uint lane [[thread_index_in_simdgroup]],
                         uint tid [[thread_index_in_threadgroup]]) {
    typedef Pair P;
    threadgroup uint4 stq[P::STAGED / 8];
    threadgroup half dd[4][32 * 16];   // a simdgroup's D of one row: [pixel][channel]
    threadgroup half *st = (threadgroup half *)stq;
    const int tx0 = int(tg.x) * P::TW, ty0 = int(tg.y) * P::TH;
    P::stage_buf(st, in, tid, tx0, ty0);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    hmat keep[4][4][2];
    P::mid_compute(st, w1, sg, keep);
    threadgroup_barrier(mem_flags::mem_threadgroup);
    P::mid_store(st, keep, sg, tid, tg);
    device const half *bias = w2 + P::WEIGHTS;
    for (int r = int(sg); r < P::TH; r += 4) {
        fmat acc[4][2];
        P::gemm_row(st, w2, r, P::MW, P::LAST_X, acc);
        _Pragma("clang loop unroll(full)")
        for (int i = 0; i < 4; i++) {
            const int x = min(8 * i, P::LAST_X);
            simdgroup_store(to_half(acc[i][0]), &dd[sg][x * 16], 16);
            simdgroup_store(to_half(acc[i][1]), &dd[sg][x * 16 + 8], 16);
        }
        simdgroup_barrier(mem_flags::mem_threadgroup);
        const int py = ty0 + r;
        for (int k = 0; k < 4; k++) {
            const int idx = k * 32 + int(lane), sub = idx & 3, px = idx >> 2;
            if (px >= P::TW) break;
            const uint2 p = uint2(2 * (tx0 + px) + (sub & 1), 2 * py + (sub >> 1));
            const float3 pic = convert_px(luma, chroma, cp, p);
            const int n0 = 2 * sub, n1 = 8 + 2 * sub;
            threadgroup const half *d = &dd[sg][px * 16];
            const half v0 = (half(pic.x) + d[n0]) + bias[(n0 >> 3) * 64 + (n0 & 7)];
            const half v1 = (half(pic.y) + d[n0 + 1]) + bias[((n0 + 1) >> 3) * 64 + ((n0 + 1) & 7)];
            const half v2 = (half(pic.z) + d[n1]) + bias[(n1 >> 3) * 64 + (n1 & 7)];
            const half v3 = (1.0h + d[n1 + 1]) + bias[((n1 + 1) >> 3) * 64 + ((n1 + 1) & 7)];
            dst.write(float4(float(v0 + (-0.0h)), float(v1 + (-0.0h)), float(v2 + (-0.0h)), float(v3 + (-0.0h))), p);
        }
        simdgroup_barrier(mem_flags::mem_threadgroup);
    }
}
