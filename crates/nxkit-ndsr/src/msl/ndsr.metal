
// ---------------------------------------------------------------------------------------------------------------------
// The vr network program (ndsr-native.md section 7), one kernel per (input scale, variant): 15 instantiations.
//
// A threadgroup is the game's: a tile of B0 x B1 input pixels (ndsr-native.md section 5), cut into NPX x NPY patches of
// 16x16 luma at stride IN. The network turns a patch into a 12x12 block of the grid, the grid gets the unsharp mask,
// and the tile's output pixels resample it. NSG simdgroups take the patches, NB at a time each (every weight tile a
// simdgroup loads serves NB patches, as in the NDVE resampler's rs_block); all 32 * NSG threads share the rest. NSG and
// NB are tuned per instantiation (kernels.rs): the speed is mostly a matter of how many simdgroups a core keeps in
// flight to hide the latency of the window, the cell samples and the barriers.
//
//   1. window   Y' of the tile and its margin as f16, in threadgroup memory
//   2. network  six layers per group of NB patches (canonical layout, section 8.3), then the cells f16(s |s|) with the
//               f16 base sample
//   3. unsharp  in place: a thread owns a block of cells, reads it with its neighbours, one barrier, writes it
//               (the whole threadgroup works, where a column a thread kept at most 96 of 128 threads busy)
//   4. pixels   bilinear in the grid, sqrt, chroma from the colour sample, signed square, encode, dither, write
//
// Weights: the canonical layout of pack.rs, 6 x 1024 halves. As many layers as the 32 KB threadgroup budget allows
// are staged in threadgroup memory (all six but for the largest tiles), the others are read from the device buffer.
// ---------------------------------------------------------------------------------------------------------------------
constant constexpr uint TG_BUDGET = 32768 - 256;

enum NdsrVariant { NDSR_A = 0, NDSR_B = 1, NDSR_C = 2 };

static inline float bits(uint u) { return as_type<float>(u); }
static inline float rcp_f32(float x) { return precise::divide(1.0f, x); }

// One layer of the network on NB patches Z (rows = picture rows, columns = picture columns, 2 x 2 tiles each), then
// Z = relu(Y) (RELU) or Z = Y (the last layer). Two orders of the same sum, which differ in the f16 roundings:
//   ndsr_layer      H_g = f16(W1_g Z), Y = f16(H_1 W2_1 + f16(H_0 W2_0))   the layers 1, 3, 5, and every layer of
//                   agl_ndsr and of nxkit-ndve's rs_block (one block);
//   ndsr_layer_rev  H_g = f16(Z W2_g), Y = f16(W1_1 H_1 + f16(W1_0 H_0))   the layers 0, 2, 4: the vr blob stores these
//                   layers transposed (ndsr-native.md section 8.3), so the vr programs contract with W2 first.
// A weight tile is loaded once and used for all NB patches.
template <bool RELU, uint NB, typename P>
static inline void ndsr_layer(thread hmat (&X)[NB][2][2], P wb) {
    hmat H[NB][2][2][2];   // [patch][g][row tile][column tile]
    _Pragma("clang loop unroll(full)")
    for (int g = 0; g < 2; g++) {
        hmat A[2][2];   // W1_g tile [row tile mi][k tile ki]
        _Pragma("clang loop unroll(full)")
        for (int mi = 0; mi < 2; mi++) {
            _Pragma("clang loop unroll(full)")
            for (int ki = 0; ki < 2; ki++) simdgroup_load(A[mi][ki], wb + 256 * g + 128 * ki + 64 * mi, 8);
        }
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
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
        hmat B0[2], B1[2];   // W2_gj: k 0-7 and k 8-15
        _Pragma("clang loop unroll(full)")
        for (int g = 0; g < 2; g++) {
            simdgroup_load(B0[g], wb + 512 + 256 * g + 128 * j, 8);
            simdgroup_load(B1[g], wb + 512 + 256 * g + 128 * j + 64, 8);
        }
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            _Pragma("clang loop unroll(full)")
            for (int mi = 0; mi < 2; mi++) {
                fmat acc = zero_acc();
                hstep(acc, H[b][0][mi][0], H[b][0][mi][1], B0[0], B1[0]);
                hstep(acc, H[b][1][mi][0], H[b][1][mi][1], B0[1], B1[1]);
                X[b][mi][j] = RELU ? relu_half(acc) : to_half(acc);
            }
        }
    }
}

template <bool RELU, uint NB, typename P>
static inline void ndsr_layer_rev(thread hmat (&X)[NB][2][2], P wb) {
    fmat acc[NB][2][2];   // [patch][row tile][column tile], the chain over the two terms
    _Pragma("clang loop unroll(full)")
    for (uint b = 0; b < NB; b++) {
        _Pragma("clang loop unroll(full)")
        for (int mi = 0; mi < 2; mi++) {
            _Pragma("clang loop unroll(full)")
            for (int nj = 0; nj < 2; nj++) acc[b][mi][nj] = zero_acc();
        }
    }
    _Pragma("clang loop unroll(full)")
    for (int t = 0; t < 2; t++) {
        hmat W2[2][2], W1[2][2];   // W2_t tile [k tile][column tile], W1_t tile [row tile][k tile]
        _Pragma("clang loop unroll(full)")
        for (int a = 0; a < 2; a++) {
            _Pragma("clang loop unroll(full)")
            for (int b = 0; b < 2; b++) {
                simdgroup_load(W2[a][b], wb + 512 + 256 * t + 128 * b + 64 * a, 8);
                simdgroup_load(W1[a][b], wb + 256 * t + 128 * b + 64 * a, 8);
            }
        }
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            hmat Hc[2][2];   // f16(Z W2_t): [row tile][column tile]
            _Pragma("clang loop unroll(full)")
            for (int mi = 0; mi < 2; mi++) {
                _Pragma("clang loop unroll(full)")
                for (int nj = 0; nj < 2; nj++) {
                    fmat a0 = zero_acc();
                    hstep(a0, X[b][mi][0], X[b][mi][1], W2[0][nj], W2[1][nj]);
                    Hc[mi][nj] = to_half(a0);
                }
            }
            _Pragma("clang loop unroll(full)")
            for (int mi = 0; mi < 2; mi++) {
                _Pragma("clang loop unroll(full)")
                for (int nj = 0; nj < 2; nj++) hstep(acc[b][mi][nj], W1[mi][0], W1[mi][1], Hc[0][nj], Hc[1][nj]);
            }
        }
    }
    _Pragma("clang loop unroll(full)")
    for (uint b = 0; b < NB; b++) {
        _Pragma("clang loop unroll(full)")
        for (int mi = 0; mi < 2; mi++) {
            _Pragma("clang loop unroll(full)")
            for (int nj = 0; nj < 2; nj++) X[b][mi][nj] = RELU ? relu_half(acc[b][mi][nj]) : to_half(acc[b][mi][nj]);
        }
    }
}

// Layer L: the even ones in the vr order, five with ReLU, the last without; staged in threadgroup memory (wt) when
// L < NST, else read from the device buffer (wd).
template <uint L, uint NST, uint NB>
static inline void ndsr_step(thread hmat (&X)[NB][2][2], threadgroup const half *wt, device const half *wd) {
    constexpr bool REV = (L % 2) == 0, RELU = L < 5;
    if (L < NST) {
        if (REV) ndsr_layer_rev<RELU>(X, wt + 1024 * L);
        else ndsr_layer<RELU>(X, wt + 1024 * L);
    } else {
        if (REV) ndsr_layer_rev<RELU>(X, wd + 1024 * L);
        else ndsr_layer<RELU>(X, wd + 1024 * L);
    }
}

template <uint NST, uint NB>
static inline void ndsr_network(thread hmat (&X)[NB][2][2], threadgroup const half *wt, device const half *wd) {
    ndsr_step<0, NST>(X, wt, wd);
    ndsr_step<1, NST>(X, wt, wd);
    ndsr_step<2, NST>(X, wt, wd);
    ndsr_step<3, NST>(X, wt, wd);
    ndsr_step<4, NST>(X, wt, wd);
    ndsr_step<5, NST>(X, wt, wd);
}

// The output encoding of the variant A (sRGB, section 7.7).
static inline float encode_srgb(float lin) {
    if (!(lin < bits(0x3b4d2e1cu))) {   // FSETP.GEU: NaN takes the power branch
        // the game's MUFU.LG2 / MUFU.EX2; the precise versions cost 3 to 18 % of a variant A kernel
        const float e = fast::exp2(fast::log2(lin) * bits(0x3ed55555u));
        return fma(e, bits(0x3f870a3du), bits(0xbd6147aeu));
    }
    return lin * bits(0x414eb852u);
}

// The dither of section 7.8 added to the encoded channels.
template <uint VAR>
static inline float3 dither(float3 o, uint x, uint y) {
    const uint h = ((x * 1597334677u) ^ (y * 3812015819u)) * 1597334677u;
    const float d = float(int(h & 0xffffu) - int(h >> 16));
    if (VAR == NDSR_A) {
        const float k = 5.983930861930276e-08f;   // 1 / (255 * 65535)
        return float3(fma(d, k, o.x), fma(d, k, o.y), fma(d, k, o.z));
    }
    if (VAR == NDSR_B) {
        const float k = 1.491595469360618e-08f;   // 1 / (1023 * 65535)
        return float3(fma(d, k, o.x), fma(d, k, o.y), fma(d, k, o.z));
    }
    const float q = d * 1.5259021893143654e-05f;   // 1 / 65535, times one ulp of the 11 / 11 / 10-bit float
    float3 r;
    for (int c = 0; c < 3; c++) {
        const uint sh = c == 2 ? 5u : 6u;
        const float e = bits((as_type<uint>(o[c]) & 0x7f800000u) - (sh << 23));
        r[c] = fma(q, (e == e) ? saturate(e) : 0.0f, o[c]);
    }
    return r;
}

// The texture filter. NDSR_SOFT_SAMPLER (tests only) replaces the hardware's with a bilinear filter in plain f32, so that a
// CPU model can match the output bit for bit; the hardware's sub-texel weight precision is a property of the game's GPU.
static inline float4 ndsr_sample(texture2d<float, access::sample> t, sampler s, float2 uv) {
#ifdef NDSR_SOFT_SAMPLER
    const int2 lim = int2(t.get_width(), t.get_height()) - 1;
    const float2 p = uv * float2(lim + 1) - 0.5f;
    const float2 fl = floor(p), f = p - fl;
    const int2 i0 = clamp(int2(fl), int2(0), lim), i1 = clamp(int2(fl) + 1, int2(0), lim);
    const float4 a = t.read(uint2(i0.x, i0.y)), b = t.read(uint2(i1.x, i0.y));
    const float4 c = t.read(uint2(i0.x, i1.y)), d = t.read(uint2(i1.x, i1.y));
    const float4 top = a * (1.0f - f.x) + b * f.x, bot = c * (1.0f - f.x) + d * f.x;
    return top * (1.0f - f.y) + bot * f.y;
#else
    return t.sample(s, uv);
#endif
}

template <uint IN, uint B0, uint B1, uint VAR, uint R, uint S, uint H, uint H1X, uint H1Y, bool TYODD, uint NSG, uint NB>
[[max_total_threads_per_threadgroup(32 * NSG)]]
kernel void ndsr_main(device const half *wsrc [[buffer(0)]], constant uint *pu [[buffer(1)]],
                      texture2d<float, access::sample> tex0 [[texture(0)]],
                      texture2d<float, access::write> dst [[texture(1)]], uint2 tg [[threadgroup_position_in_grid]],
                      uint tid [[thread_index_in_threadgroup]], uint sg [[simdgroup_index_in_threadgroup]],
                      uint lane [[thread_index_in_simdgroup]]) {
    // BEST GUESS: the hook cannot bind the game's sampler; linear, clamp to edge, normalized.
    constexpr sampler smp(coord::normalized, address::clamp_to_edge, filter::linear);
    constexpr uint NPX = (B0 + 1) / IN, NPY = (B1 + 1) / IN;
    constexpr uint GW = 12 * NPX, GH = 12 * NPY;
    constexpr int M = int((16 - IN) / 2);
    constexpr uint WW = IN * (NPX - 1) + 16, WH = IN * (NPY - 1) + 16;
    constexpr uint NTHREADS = 32 * NSG;
    constexpr uint NGROUPS = (NPX * NPY + NB - 1) / NB, ROUNDS = (NGROUPS + NSG - 1) / NSG;
    constexpr uint BASE = (GW * GH + WW * WH + NSG * 256) * 2;
    // layers staged: as many as fit next to the rest
    constexpr uint NFIT = BASE + 2048 <= TG_BUDGET ? (TG_BUDGET - BASE) / 2048 : 0;
    constexpr uint NST = NFIT > 6 ? 6 : NFIT;
    threadgroup half grid[GW * GH];
    threadgroup half win[WW * WH];
    threadgroup half ys[NSG][256];
    threadgroup half wt[NST > 0 ? 1024 * NST : 8];

    const int win_w = int(pu[2]) + 1, win_h = int(pu[3]) + 1;
    const int out_w = int(pu[6]) + 1, out_h = int(pu[7]) + 1;
    const int tx0 = int(B0 * tg.x), ty0 = int(B1 * tg.y);

    // 1. the window of Y' as f16, coordinates clamped to the picture
    for (uint i = tid; i < 128 * NST; i += NTHREADS) ((threadgroup uint4 *)wt)[i] = ((device const uint4 *)wsrc)[i];
    for (uint i = tid; i < WW * WH; i += NTHREADS) {
        const int x = clamp(tx0 - M + int(i % WW), 0, win_w - 1), y = clamp(ty0 - M + int(i / WW), 0, win_h - 1);
        win[i] = half(tex0.read(uint2(x, y)).y);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 2. the network per group of NB patches, then the grid cells
    const float rw = rcp_f32(float(win_w)), rh = rcp_f32(float(win_h));
    for (uint rd = 0; rd < ROUNDS; rd++) {
        const uint p0 = (rd * NSG + sg) * NB;   // the first patch of the group
        if (p0 >= NPX * NPY) break;
        hmat X[NB][2][2];
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            const uint pi = min(p0 + b, NPX * NPY - 1);   // a short last group repeats a patch, not stored
            const uint q = pi % NPX, p = pi / NPX;
            _Pragma("clang loop unroll(full)")
            for (int a = 0; a < 2; a++) {
                _Pragma("clang loop unroll(full)")
                for (int c = 0; c < 2; c++) simdgroup_load(X[b][a][c], &win[(IN * p + 8 * a) * WW + IN * q + 8 * c], WW);
            }
        }
        ndsr_network<NST>(X, (threadgroup const half *)wt, wsrc);
        // the base samples of the cells of the NB patches, requested together: they do not depend on the network
        half base[NB][5];
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            const uint pq = min(p0 + b, NPX * NPY - 1) % NPX, pp = min(p0 + b, NPX * NPY - 1) / NPX;
            _Pragma("clang loop unroll(full)")
            for (uint k = 0; k < 5; k++) {
                const uint e = lane + 32 * k;
                base[b][k] = 0.0h;
                if (e < 144) {
                    const uint r = e / 12, c = e % 12;
                    const uint i = 12 * pp + r, j = 12 * pq + c;
                    const float cx = (j & 1) == 0 ? fma(float(j), bits(S), bits(H)) : fma(float(j - 1), bits(S), bits(H1X));
                    const float cy = (TYODD && (i & 1) != 0) ? fma(float(i - 1), bits(S), bits(H1Y)) : fma(float(i), bits(S), bits(H));
                    const float u = (cx + float(tx0)) * rw, v = (cy + float(ty0)) * rh;
                    base[b][k] = half(ndsr_sample(tex0, smp, float2(u, v)).y);
                }
            }
        }
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            if (p0 + b < NPX * NPY) {
                _Pragma("clang loop unroll(full)")
                for (int a = 0; a < 2; a++) {
                    _Pragma("clang loop unroll(full)")
                    for (int c = 0; c < 2; c++) simdgroup_store(X[b][a][c], &ys[sg][a * 128 + c * 8], 16);
                }
                simdgroup_barrier(mem_flags::mem_threadgroup);
                const uint pq = (p0 + b) % NPX, pp = (p0 + b) / NPX;
                _Pragma("clang loop unroll(full)")
                for (uint k = 0; k < 5; k++) {
                    const uint e = lane + 32 * k;
                    if (e < 144) {
                        const uint r = e / 12, c = e % 12;
                        const float s = float(base[b][k]) + float(ys[sg][r * 16 + c]);
                        grid[(12 * pp + r) * GW + 12 * pq + c] = half(s * abs(s));
                    }
                }
                simdgroup_barrier(mem_flags::mem_threadgroup);
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 3. the unsharp mask in place: x mirrored, y clamped (section 7.5). A thread owns a block of UCW columns and UR
    // rows, reads it with the ring of neighbours into registers, and after one barrier writes the result.
    const float w18 = as_type<float>(pu[18]);
    const half k1 = half(fma(w18, 0.5f, 1.0f)), k2 = half(w18 * -0.125f);
    {
        constexpr uint UCW = 6, UCG = GW / UCW, UBANDS = NTHREADS / UCG, UR = (GH + UBANDS - 1) / UBANDS;
        static_assert(GW % UCW == 0, "whole column blocks");
        const int c0 = int(tid % UCG) * int(UCW), r0 = int(tid / UCG) * int(UR);
        half v[UR + 2][UCW + 2];
        _Pragma("clang loop unroll(full)")
        for (uint rr = 0; rr < UR + 2; rr++) {
            const int i = clamp(r0 - 1 + int(rr), 0, int(GH) - 1);
            _Pragma("clang loop unroll(full)")
            for (uint cc = 0; cc < UCW + 2; cc++) {
                int j = c0 - 1 + int(cc);
                j = j < 0 ? 1 : (j >= int(GW) ? int(GW) - 2 : j);
                v[rr][cc] = grid[i * GW + j];
            }
        }
        threadgroup_barrier(mem_flags::mem_threadgroup);
        _Pragma("clang loop unroll(full)")
        for (uint rr = 0; rr < UR; rr++) {
            if (r0 + int(rr) < int(GH)) {
                _Pragma("clang loop unroll(full)")
                for (uint cc = 0; cc < UCW; cc++) {
                    const half t1 = v[rr + 1][cc + 1] * k1;
                    const half t2 = fma(v[rr + 1][cc + 2], k2, t1);
                    const half t3 = v[rr][cc + 1] + v[rr + 1][cc];
                    const half t4 = t3 * k2;
                    const half t5 = fma(v[rr + 2][cc + 1], k2, t4);
                    grid[(r0 + rr) * GW + c0 + cc] = t2 + t5;
                }
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 4. the output pixels of the tile (section 7.6)
    const int ax = int(B0), ay = int(B1);
    const int x0 = tg.x == 0 ? 0 : ((2 * ax * int(tg.x) + 1) * out_w + win_w - 1) / (2 * win_w);
    const int x1 = ax * int(tg.x + 1) >= win_w ? out_w : ((2 * ax * int(tg.x + 1) + 1) * out_w + win_w - 1) / (2 * win_w);
    const int y0 = tg.y == 0 ? 0 : ((2 * ay * int(tg.y) + 1) * out_h + win_h - 1) / (2 * win_h);
    const int y1 = ay * int(tg.y + 1) >= win_h ? out_h : ((2 * ay * int(tg.y + 1) + 1) * out_h + win_h - 1) / (2 * win_h);
    const int tw = x1 - x0, th = y1 - y0;
    const float rwo = rcp_f32(float(out_w)), rho = rcp_f32(float(out_h));
    const float sxw = float(win_w) * bits(R), syh = float(win_h) * bits(R);
    const float sx = sxw * rwo, sy = syh * rho;
    const float ox = fma(-(float(tx0) * sxw), rw, fma(sx, 0.5f, -0.5f));
    const float oy = fma(-(float(ty0) * syh), rh, fma(sy, 0.5f, -0.5f));
    const uint2 off = uint2(pu[4], pu[5]);
    const bool dith = pu[19] != 0;
    // pixel n = tid, tid + 128, ... of the tile, row-major; the position steps instead of dividing
    const int dstep = int(NTHREADS) % max(tw, 1), rstep = int(NTHREADS) / max(tw, 1);
    int px = int(tid) % max(tw, 1), py = int(tid) / max(tw, 1);
    for (int n = int(tid); tw > 0 && th > 0 && n < tw * th; n += int(NTHREADS)) {
        const int x = x0 + px, y = y0 + py;
        px += dstep;
        py += rstep;
        if (px >= tw) {
            px -= tw;
            py++;
        }
        const float gx = fma(sx, float(x), ox), gy = fma(sy, float(y), oy);
        const float fix = floor(gx), fiy = floor(gy);
        const float fx = gx - fix, fy = gy - fiy;
        const int ix = int(fix), iy = int(fiy);
        const int xa = clamp(ix, 0, int(GW) - 1), xb = clamp(ix + 1, 0, int(GW) - 1);
        const int ya = clamp(iy, 0, int(GH) - 1), yb = clamp(iy + 1, 0, int(GH) - 1);
        const float a = float(grid[ya * GW + xa]), b = float(grid[ya * GW + xb]);
        const float c = float(grid[yb * GW + xa]), d = float(grid[yb * GW + xb]);
        const float top = fma(fx, b, fma(-fx, a, a)), bot = fma(fx, d, fma(-fx, c, c));
        const float v = fma(fy, bot, fma(-fy, top, top));
        const float gs = precise::sqrt(max(v, 0.0f));
        const float3 t = ndsr_sample(tex0, smp, float2((float(x) + 0.5f) * rwo, (float(y) + 0.5f) * rho)).xyz;
        const float rr = gs + (t.x - t.y), bb = gs + (-t.y + t.z);
        const float gg = fma(gs, bits(0x3fb2f88eu), fma(rr, bits(0xbe983266u), bb * bits(0xbdcebf49u)));
        const float3 cc = float3(rr, gg, bb);
        float3 o = cc * abs(cc);
        if (VAR == NDSR_A) o = float3(encode_srgb(o.x), encode_srgb(o.y), encode_srgb(o.z));
        if (dith) o = dither<VAR>(o, uint(x), uint(y));
        dst.write(float4(o, 1.0f), uint2(uint(x), uint(y)) + off);
    }
}

// One instantiation per vr network program (ndsr-native.md section 4): name, in, b0, b1, variant, R, s, h, h1x, h1y, ty odd,
// simdgroups of a threadgroup, patches a simdgroup takes at once.
#define NDSR_KERNEL(NAME, IN, B0, B1, VAR, R, S, H, H1X, H1Y, TYODD, NSG, NB) \
    template [[host_name(NAME)]] kernel void ndsr_main<IN, B0, B1, VAR, R, S, H, H1X, H1Y, TYODD, NSG, NB>( \
        device const half *, constant uint *, texture2d<float, access::sample>, texture2d<float, access::write>, uint2, \
        uint, uint, uint);

NDSR_KERNEL("ndsr_in10_a", 10, 39, 29, 0, 0x3f99999au, 0x3f555556u, 0x3ed55556u, 0x3fa00000u, 0u, false, 12, 1)
NDSR_KERNEL("ndsr_in10_b", 10, 39, 29, 1, 0x3f99999au, 0x3f555556u, 0x3ed55556u, 0x3fa00000u, 0u, false, 12, 1)
NDSR_KERNEL("ndsr_in10_c", 10, 39, 39, 2, 0x3f99999au, 0x3f555556u, 0x3ed55556u, 0x3fa00000u, 0u, false, 16, 1)
NDSR_KERNEL("ndsr_in8_a", 8, 31, 31, 0, 0x3fc00000u, 0x3f2aaaabu, 0x3eaaaaabu, 0x3f800000u, 0x3f800000u, true, 16, 1)
NDSR_KERNEL("ndsr_in8_b", 8, 31, 31, 1, 0x3fc00000u, 0x3f2aaaabu, 0x3eaaaaabu, 0x3f800000u, 0x3f800000u, true, 16, 1)
NDSR_KERNEL("ndsr_in8_c", 8, 31, 31, 2, 0x3fc00000u, 0x3f2aaaabu, 0x3eaaaaabu, 0x3f800000u, 0x3f800000u, true, 16, 1)
NDSR_KERNEL("ndsr_in7_a", 7, 27, 27, 0, 0x3fdb6db8u, 0x3f155556u, 0x3e955556u, 0x3f600000u, 0x3f600001u, true, 16, 1)
NDSR_KERNEL("ndsr_in7_b", 7, 55, 41, 1, 0x3fdb6db8u, 0x3f155556u, 0x3e955556u, 0x3f600000u, 0x3f600001u, true, 8, 1)
NDSR_KERNEL("ndsr_in7_c", 7, 55, 20, 2, 0x3fdb6db8u, 0x3f155556u, 0x3e955556u, 0x3f600000u, 0x3f600001u, true, 12, 2)
NDSR_KERNEL("ndsr_in6_a", 6, 23, 23, 0, 0x40000000u, 0x3f000000u, 0x3e800000u, 0x3f400000u, 0x3f400000u, true, 16, 1)
NDSR_KERNEL("ndsr_in6_b", 6, 47, 23, 1, 0x40000000u, 0x3f000000u, 0x3e800000u, 0x3f400000u, 0x3f400000u, true, 12, 1)
NDSR_KERNEL("ndsr_in6_c", 6, 47, 23, 2, 0x40000000u, 0x3f000000u, 0x3e800000u, 0x3f400000u, 0x3f400000u, true, 12, 1)
NDSR_KERNEL("ndsr_in4_a", 4, 31, 23, 0, 0x40400000u, 0x3eaaaaabu, 0x3e2aaaabu, 0x3f000000u, 0x3f000000u, true, 8, 3)
NDSR_KERNEL("ndsr_in4_b", 4, 31, 23, 1, 0x40400000u, 0x3eaaaaabu, 0x3e2aaaabu, 0x3f000000u, 0x3f000000u, true, 8, 3)
NDSR_KERNEL("ndsr_in4_c", 4, 31, 23, 2, 0x40400000u, 0x3eaaaaabu, 0x3e2aaaabu, 0x3f000000u, 0x3f000000u, true, 8, 3)
