// ---------------------------------------------------------------------------------------------------------------------
// The vr_light network program (ndsr-native.md section 15), one kernel per program: 15 instantiations.
//
// The threadgroup is the game's: B0 x B1 input pixels, NPX x NPY patches of 16x16 luma at stride IN. The network turns
// a patch into an OUT x OUT block of the grid, and the grid is the output tile itself (GW x GH pixels, push words 18,
// 19): no resample to an arbitrary size, everything after the network in f16 as the game does it.
//
//   1. window   the f16 luma of the sqrt-RGB texels (F32IN: the f32 luma, then f16), in threadgroup memory
//   2. network  NL layers (6, or 3 for the 4/3 model) per group of NB patches, the vr order (ndsr.metal), the result
//               N = Y[0..OUT)[0..OUT) is the grid
//   3. pixels   a thread owns PPT pixels of the tile: the grid at (fma(x, w20, w22), fma(y, w21, w23)) by f16 lerps,
//               plus the f16 colour sample, signed square, luma L into threadgroup memory (over the dead window)
//   4. sharpen  d = w28/2 L - w28/8 (4 neighbours, clamped to the tile), out = lin + d, sRGB for SRGB, write
// ---------------------------------------------------------------------------------------------------------------------

static inline half hbits(ushort u) { return as_type<half>(u); }

// The f16 luma of the input (B first) and of the result (G first): the game's orders, ndsr-native.md section 15.3.
static inline half light_luma_in(float4 t) {
    const half3 c = half3(t.xyz);
    return fma(c.y, hbits(0x39b8), fma(c.x, hbits(0x32cd), c.z * hbits(0x2c9e)));
}
static inline half light_luma_in_f32(float4 t) {
    return half(fma(t.z, bits(0x3d93dd98u), fma(t.x, bits(0x3e59b3d0u), t.y * bits(0x3f371759u))));
}
static inline half light_luma_out(half3 l) {
    return fma(l.z, hbits(0x2c9e), fma(l.x, hbits(0x32cd), l.y * hbits(0x39b8)));
}
static inline half light_lerp(half a, half b, half f) { return fma(f, b, fma(-f, a, a)); }

// The cell and the f16 fraction of one axis: clamped so that the right neighbour stays in the grid (fraction 1 then),
// and below the grid to the first cell.
static inline void light_axis(float t, int n, thread int &i, thread half &f) {
    const float fl = floor(t);
    if (fl > float(n - 2)) {
        i = n - 2;
        f = 1.0h;
    } else if (fl < 0.0f) {
        i = 0;
        f = 0.0h;
    } else {
        i = int(fl);
        f = half(t - fl);
    }
}

template <uint NL, uint NST, uint NB>
static inline void light_network(thread hmat (&X)[NB][2][2], threadgroup const half *wt, device const half *wd) {
    ndsr_step<0, NST, NB, NL>(X, wt, wd);
    ndsr_step<1, NST, NB, NL>(X, wt, wd);
    ndsr_step<2, NST, NB, NL>(X, wt, wd);
    if (NL > 3) {
        ndsr_step<3, NST, NB, NL>(X, wt, wd);
        ndsr_step<4, NST, NB, NL>(X, wt, wd);
        ndsr_step<5, NST, NB, NL>(X, wt, wd);
    }
}

template <uint IN, uint OUT, uint B0, uint B1, uint NL, bool F32IN, bool SRGB, uint NSG, uint NB>
[[max_total_threads_per_threadgroup(32 * NSG)]]
kernel void ndsr_light(device const half *wsrc [[buffer(0)]], constant uint *pu [[buffer(1)]],
                       texture2d<float, access::sample> tex0 [[texture(0)]],
                       texture2d<float, access::write> dst [[texture(1)]], uint2 tg [[threadgroup_position_in_grid]],
                       uint tid [[thread_index_in_threadgroup]], uint sg [[simdgroup_index_in_threadgroup]],
                       uint lane [[thread_index_in_simdgroup]]) {
    constexpr sampler smp(coord::normalized, address::clamp_to_edge, filter::linear);
    constexpr uint NPX = (B0 + 1) / IN, NPY = (B1 + 1) / IN;
    constexpr uint GW = OUT * NPX, GH = OUT * NPY, NPIX = GW * GH;
    constexpr int M = int((16 - IN) / 2);
    constexpr uint WW = IN * (NPX - 1) + 16, WH = IN * (NPY - 1) + 16;
    constexpr uint SCR = WW * WH > NPIX ? WW * WH : NPIX;
    constexpr uint NTHREADS = 32 * NSG, PPT = (NPIX + NTHREADS - 1) / NTHREADS;
    constexpr uint NGROUPS = (NPX * NPY + NB - 1) / NB, ROUNDS = (NGROUPS + NSG - 1) / NSG;
    constexpr bool DIRECT = OUT == 8;   // the 8x8 result tile is the block of the grid: no scratch
    constexpr uint YS = DIRECT ? 1 : 256;
    constexpr uint BASE = (GW * GH + SCR + NSG * YS) * 2;
    constexpr uint NFIT = BASE + 2048 <= TG_BUDGET ? (TG_BUDGET - BASE) / 2048 : 0;
    constexpr uint NST = NFIT > NL ? NL : NFIT;
    threadgroup half grid[GW * GH];
    threadgroup half scr[SCR];   // the window, then the luma of the tile
    threadgroup half ys[NSG][YS];
    threadgroup half wt[NST > 0 ? 1024 * NST : 8];

    const int tx0 = int(B0 * tg.x), ty0 = int(B1 * tg.y);
    const int2 lim = int2(tex0.get_width(), tex0.get_height()) - 1;

    // 1. the window, texel coordinates clamped to the picture
    for (uint i = tid; i < 128 * NST; i += NTHREADS) ((threadgroup uint4 *)wt)[i] = ((device const uint4 *)wsrc)[i];
    for (uint i = tid; i < WW * WH; i += NTHREADS) {
        const int x = clamp(tx0 - M + int(i % WW), 0, lim.x), y = clamp(ty0 - M + int(i / WW), 0, lim.y);
        const float4 t = tex0.read(uint2(x, y));
        scr[i] = F32IN ? light_luma_in_f32(t) : light_luma_in(t);
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 2. the network per group of NB patches; its result is the grid
    for (uint rd = 0; rd < ROUNDS; rd++) {
        const uint p0 = (rd * NSG + sg) * NB;
        if (p0 >= NPX * NPY) break;
        hmat X[NB][2][2];
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            const uint pi = min(p0 + b, NPX * NPY - 1);
            const uint q = pi % NPX, p = pi / NPX;
            _Pragma("clang loop unroll(full)")
            for (int a = 0; a < 2; a++) {
                _Pragma("clang loop unroll(full)")
                for (int c = 0; c < 2; c++) simdgroup_load(X[b][a][c], &scr[(IN * p + 8 * a) * WW + IN * q + 8 * c], WW);
            }
        }
        light_network<NL, NST>(X, (threadgroup const half *)wt, wsrc);
        _Pragma("clang loop unroll(full)")
        for (uint b = 0; b < NB; b++) {
            if (p0 + b < NPX * NPY) {
                const uint q = (p0 + b) % NPX, p = (p0 + b) / NPX;
                if (DIRECT) {
                    simdgroup_store(X[b][0][0], &grid[(OUT * p) * GW + OUT * q], GW);
                } else {
                    _Pragma("clang loop unroll(full)")
                    for (int a = 0; a < 2; a++) {
                        _Pragma("clang loop unroll(full)")
                        for (int c = 0; c < 2; c++) simdgroup_store(X[b][a][c], &ys[sg][a * 128 + c * 8], 16);
                    }
                    simdgroup_barrier(mem_flags::mem_threadgroup);
                    for (uint e = lane; e < OUT * OUT; e += 32) {
                        const uint r = e / OUT, c = e % OUT;
                        grid[(OUT * p + r) * GW + OUT * q + c] = ys[sg][r * 16 + c];
                    }
                    simdgroup_barrier(mem_flags::mem_threadgroup);
                }
            }
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 3. the linear colour of the thread's pixels, kept in registers; their luma to threadgroup memory
    const float sx = as_type<float>(pu[20]), sy = as_type<float>(pu[21]);
    const float gx0 = as_type<float>(pu[22]), gy0 = as_type<float>(pu[23]);
    const float rwo = as_type<float>(pu[26]), rho = as_type<float>(pu[27]);
    const int ox = int(GW * tg.x), oy = int(GH * tg.y);
    half3 lin[PPT];
    _Pragma("clang loop unroll(full)")
    for (uint k = 0; k < PPT; k++) {
        const uint n = tid + k * NTHREADS;
        if (n < NPIX) {
            const int xl = int(n % GW), yl = int(n / GW);
            int ix, iy;
            half fx, fy;
            light_axis(fma(float(xl), sx, gx0), int(GW), ix, fx);
            light_axis(fma(float(yl), sy, gy0), int(GH), iy, fy);
            const half top = light_lerp(grid[iy * GW + ix], grid[iy * GW + ix + 1], fx);
            const half bot = light_lerp(grid[(iy + 1) * GW + ix], grid[(iy + 1) * GW + ix + 1], fx);
            const half up = light_lerp(top, bot, fy);
            const float2 uv = float2(fma(float(ox + xl), rwo, 0.5f * rwo), fma(float(oy + yl), rho, 0.5f * rho));
            const half3 t = half3(ndsr_sample(tex0, smp, uv).xyz) + up;
            lin[k] = t * abs(t);
            scr[n] = light_luma_out(lin[k]);
        }
    }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // 4. the sharpening and the write
    const float w28 = as_type<float>(pu[28]);
    const half k1 = half(w28 * 0.5f), k2 = half(w28 * -0.125f);
    const int nx = min(int(GW), int(pu[24]) - ox), ny = min(int(GH), int(pu[25]) - oy);
    const uint2 off = uint2(pu[4], pu[5]);
    _Pragma("clang loop unroll(full)")
    for (uint k = 0; k < PPT; k++) {
        const uint n = tid + k * NTHREADS;
        const int xl = int(n % GW), yl = int(n / GW);
        if (n < NPIX && xl < nx && yl < ny) {
            const int xm = max(xl - 1, 0), xp = min(xl + 1, int(GW) - 1);
            const int ym = max(yl - 1, 0), yp = min(yl + 1, int(GH) - 1);
            const half left = fma(scr[yl * GW + xm], k2, scr[yl * GW + xp] * k2);
            const half centre = fma(scr[yl * GW + xl], k1, scr[yp * GW + xl] * k2);
            const half d = fma(scr[ym * GW + xl], k2, left + centre);
            float3 o = float3(lin[k] + d) + 0.0f;
            if (SRGB) o = float3(encode_srgb(o.x), encode_srgb(o.y), encode_srgb(o.z));
            dst.write(float4(o, 1.0f), uint2(uint(ox + xl), uint(oy + yl)) + off);
        }
    }
}

// One instantiation per vr_light network program (ndsr-native.md section 15): name, in, out, b0, b1, layers, f32
// luma, sRGB, simdgroups of a threadgroup, patches a simdgroup takes at once.
#define NDSR_LIGHT(NAME, IN, OUT, B0, B1, NL, F32IN, SRGB, NSG, NB) \
    template [[host_name(NAME)]] kernel void ndsr_light<IN, OUT, B0, B1, NL, F32IN, SRGB, NSG, NB>( \
        device const half *, constant uint *, texture2d<float, access::sample>, texture2d<float, access::write>, uint2, \
        uint, uint, uint);

NDSR_LIGHT("ndsr_light_r12_a", 10, 12, 40, 40, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r12_b", 10, 12, 40, 40, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r43_a", 6, 8, 48, 48, 3, true, true, 16, 1)
NDSR_LIGHT("ndsr_light_r43_b", 6, 8, 48, 24, 3, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r15_a", 8, 12, 32, 32, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r15_b", 8, 12, 32, 32, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r127_a", 7, 12, 28, 28, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r127_b", 7, 12, 28, 28, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r2_a", 6, 12, 24, 24, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r2_b", 6, 12, 24, 24, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r24_a", 5, 12, 20, 20, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r24_b", 5, 12, 20, 20, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r3_a", 4, 12, 16, 16, 6, false, true, 16, 1)
NDSR_LIGHT("ndsr_light_r3_b", 4, 12, 16, 16, 6, false, false, 16, 1)
NDSR_LIGHT("ndsr_light_r3_c", 4, 12, 16, 16, 6, true, false, 16, 1)
