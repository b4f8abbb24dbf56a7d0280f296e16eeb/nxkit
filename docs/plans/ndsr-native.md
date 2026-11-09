# agl NDSR upscaler: what it is

Game: Zelda Ocarina of Time (0400486024DEC000, v131072). Engine family agl. The upscaler is Nintendo's NDSR ("neural
super resolution"). It is the same library whose pack sits inside the `nn::ndve` movie file
([ndve-native.md](ndve-native.md)): the NDVE resampler `4f41ccf24f163e0d` is an NDSR network.

Claim marks, as in ndve-native.md:

- **V** = verified from file bytes, the translated MSL or the SASS.
- **I** = inferred, not re-derived from the code.
- **R** = only answerable at runtime (section 9).

Scratch tools (not in the repo): `/private/tmp/claude-501/-Users-nzaljic-repos-me-nxkit-rt/2bd3ad49-183b-493a-8d88-aa4b15fad0cc/scratchpad/ndsr/`
(`sarc.py` extracts the packs, `pck.py` lists a pack, `ndsrlib.py` maps pack entries to shader keys, `table.py`
prints the per-shader table of section 4, `lstn.py <key>` prints the SASS listing through the NDVE disassembler,
`common.py` is a shim that lets the NDVE emulator `ndve/emu.py` load NDSR shaders). The emulator does not implement
`TLD4` (0xb63) and `MUFU` yet, so no NDSR shader has been emulated (section 13).

## 1. Summary

- Three NDSR packs ship in `Lib/agl/agl_resource.Nin_Ounce_NVN2.release.sarc.zs`: `agl_ndsr.pck`, `agl_ndsr_vr.pck`,
  `agl_ndsr_vr_light.pck` (V). Together they hold 67 distinct programs in the shader map: 3 vertex + 5 pixel programs
  and 59 compute programs (22 + 20 + 17), 13,570 HMMA instructions in all (V, matches the task's 13,570).
- Each pack is **anti-aliasing + a single-frame neural upscaler**. The anti-aliasing is SMAA 1x (edge detection, blending
  weights, neighbourhood blending; the pack carries SMAA's area and search textures). The network is a small per-block
  MLP (6 blocks of 0x800 bytes of f16 weights, the same block size as the NDVE resampler) that upsamples by a **fixed
  ratio** baked into each network (1.2, 1.25, 4/3, 1.375, 1.5, 5/3, 12/7, 11/6, 2, 2.2, 2.4 or 3).
- **Not temporal** (V from the bindings of all 59 compute and 5 pixel programs): no depth, no motion vectors, no
  history, no exposure, no jitter. One colour texture in, one image out.
- The three packs are three implementations of the same models: `agl_ndsr.pck` is the graphics-pipeline SMAA plus
  LDSM/MOVM networks at fixed ratio; `agl_ndsr_vr.pck` is compute SMAA plus compute networks that resample to an
  **arbitrary** output size ("vr" = variable ratio, I) from five fixed-ratio models; `agl_ndsr_vr_light.pck` is the same
  with cheaper networks (about 190 HMMA per program instead of up to 576). The vr weight blobs are the `agl_ndsr.pck`
  blobs of the same ratio with the f16 values reordered (V: equal multisets, see section 5).
- Which pack the game uses in gameplay is **R**. The repo's notes name `agl_ndsr_vr` (docs/analysis/oot-open-issues.md);
  dynamic resolution (763x429 .. 1536x864) fits the vr pack, which takes any input/output ratio (I).
- Cost (section 11): about 1.4-1.6 GMAC per frame for the network at any ratio (I), well under the movie network's
  10.6 GMAC; on an M2 Max about 0.6-0.8 ms plus 0.2-0.5 ms for the SMAA passes.

## 2. Where it lives (V)

- File: `romfs/Lib/agl/agl_resource.Nin_Ounce_NVN2.release.sarc.zs`, zstd frame with dictionary id 1 (member 0 of
  `romfs/Pack/ZsDic.pack.zs`, as for the NDVE file). Decompressed: SARC, 23,273,472 bytes, SHA-256
  `eafcfdcc...14de5`.
- Members (SARC data offset + member start):

| Member | SARC offset | Bytes | SHA-256 |
|---|---|---|---|
| `agl_ndsr.pck` | 0xccc000 | 0xf8000 | `47f6f511247fdee2ed654b591cf2e32f484dfb13fcf882977dcfcd4f8d1ea9f4` |
| `agl_ndsr_vr.pck` | 0xc33000 | 0x99000 | `191ea1989c1b6d5b85de4d1a549c2c63ec3fa87d0a39435d4f0c401e3ac3d029` |
| `agl_ndsr_vr_light.pck` | 0x15ac000 | 0x86000 | `7780c292095d92aef17a8e03434bdcbb8459469f635988e025e250429ffbdc18` |

- The shader map (`work/shader/shader_map.tsv`) lists them as `.../agl_ndsr*.pck#NDSR@<ctl offset>`.
- Repo facts that this corrects:
  - `nxb-build` `upscaler_keys` (crates/nxb-build/src/host/header.rs) collects only `/agl_ndsr.pck#` compute keys, i.e.
    the 22 fixed-ratio networks, not the vr or vr_light programs (V). Nothing at runtime reads `upscaler_keys` today
    (V, grep), so this is harmless now but wrong if the vr pack is the gameplay one.
  - The agl family test uses the archive name `Shader/agl_ndsr_vr.bnsh`; the real names are the three `.pck` members
    above (V).
  - The hint keys `e40f5efb5480e692` and `6bd913e1f34e83eb` do not appear in this build's shader map, shader cache or
    logs (V). They come from another key scheme or another build.

## 3. The pack format (V)

Header: `NDSR`, u32 version (0x00010100 here, 0x00040100 in the NDVE file), u32 (0x00020302; NDVE 0x00080300), u32
entry count n (0x9c / 0x4e / 0x66), u32 at +0x10 (0x00020c00 / 0x06020c00 / 0x05020c00; NDVE 0x01020c00, byte +0x13
unknown). Entries start at **+0x14** in this SDK: n x 8 bytes `(u32 offset, u32 length << 4 | type)`, offsets from the
pack start. (The splitter's "+0x10 or +0x14" rule reads 16-byte records; both views agree here.)

| Type | Content |
|---|---|
| 0 / 1 | vertex program control (0xe0) / code |
| 3 / 4 | pixel program control / code |
| 6 / 7 | compute program control (0xe0) / code |
| 9 | 4 bytes before a non-network compute program: u16 tile width, u16 tile height in pixels (0,0 = indirect, I) |
| 10 | 16 bytes `0, 0, 1, 1` (u32): an indirect-dispatch template (I) |
| 11 | 12-byte texture descriptor: u16 width, u16 height, then format bytes (`0x0d` = RG8, `0x01` = R8, `0x3d` = RGBA8; I from sizes) |
| 12 | texture data of the preceding descriptor (block-linear, I) |
| 13 | network weights, f16 |
| 14 | 4-byte network config before each network program (section 5) |

Contents:

- `agl_ndsr.pck`: texture 160x560 RG8 (0x32000 bytes incl. padding, SHA `0bd5a1f2...`) = SMAA AreaTex; texture 64x16 R8
  (0x400, SHA `35f3f317...`, bytes 0x00/0x7f/0xfe) = SMAA SearchTex (I, sizes and values match SMAA); 5 vs/ps pairs
  (SMAA graphics passes, section 6); 33 network entries = 11 weight blobs x 3 program variants, 22 distinct programs.
- `agl_ndsr_vr.pck`: texture 192x128 RG8 (0xc000, SHA `80bcd38b...`) = the compute SMAA's area texture (I); 5 compute
  pre-passes (SMAA, section 6); the 16-byte template; 15 network entries = 5 weight blobs x 3 variants, 15 distinct
  programs.
- `agl_ndsr_vr_light.pck`: the same texture and template (byte-identical to vr, V); 5 compute pre-passes (the first three
  byte-identical to vr's, so the shader map lists them once); 21 network entries = 7 weight blobs x 3 variants, 15
  distinct programs (two control sections are shared between two code areas: `0x84d70` and `0x84f30`).

Shader-map count: 8 graphics rows (vertex 4ba19662, 62f35869, db200a1b, the last used by three pairs; 5 pixel) +
22 + 20 + 17 compute rows = 67 (V). The vr_light pack's first three pre-passes are vr's programs and get no row of
their own.

## 4. Programs and roles

Per compute program (V from the control sections and the SASS: local size, shared bytes, push words, HMMA count, SASS op
counts; bindings from the `_b.json`). Full table: `scratchpad/ndsr/table.txt`.

### vr pack (the likely gameplay path)

| # | Key | Local | Role | Reads | Writes | Grid |
|---|---|---|---|---|---|---|
| 1 | f8965bffbdf51d78 | 192 | SMAA edge detection, edges compacted into a list (atomics on ssbo 0/1) | colour tex3 (TLD) | edge image img0, ssbo 8/9 (list, counter) | ceil(W/32) x ceil(H/24) (cfg 0x20,0x18; I) |
| 2 | d17eefaff4f0bc23 | 96 | SMAA blending weights for the listed edge pixels | ssbo 8 (list), ubo_ptr 10, edge tex4 (TLD4 gathers), area tex6 | img1 r32ui (packed RGBA8 weights) | indirect (cfg 0,0; I) |
| 3 | ddc1143b7932a93e / 92918e9b7e2f89a4 / 3426ccfe7d691d0f | 128 | SMAA neighbourhood blending, 3 output variants (R which) | weights tex5, colour tex3 (bilinear sampler) | img2 | ceil(W/16) x ceil(H/16) (cfg 0x10,0x10; V from MSL: 16x16 px, 2 px per thread) |
| 4 | one of 15 networks | 128 | NDSR upscale | tex0 (+ sampler), weights ssbo 2 (0x3000) | img1 at the output size | ceil(W/step_x) x ceil(H/step_y) (I) |

Neighbourhood blending (V from MSL of ddc1143b): per pixel read the weights texel `a`; if `max(max(a.x,a.z),
max(a.y,a.w)) < 1e-5` copy the colour; else pick horizontal or vertical by `max(a.x,a.z) > max(a.y,a.w)`, take two
bilinear colour samples at `uv ± w * texel` clamped to the push rectangle (bank 24 row 3), blend with weights
`w1/(w1+w2)`, `w2/(w1+w2)` (f32 fma order as in the MSL). Output `float4(rgb + bits(push row 4), alpha)` where the
push row 4 words are **added to the float bits** (an exponent bias, i.e. a power-of-two scale; R for the values).
Edge threshold 0.1 rounded to f16 (`0.099975586`, V in f8965bff).

Network programs (V: keys, config, HMMA; ratio V where the MSL has the constant, otherwise I from the config byte):

| Weights (pack offset) | Ratio | Variant A (sRGB out) | Variant B | Variant C | HMMA A/B/C |
|---|---|---|---|---|---|
| 0x88710 | 1.2 | 14c10aa70b81f04e (271d) | f407ab4429b8eae6 (271d) | caf3a6e1b2f70605 (2727) | 144/144/192 |
| 0x8b710 | 1.5 | 49e67caae643e42a (1f1f) | 3bed6a425f72e4d6 (1f1f) | 7a5a51af95b27ae5 (1f1f) | 186 each |
| 0x8e710 | 12/7 | c16ce0c879c981a3 (1b1b) | e30678db92cf66ff (3729) | 4aa7eec58d2fa656 (3714) | 192/576/288 |
| 0x91710 | 2.0 | 45f7364945aff920 (1717) | 17c6bd35f159a957 (2f17) | 57cdcd5913bd385f (2f17) | 192/384/384 |
| 0x94710 | 3.0 | e4b49569547e3d47 (1f17) | f0c1f33f84b619dc (1f17) | aec4dfce7ab3cefa (1f17) | 560 each |

Variant A ends in a manual linear-to-sRGB encode (`12.92`, `1.055`, `-0.055`, exponent `0.41666666`) and a hash dither
(V, section 7). B and C have no sRGB encode; they differ in tiling (config bytes 0-1). Which variant runs is R; my guess
is A when the output image is UNORM and B/C when it is an sRGB or float format (I).

### vr_light pack

Pre-passes: the three vr programs 1, 2, 3a, plus `1fdc85178b60f6ce` and `a335c1db6270c355` (neighbourhood blending
variants, 0x800 and 0xd00 code bytes). Networks (config, HMMA): 9327ea56aafa15b9 / 8df148d58b7cbe8e (2828, 1.2, 192);
927ceaf3cf1be240 (3030, 4/3, **256 threads**, 176) / fa3033d05c0a41d6 (3018, 4/3, 176); 48d00da84b3484df /
aad7ea7e5c02f41d (2020, 1.5, 186); 6160882513197af3 / b106afba1a18faa9 (1c1c, 12/7, 192); 2f9bca45ef842a3b /
04135bb812a0d66e (1818, 2.0, 192); d8fffc05afaaa981 / 7c511cc23ff85d14 (1414, 2.4, 192); bdb298c6aa71a298 /
c8b20adc9d4978e0 / d99fbeeb53d38847 (1010, 3.0, 188). The 4/3 networks use a 0x1800-byte blob (3 blocks). The light
networks compute luma in f16 (`0.71484375` = f16(0.7152), `0.072143555` = f16(0.0722)) where vr gathers one channel.
Key assignment for the two shared control sections follows entry order (I).

### agl_ndsr.pck (fixed ratio, graphics SMAA)

Graphics programs (V from MSL constants): `0b35a945d761a4c7` SMAA edge detection (threshold 0.1, local contrast 2.0);
`e9e73df1d5aad999` SMAA blending weights (search offsets -3.75 / 3.25, 0.9 rounding); `b9a97ba035dd8a72`,
`4c59eab22474ab59`, `401877446d25d079` neighbourhood blending (the last two with sRGB encode); vertex programs
`4ba1966203a4f77b`, `62f358694640f6de`, `db200a1b53fd6fef`.

Networks, 128 threads each, all LDSM.x4 / LDSM.x4.T / MOVM like the NDVE resampler, luma `0.2126/0.7152/0.0722` in f32
(V: `* 0.7152f`). Per weight blob: variant A, then variant B twice (B adds an sRGB-to-linear decode of the input:
`0.04045`, `1/12.92`, `0.055/1.055`, `2.4`; V from constants). Configs and HMMA (A/B): d5f00 3228b901 1.2
abef20ad0d9a9b12 240 / 725bcf5c3f4bb3f3 288; d8f00 1.25 e9213c7fba8ccf93 192 / 87bdc29ac7ce9adb 282; dbf00 (0x1800) 4/3
aa1243f635baa8e8 88 / 54c6b94ddf539d9f 220; dd700 1.375 1de2a54f4b81e4e0 288 / 18f53257e4a46245 282; e0700 1.5
3fe6b091bb8c40ce 372 / e16733ed9a14fc7a 372; e3700 5/3 fdeb898427db8562 / a326e7353ec81e2c 288; e6700 12/7
4acef5afefba098b / c78798281d3519c1 144; e9700 11/6 37121447f861a7d3 / add52a00627e7e84 288; ec700 2.0
fb5038f1befcd03b / fbe679b956a85e1f 384; ef700 2.2 196dfe02185a7518 144 / 0253bd17deceee79 288; f2700 3.0
028bd0b187f7342c / f54f031dbc6f65f5 374. Bindings: bank 24 (18 push words), weights ssbo 2, colour tex0 with
sampler, image img1.

## 5. Network config and weights

**Config (type 14), 4 bytes `b0 b1 b2 b3`.**

- `b0`, `b1` = input pixels per threadgroup step in x and y (V: the MSL of 14c10aa7 has `wg.x * 39`, `wg.y * 29` for
  config `271d`; e30678db `* 55`, `* 41` for `3729`; e4b49569 `* 31`, `* 23` for `1f17`; abef20ad `* 50` for `3228`).
  The threadgroup of 14c10aa7 reads a 46 x 36 input window from `(39 wx - 3, 29 wy - 3)` with 4 gathers per thread
  (rows of 23 gathers, 18 rows; V from MSL).
- `b2` = `(out - 1) << 4 | (in - 1)`: the network maps an `in x in` input block to an `out x out` output block, ratio
  `out / in` (I, but it reproduces every ratio constant found in the MSL: 0xb9 -> 12/10 = 1.2 (`* 1.2f` in 14c1),
  0xb7 -> 1.5, 0xb6 -> 12/7 (`1.7142859`), 0xb5 -> 2.0, 0xb3 -> 3.0; and the NDVE resampler 0x75 -> 8/6 = 4/3, which
  ndve-native.md measured).
- `b3`: 0x01 (agl_ndsr), 0x22 (vr), 0x12 (vr_light), 0x21 (NDVE). A variant or version field; meaning unknown.

**Weights (type 13).** 0x3000 bytes = 6 blocks of 0x800 bytes (1024 f16), or 0x1800 = 3 blocks for the 4/3 models
of agl_ndsr / vr_light. All values finite, range about -4 to +4 (V). NDVE's resampler 4f41 uses the same 0x800-byte
block (two 16x16 matrices + four 16x8 tiles, ndve-native.md section 8; V there).

- The vr blobs are the agl_ndsr blobs of the same ratio with the f16 values permuted (V: equal multisets for 1.2, 1.5,
  12/7, 2.0, 3.0; the permutation keeps each 0x800-byte block in place, V for the 1.2 blob, 4236 of 6144 values unique
  and all of them stay in their block). The vr_light blobs equal vr's for 1.2, 1.5, 12/7, 2.0, 3.0 (byte-identical,
  V); its 4/3 blob is agl_ndsr's 4/3 permuted (V); its 2.4 blob is new.
- So there are 12 trained models (ratios 1.2, 1.25, 4/3, 1.375, 1.5, 5/3, 12/7, 11/6, 2, 2.2, 2.4, 3) and the pack
  formats only re-lay them out per kernel (I). NDVE's 4/3 blob differs from agl's 4/3 model (V, other values; it is
  also 6 blocks, agl's 4/3 is 3).
- SHA-256 per blob (pack offset): see `scratchpad/ndsr/` output of `pck.py`; the fingerprint uses the five vr blobs:
  1.2 `b82b7ec1326f05d6d0ce143b815de0779849ebbb4d40f77c0fba7821979c39b0`, 1.5
  `94adcd47f0648e3c7e53cb88bfdd4366426145188a2301fa8acf9b68a05016fe`, 12/7
  `f88305d44dfb27d6e8a64932c3188f62972bb4375e029f65020ecdbaee83d3b6`, 2.0
  `e960abf906280cd5a3942987f741a8b5bb30dc26da9bf2c22fc029f2cdaf93fe`, 3.0
  `5de06ec356888082e6f04e266767894b30dce661872146c18c28efde6fbda30a`; vr_light adds 4/3
  `45209a676f28e76a8709f9c4d72d8ecb003cd11fb0a893c4e57c26276eb091b4` and 2.4
  `8678cc5717d22839dbcd5f9c094ebe52137a83432a7ed888ebb5eeff47be9395`.

## 6. Inputs and outputs (V bindings, I formats)

- In: one colour texture at the render resolution (W x H, dynamic: about 763x429 .. 1536x864 per the task, R). It
  feeds the SMAA edge detection and, after neighbourhood blending, the network. Format R (likely RGBA8 or RGB10A2 in a
  gamma or sqrt-like domain, see section 7).
- Intermediates (vr): edge image (img0 of pass 1), edge list and counter (ssbo 8, 9), weights image r32ui (pass 2),
  anti-aliased colour (img2 of pass 3). All R for format and size, expected W x H.
- Out: one image at the output size (img1 of the network). Expected 1920x1080 docked (I, the game's output; NDVE
  writes 2560x1440, so R).
- No depth, motion, history, exposure or jitter input in any program (V).

## 7. The network program, vr variant A, what is known (V from MSL of 14c10aa70b81f04e unless marked)

1. Load: 128 threads gather a 46 x 36 window of **one channel** (`gather(..., component::y)`, i.e. G) of tex0,
   converted to f16, into threadgroup memory (`sm0`, 48-wide rows). Coordinates are `(x + 1) * push[2].x`,
   `(y + 1) * push[2].y` (texel-centre gathers, push words 8, 9 = 1/W, 1/H, I).
2. Weights: 0x3000 bytes from ssbo 2 into threadgroup memory (`LDG` x 24 per lane, `STS`, `BAR`), V from SASS.
3. MLP: 144 `HMMA.16816.F16` per warp program (no LDSM, no MOVM in the vr programs; A and B come from `LDS`), chains of
   2 (stage 1 -> stage 2, D of stage 1 is A of stage 2 in registers, as in NDVE's resampler) with `HADD2`/`HMUL2`/
   `HFMA2` between. The vr programs have no `HMNMX2` (V), so a ReLU between blocks, if there is one, is the `.RELU`
   flag of `HFMA2`/`HADD2` (I; the NDVE resampler used `HMNMX2`).
4. Resample: the network output is a grid at `ratio x` the input; the output pixel position maps to that grid by
   `p * (1.2 * in_size / out_size) + (0.5 * 1.2 * in/out - 0.5 - ...)` (V: the factor `1.2` times `float(1 + push[0].w)`
   over `float(1 + push[1].w)`, `fma(.., 0.5, -0.5)`), then a **bilinear interpolation of the f16 grid** in threadgroup
   memory (4 taps, `floor`, fractional weights in f32 fma form, clamps `min(., 35)`, `min(., 47)`).
5. Colour: `g = sqrt(max(0, bilinear(grid)))`; then `c' = g + (s.c - s.g)` for c = R, B, where `s` is a bilinear sample
   of tex0 at the output position (`sample(...).x - .y`, V for R); then `c = c' * |c'|` (signed square back to linear);
   then sRGB encode `c <= 0.0031308 ? 12.92 c : 1.055 c^0.41666666 - 0.055`; then dither
   `c + hash(x, y) * 5.983931e-8` where `hash` is an integer hash with multipliers `1597334677` and `-482951477` and
   the top 16 bits are used (V for the constants and the shape; the exact bit operations are in MSL lines
   15059-15077). Alpha = 1.0 except at the last block (`float4(r, g, b, a)` in one of the four writes; R why).
   Interpretation (I): the network super-resolves G in a square-root domain (the input is treated as `sqrt` of linear
   light), chroma is carried as the bilinear `R - G`, `B - G` difference in the same domain.
6. Write: 4 image writes per thread loop, coordinates `2 * q + push[1].xy` (push words 4, 5 = output offset) with a
   bounds test against `1 + push[1].zw` (words 6, 7) and `1 + push[0].zw` (words 2, 3), V.
7. Push words read by 14c1 (V): 2, 3, 4, 5, 6, 7, 8, 9, 18, 19 of 20.

What is not known yet for a bit-faithful reimplementation: the exact HMMA chain structure (which weight tile, which
grid rows), the position of the ReLUs, the bias handling, the tap layout of the input block (`in x in` pixels of G
into the 16 K lanes), the shape of the network grid and its alignment, the meaning of words 18, 19, and the fixed-ratio
agl_ndsr variant's epilogue. All of these come from lane-exact emulation (section 13).

## 8. HMMA, LDSM and MOVM patterns (V counts, whole program)

All tensor instructions are `HMMA.16816.F16` (f16 accumulate, rounding after every HMMA as in NDVE). No `.F32` HMMA.

| Group | HMMA | LDSM.x4 | LDSM.x4.T | MOVM | SHFL | other |
|---|---|---|---|---|---|---|
| agl_ndsr networks (22) | 88-384 | 6-12 | 6-12 | 48-192 (= HMMA / 2) | 0 (4 and 10 in the 4/3 pair) | TLD 8-24, TEX 8-40, SUST 8-40, BAR 6-12 |
| vr networks (15) | 144-576 | 0 | 0 | 0 | 0 | TLD4 3-7, TEX 29-101, MUFU, SUST 4 |
| vr_light networks (15) | 176-192 | 0 | 0 | 0 (8 in the 4/3 pair) | 0 | TLD4 6-15, TEX 16-18, SUST 16-18 |
| SMAA compute (5 + 2) | 0 | 0 | 0 | 0 | 3 in f8965bff | atomics in f8965bff |

- The agl_ndsr networks have exactly the NDVE resampler's pattern: `LDSM.x4` (A from staged input), `LDSM.x4.T`
  (B, weights), and one `MOVM` (8x8 b16 transpose) per two HMMAs (the D -> A hand-over between stages through a
  transpose, ndve-native.md section 9). These are the shaders whose MOVM/LDSM the translator must handle.
- The vr and light networks load fragments with plain `LDS` and rearrange with `PRMT`/`HADD2` (I), so the fragment
  layout of section 9 of ndve-native.md applies to their HMMA operands but the lane-to-address map is per program.
- Lane dependence (I): every operand address is lane arithmetic (g = lane >> 2, t = lane & 3) as in NDVE; `SHFL` only in
  the edge detection (warp reduction of the edge count) and in the agl 4/3 networks.

## 9. Fingerprint for runtime detection

A frame runs the vr NDSR upscaler if (V for the static parts):

1. The dispatch keys contain `f8965bffbdf51d78`, `d17eefaff4f0bc23`, one of `ddc1143b7932a93e` / `92918e9b7e2f89a4` /
   `3426ccfe7d691d0f`, then one of the 15 vr network keys of section 4 (or the light equivalents). The network key
   alone is enough to replace the network; the SMAA passes can stay translated.
2. The 0x3000 bytes bound as ssbo 2 of the network dispatch hash to the blob of that key's ratio (section 5).
3. The config is implied by the key: the plan takes `b0`, `b1`, `b2` from a table keyed by program key.

For `agl_ndsr.pck` the same with its 22 keys and 11 blobs (SHA-256 from `pck.py`), and the graphics SMAA passes as
draws, not dispatches.

## 10. What to capture at runtime (R)

1. Which pack runs in gameplay, and which network key per frame as the dynamic resolution changes (log the key and the
   push words of every distinct network dispatch once).
2. Per network dispatch: the grid size, all 20 push words (vr) / 29 (light) / 18 (agl) as u32 and f32, the tex0
   texture (size, format, sRGB flag) and its sampler (filter, address mode), the img1 image (size, format), the ssbo 2
   address and the first 0x3000 bytes.
3. Per SMAA dispatch: push words (29), textures and images with formats and sizes, the ssbo 8/9 addresses and sizes,
   the indirect argument buffer of pass 2, the area texture binding (expect the 192x128 RG8 blob).
4. The neighbourhood-blending variant used and its push row 4 (the float-bit bias).
5. Whether the AA output is read by anything but the network (if not, the native plan may fuse it).
6. Two frames' input and output images for the compare mode (`NXKIT_NDSR_COMPARE=1`).

## 11. Cost

MAC count (I: assumes every warp of the threadgroup executes every HMMA of the program once, no loops; each HMMA is
16 x 8 x 16 = 2048 MAC):

- vr 1.2 (14c1, 144 HMMA, step 39 x 29) at 1536x864 in: 40 x 30 = 1200 threadgroups x 4 warps x 144 = 691k HMMA =
  **1.42 GMAC**.
- vr 3.0 (e4b4, 560 HMMA, step 31 x 23) at 640x360 in: 21 x 16 = 336 x 4 x 560 = 753k HMMA = **1.54 GMAC**.
- vr 12/7 variant B (e306, 576 HMMA, step 55 x 41) at 1120x630: 21 x 16 x 4 x 576 = 774k HMMA = 1.59 GMAC.
- So about 1.4-1.6 GMAC per frame whatever the ratio. At the NDVE kernels' 2.1-2.5 TMAC/s on an M2 Max: **0.6-0.8 ms**
  for the network. The SMAA passes are small (about 0.2-0.5 ms at 1536x864, I). The translated chain's time is R
  (measure with `NXKIT_PACE_DIAG=1`).

## 12. Where it belongs (recommendation)

A core crate, not the family layer: **`nxkit-ndsr` (L1, std only)**, next to `nxkit-ndve`, with `nxkit-ndve` later
taking its resampler from it.

- NDSR is not agl code only: the NDVE movie file embeds an NDSR pack, and its resampler `4f41ccf24f163e0d` is an NDSR
  network (config 0x18067521, same 0x800-byte blocks, same LDSM/MOVM pattern as agl_ndsr). The library is Nintendo SDK
  knowledge used by agl and by `nn::ndve`, so its kernels belong with the other SDK network.
- Detection is by content (program keys and weight hashes), as for NDVE. It needs no title fact, and `nxkit-gpu` must
  not depend on `titles/` (layers L1 vs T).
- What the family layer `titles/families/agl` should hold: the fact that agl games use NDSR (the `ShaderRole::Upscaler`
  hook, with its archive match fixed to the three `.pck` names and the evidence upgraded to checked), and, if ever
  needed, a runtime hook that says which pack a title uses or turns the native path off for a title. No kernels.

## 13. Unknown after this pass

- Lane-exact semantics of every network program (sections 7, 8). The NDVE emulator needs `TLD4` (0xb63; four texels,
  one component) and `MUFU` (symbolic `rcp`, `rsq`, `lg2`, `ex2`, `sqrt`) before it can run them; then the method of
  ndve-native.md section 5 applies: HMMA chains, A/B row bases, weight offsets, epilogue expression trees.
- The SMAA compute passes in detail (the edge list format, the indirect arguments, the area texture layout 192x128).
- Which pack and which variant run in gameplay; the input and output formats and sizes; all push values.
- The meaning of config byte `b3`, header word +0x10 byte 3, and push words 18, 19 of the vr networks.
- The HMMA internal rounding (as for NDVE).
