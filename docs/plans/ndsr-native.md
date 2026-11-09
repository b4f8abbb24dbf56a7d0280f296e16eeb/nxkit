# agl NDSR upscaler: what it is

Game: Zelda Ocarina of Time (0400486024DEC000, v131072). Engine family agl. The upscaler is Nintendo's NDSR ("neural
super resolution"). It is the same library whose pack sits inside the `nn::ndve` movie file
([ndve-native.md](ndve-native.md)): the NDVE resampler `4f41ccf24f163e0d` is an NDSR network.

Claim marks, as in ndve-native.md:

- **V** = verified from file bytes, the translated MSL or the SASS. **V (emulated)** = a Python model of the network
  program, written from the emulated SASS, gives the same output bits as a lane-exact emulation of the SASS (all
  threadgroup lanes, concrete data) for every output pixel of the tested threadgroups (section 7.10).
- **I** = inferred, not re-derived from the code.
- **R** = only answerable at runtime (section 10).

Scratch tools (not in the repo): `/private/tmp/claude-501/-Users-nzaljic-repos-me-nxkit-rt/2bd3ad49-183b-493a-8d88-aa4b15fad0cc/scratchpad/ndsr/`
(`sarc.py` extracts the packs, `pck.py` lists a pack, `ndsrlib.py` maps pack entries to shader keys, `table.py`
prints the per-shader table of section 4, `lstn.py <key>` prints the SASS listing through the NDVE disassembler,
`common.py` is a shim that lets the NDVE emulator `ndve/emu.py` load NDSR shaders). Added in this pass:

- `cemu.py`: the NDVE emulator extended to NDSR. `CEmu` runs a threadgroup on concrete values (weights from the blob,
  textures as functions, f16 ops rounded to nearest even from the exact result, f32 ops with `.FTZ` and the directed
  rounding modes, MUFU as correctly rounded `rcp/rsq/sqrt/lg2/ex2/sin/cos`, HMMA = f16 of the exactly rounded sum of the
  16 products and C). New ops: `TLD4` (gather, `.PTP` offsets), `TEX` (bilinear, lod 0), `TLD`, `SUST`, `LDC`, `S2UR`,
  `MUFU`, `FSETP`, `FMNMX`, `FSEL`, `F2I`/`I2F`/`FRND` with rounding modes, `LDS`/`STS` 16-bit, `HMNMX2`, `HADD2.F32`.
  `SEmu` is the symbolic variant (weights and texels as tokens, f32/f16 expression trees) used to read the HMMA chains.
- `refvr.py`: the reference model of the vr network programs (section 7), `cmpvr.py` / `batchvr.py`: emulator against
  model, `s3.py <key> <ct> <warp>`: the HMMA chains of one warp, `tabrule.py`: the cell-offset immediates of section 7.4,
  `aglperm.py`: the agl_ndsr weight layout and the agl -> vr permutation (section 8.3), `tr.py`: a one-lane trace.

## 1. Summary

- Three NDSR packs ship in `Lib/agl/agl_resource.Nin_Ounce_NVN2.release.sarc.zs`: `agl_ndsr.pck`, `agl_ndsr_vr.pck`,
  `agl_ndsr_vr_light.pck` (V). Together they hold 67 distinct programs in the shader map: 3 vertex + 5 pixel programs
  and 59 compute programs (22 + 20 + 17), 13,570 HMMA instructions in all (V, matches the task's 13,570).
- Each pack is **anti-aliasing + a single-frame neural upscaler**. The anti-aliasing is SMAA-style morphological AA
  (edge detection, blending weights, neighbourhood blending; section 6.1: the edge detection and the neighbourhood
  blending are SMAA's, the compute blending-weight pass is not SMAA's reference shader). The network is a 6-layer
  **separable bilinear MLP** on 16x16 patches of one channel: per layer `Y = W1_a Z W2_a + W1_b Z W2_b`, ReLU and a
  transpose between layers, f16 everywhere, 0x800 bytes of weights per layer (V emulated, section 7.3). Each patch gives
  a 12x12 block of a grid at `12 / in` times the input resolution, with a **fixed ratio** baked into each network (1.2,
  1.25, 4/3, 1.375, 1.5, 5/3, 12/7, 11/6, 2, 2.2, 2.4 or 3). The vr networks then sharpen that grid and resample it
  bilinearly to any output size.
- **All 15 vr network programs are recovered bit for bit** (V emulated, section 7.10): input window, HMMA chains, weight
  element order, ReLU positions, the grid with its bilinear base, the unsharp mask, the arbitrary-ratio resample, the
  chroma reconstruction, the three output encodings (A: sRGB 8-bit, B: linear 10-bit, C: linear 11/11/10 float) and the
  integer hash dither. The model is in `scratchpad/ndsr/refvr.py`.
- **Not temporal** (V from the bindings of all 59 compute and 5 pixel programs): no depth, no motion vectors, no
  history, no exposure, no jitter. One colour texture in, one image out.
- The three packs are three implementations of the same models: `agl_ndsr.pck` is the graphics-pipeline SMAA plus
  LDSM/MOVM networks at fixed ratio; `agl_ndsr_vr.pck` is compute SMAA plus compute networks that resample to an
  **arbitrary** output size ("vr" = variable ratio, I) from five fixed-ratio models; `agl_ndsr_vr_light.pck` is the same
  with cheaper networks (about 190 HMMA per program instead of up to 576). The vr weight blobs are the `agl_ndsr.pck`
  blobs of the same ratio with the f16 values reordered: the permutation is now known exactly (V for the 1.2 blob, all
  6144 values, section 8.3), and the agl_ndsr layout is the NDVE resampler's layout.
- Ocarina of Time uses the **vr_light** pack in gameplay: network `fa3033d05c0a41d6` (4/3), 1536x864 -> 2048x1152
  R11G11B10F (V from the capture, section 10.1). **All 15 vr_light programs are recovered bit for bit too** (section
  15): they have a fixed ratio, the grid is the output tile, and the epilogue runs in f16 with a luma sharpening.
- Cost (section 11): 98,304 MAC per 16x16 patch, about 1.3-1.5 GMAC per frame for the network at any ratio (V for the
  per-patch count, I for the frame), well under the movie network's 10.6 GMAC; on an M2 Max about 0.6-0.8 ms plus
  0.2-0.5 ms for the SMAA passes (I).

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
| 3 | ddc1143b7932a93e / 92918e9b7e2f89a4 / 3426ccfe7d691d0f | 128 | SMAA neighbourhood blending, 3 output variants (section 6.1; 92918e9b is the one that feeds the vr networks) | weights tex5, colour tex3 (bilinear sampler) | img2 | ceil(W/16) x ceil(H/16) (cfg 0x10,0x10; V from MSL: 16x16 px, 2 px per thread) |
| 4 | one of 15 networks | 128 | NDSR upscale | tex0 (+ sampler), weights ssbo 2 (0x3000) | img1 at the output size | ceil(W_in/b0) x ceil(H_in/b1) (I from the tile rule of section 7.6, which needs exactly these threadgroups) |

Neighbourhood blending (V from MSL of ddc1143b): per pixel read the weights texel `a`; if `max(max(a.x,a.z),
max(a.y,a.w)) < 1e-5` copy the colour; else pick horizontal or vertical by `max(a.x,a.z) > max(a.y,a.w)`, take two
bilinear colour samples at `uv ± w * texel` clamped to the push rectangle (bank 24 row 3), blend with weights
`w1/(w1+w2)`, `w2/(w1+w2)` (f32 fma order as in the MSL). Output `float4(rgb + bits(push row 4), alpha)` where the
push row 4 words are **added to the float bits** (an exponent bias, i.e. a power-of-two scale; R for the values).
Edge threshold 0.1 rounded to f16 (`0.099975586`, V in f8965bff).

Network programs (V: keys, config, HMMA, ratio immediate from the SASS; config bytes `b0 b1` in brackets):

| Weights (pack offset) | Ratio (immediate) | Variant A | Variant B | Variant C | HMMA A/B/C |
|---|---|---|---|---|---|
| 0x88710 | 1.2 (0x3f99999a) | 14c10aa70b81f04e (271d) | f407ab4429b8eae6 (271d) | caf3a6e1b2f70605 (2727) | 144/144/192 |
| 0x8b710 | 1.5 (0x3fc00000) | 49e67caae643e42a (1f1f) | 3bed6a425f72e4d6 (1f1f) | 7a5a51af95b27ae5 (1f1f) | 186 each |
| 0x8e710 | 12/7 (0x3fdb6db8, not f32(12/7)) | c16ce0c879c981a3 (1b1b) | e30678db92cf66ff (3729) | 4aa7eec58d2fa656 (3714) | 192/576/288 |
| 0x91710 | 2.0 (0x40000000) | 45f7364945aff920 (1717) | 17c6bd35f159a957 (2f17) | 57cdcd5913bd385f (2f17) | 192/384/384 |
| 0x94710 | 3.0 (0x40400000) | e4b49569547e3d47 (1f17) | f0c1f33f84b619dc (1f17) | aec4dfce7ab3cefa (1f17) | 560 each |

All three variants run the same network, grid, unsharp mask and resample (V emulated); they differ only in the tile
(config bytes `b0 b1`) and the output encoding (section 7.7): **A** = sRGB-encoded with a dither of 1/255 (an 8-bit
UNORM image, I), **B** = linear with a dither of 1/1023 (a 10-bit UNORM image such as RGB10A2, I), **C** = linear with a
dither of one unit of the last place of an 11/11/10-bit float (R11G11B10F, I). All 15 read the input written by
neighbourhood-blending variant `92918e9b7e2f89a4` (they all invert its luma, section 7.6). Which variant runs is R; the
image format of img1 decides it (I).

### vr_light pack

Pre-passes: the three vr programs 1, 2, 3a, plus `1fdc85178b60f6ce` and `a335c1db6270c355` (neighbourhood blending
variants, 0x800 and 0xd00 code bytes). Networks (config, HMMA): 9327ea56aafa15b9 / 8df148d58b7cbe8e (2828, 1.2, 192);
927ceaf3cf1be240 (3030, 4/3, **256 threads**, 176) / fa3033d05c0a41d6 (3018, 4/3, 176); 48d00da84b3484df /
aad7ea7e5c02f41d (2020, 1.5, 186); 6160882513197af3 / b106afba1a18faa9 (1c1c, 12/7, 192); 2f9bca45ef842a3b /
04135bb812a0d66e (1818, 2.0, 192); d8fffc05afaaa981 / 7c511cc23ff85d14 (1414, 2.4, 192); bdb298c6aa71a298 /
c8b20adc9d4978e0 / d99fbeeb53d38847 (1010, 3.0, 188). The 4/3 networks use a 0x1800-byte blob (3 blocks). The light
networks compute luma in f16 (`0.71484375` = f16(0.7152), `0.072143555` = f16(0.0722)) where vr gathers one channel.
Key assignment for the two shared control sections follows entry order (I).

Light network structure (V from the symbolic HMMA chains of 9327ea56aafa15b9, warp 0; compared bit for bit in section 15):
the same 6-layer, 2-term separable network as vr (section 7.3), the same weight sub-blocks (vr_light's 1.2 blob is
vr's), ReLU by `HMNMX2`. The input element is computed per texel in f16 as
`hfma2(G, 0x39b8, hfma2(R, 0x32cd, hmul2(B, 0x2c9e)))` from TLD4 gathers of R, G and B, i.e. the luma of the
square-root RGB that neighbourhood-blending variant `1fdc85178b60f6ce` writes (section 6.1). The light programs read 29
push words (vr: 20); their epilogue was not emulated.

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

agl_ndsr network structure (V from the symbolic HMMA chains of abef20ad0d9a9b12, warp 0; not compared bit for bit):
the same 6-layer, 2-term network, but in the NDVE resampler's convention (no transpose between layers, the D -> operand
hand-over goes through `MOVM`) and with the NDVE weight layout (section 8.3). The input element is the f32 luma
`fma(B, 0.0722, fma(R, 0.2126, G * 0.7152))` of TLD texels, rounded to f16, exactly as NDVE's `ndve_luma`.

## 5. Network config and weights

**Config (type 14), 4 bytes `b0 b1 b2 b3`.**

- `b0`, `b1` = input pixels per threadgroup step in x and y (V emulated for all 15 vr programs). A threadgroup holds
  `npx = (b0 + 1) / in` by `npy = (b1 + 1) / in` patches (V emulated), so neighbouring threadgroups overlap by one input
  pixel, which the resample of section 7.6 needs at the tile edge.
- `b2` = `(out - 1) << 4 | (in - 1)`: the network maps the centre `in x in` pixels of a 16x16 patch to an `out x out`
  block of the grid, ratio `out / in` (V emulated for vr: `out` = 12 in all vr programs; `in` = 10, 8, 7, 6, 4). The
  ratio the shader multiplies by is an immediate (section 4 table), for 12/7 one ulp above f32(12/7). NDVE's resampler
  has 0x75 -> 8/6 = 4/3.
- `b3`: 0x01 (agl_ndsr), 0x22 (vr), 0x12 (vr_light), 0x21 (NDVE). A variant or version field; meaning unknown.

**Weights (type 13).** 0x3000 bytes = 6 layers of 0x800 bytes (1024 f16: four 16x16 matrices), or 0x1800 = 3
layers for the 4/3 models of agl_ndsr / vr_light (I: 3 layers, not emulated). No biases (V emulated: no bias term in
any vr chain). All values finite, range about -4 to +4 (V). NDVE's resampler 4f41 uses the same 0x800-byte layer.

- The vr blobs are the agl_ndsr blobs of the same ratio with the f16 values permuted: the permutation is section 8.3
  (V for the 1.2 blob, every one of the 6144 values; equal multisets for 1.5, 12/7, 2.0, 3.0). The vr_light blobs
  equal vr's for 1.2, 1.5, 12/7, 2.0, 3.0 (byte-identical, V); its 4/3 blob is agl_ndsr's 4/3 permuted (V); its 2.4
  blob is new.
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

## 6. Inputs and outputs

- In: one colour texture at the render resolution (W x H, dynamic: about 763x429 .. 1536x864 per the task, R). It
  feeds the SMAA edge detection and the neighbourhood blending; the network reads the blending output (section 6.1).
- Intermediates (vr): edge image (img0 of pass 1), edge list and counter (ssbo 8, 9), weights image r32ui (pass 2),
  anti-aliased colour (img2 of pass 3). Formats and sizes R, expected W x H.
- Out: one image at the output size (img1 of the network). Expected 1920x1080 docked (I). Its format follows from the
  variant (section 7.7): A 8-bit UNORM, B 10-bit UNORM, C R11G11B10F (I from the dither amplitudes).
- No depth, motion, history, exposure or jitter input in any program (V).

### 6.1 The anti-aliasing passes (V from the MSL unless marked)

- **Edge detection** `f8965bffbdf51d78` (vr and vr_light): SMAA's luma edge detection with threshold 0.1 (as f16,
  `0.099975586`) and SMAA's local contrast adaptation factor 2.0; reads the colour with `read` (TLD, integer
  coordinates), writes the two edge flags (left, top) as two floats to img0 and appends every edge pixel to a list in
  ssbo 0 (`atomic_fetch_add` on word 0 = count, `atomic_fetch_max` on word 1; I: the indirect dispatch size of pass 2).
  Constants 0.1 and 2.0 only: the SMAA reference shader's predication and the colour/depth variants are absent.
- **Blending weights** `d17eefaff4f0bc23`: runs over the edge list (indirect dispatch), gathers the edge image (TLD4)
  and reads a **192x128 RG8 area texture** (tex6, the pack's texture) and writes packed RGBA8 weights to an r32ui
  image. Constants 255, 4, 0.75 and none of SMAA's search constants; the area texture is not SMAA's 160x560 AreaTex
  (which holds the diagonal and sub-pixel tables). So this is an SMAA-style orthogonal blending-weight pass of
  Nintendo's own (V for the constants and sizes, I for the meaning). The graphics pack `agl_ndsr.pck` instead has
  SMAA's own textures (160x560 AreaTex, 64x16 SearchTex) and the reference search offsets (-3.75 / 3.25).
- **Neighbourhood blending** (`ddc1143b`, `92918e9b`, `3426ccfe`; light: `1fdc8517`, `a335c1db`): SMAA's
  neighbourhood blending (pick the larger of the horizontal and vertical weight pairs, two bilinear colour samples at
  the offsets, weights `w / (w1 + w2)`, copy when all weights are below 1e-5). They differ only in what they write
  (V from each MSL's write): `ddc1143b` writes the blended linear colour; **`92918e9b` writes
  `(sqrt R, Y', sqrt B, a)` with `Y' = fma(sqrt B, 0.0722, fma(sqrt R, 0.2126, sqrt G * 0.7152))`** (the luma of the
  square roots, f32); `3426ccfe` packs one 32-bit value (x only); `1fdc8517` writes `(sqrt R, sqrt G, sqrt B, a)`;
  `a335c1db` packs one value. Each written float gets the push row 4 word **added to its bits** (an exponent offset,
  i.e. a scale by a power of two; R for the values, 0 expected).
- **What the network reads** (V emulated against the format): every vr network takes Y' from tex0.y and reconstructs
  `sqrt G = 1.3982103 Y' - 0.2972595 sqrt R - 0.10095078 sqrt B` (the exact inverse of `92918e9b`'s luma), so the vr
  networks read the output of `92918e9b`; the light networks build their own f16 luma from `1fdc8517`'s output. That
  the bound tex0 is that image (and not the scene colour) is R, but nothing else produces this format.

## 7. The vr network program (V emulated for all 15 vr programs unless marked)

Notation: `f16()` rounds to nearest even; f32 operations round to nearest with denormals flushed (the SASS `.FTZ`);
`fma` is one rounding; `rcp`, `sqrt`, `lg2`, `ex2` are the hardware's MUFU (their exact error is a hardware property,
section 13; the model uses correctly rounded values). `W_in = w2 + 1`, `H_in = w3 + 1`, `W_out = w6 + 1`,
`H_out = w7 + 1` (push words, section 7.9). `(wx, wy)` = threadgroup id. Per program the constants are `b0, b1, in`
(config), `R` (the ratio immediate, section 4), `npx = (b0 + 1) / in`, `npy = (b1 + 1) / in`, `gw = 12 npx`,
`gh = 12 npy`, `m = (16 - in) div 2` (3, 4, 4, 5, 6 for in = 10, 8, 7, 6, 4).

### 7.1 Input window

Texel `(i, j)` of the window is `X(i, j) = f16(tex0.y at (b0 wx - m + i, b1 wy - m + j))`, `i < in (npx - 1) + 16`,
`j < in (npy - 1) + 16` (1.2 A: 46 x 36 from `(39 wx - 3, 29 wy - 3)`).

- 12 of the 15 programs load it with `TLD4.PTP` gathers of component 1 at `u = (x + 1) * w8`, `v = (y + 1) * w9` with
  the four offsets (0,0), (1,0), (0,1), (1,1) packed in two registers; each gather returns texels
  `(x, y), (x + 1, y), (x, y + 1), (x + 1, y + 1)` as f16 (I: the emulator applies GL's rule that each offset selects
  texel `i0 j0` of its footprint, the only reading under which the 414 gathers tile the window without gaps or
  repeats; the emulation cannot test the hardware rule). Outside the texture the sampler's address mode decides (R;
  clamp to edge expected and modelled).
- The 3.0 programs (`e4b4`, `f0c1`, `aec4`) load with `TLD` (integer fetch) and clamp the coordinates themselves to
  `[0, w2] x [0, w3]` (V), then `F2FP` to f16.
- Shared layout (1.2 A): each gather writes its two f16 pairs to two half-rows, pitch 0x70 bytes (56 halves) per input
  row. Not needed for a reimplementation.

### 7.2 Patches

Patch `(q, p)`, `q < npx`, `p < npy`, is the 16x16 block of the window at `(in q, in p)`. As an HMMA operand it is
`X[k][n] = X(in q + k, in p + n)`: K runs along **x**, N along **y** (V emulated: `B[k][n]` of the first HMMA of warp 0
is texel `(192 + k, 142 + n)` for 1.2 A at (5,5)). Warps take patches by column (`q`), all rows `p` (1.2 A: warp `w` =
`q`; 3.0: two columns a warp, V); the order does not change the result.

### 7.3 The network

Twelve weight sub-blocks `s = 0..11` of 512 halves (section 8.2 for the element order): `W1_s` (16x16, rows r, columns
k) and `W2_s` (16x16, rows k, columns n). Six layers `L = 0..5`, each using sub-blocks `2L` and `2L + 1`:

```
Z_0 = X
for L in 0..5:
    H_a = f16(W1_{2L}   Z_L)                 # 16x16, one HMMA (K = 16) per 8 columns
    D_a = f16(H_a W2_{2L})                   # C = 0
    H_b = f16(W1_{2L+1} Z_L)
    Y_L = f16(H_b W2_{2L+1} + D_a)           # C = D_a: the two terms chain through the accumulator
    if L < 5: Z_{L+1} = max(Y_L, +0)^T       # HMNMX2 (ReLU), then a transpose: Z[k][n] = max(Y[n][k], 0)
Y = Y_5                                      # no ReLU
```

- Every `f16()` is one HMMA.16816.F16: one rounding of a K = 16 dot product plus C (V for where the roundings are; the
  rounding inside one HMMA is the hardware unknown NDVE has too).
- No bias, no other operation between the HMMAs (V: the only instructions on the fragments are the HMMAs, `HMNMX2`
  against RZ with `!PT`, i.e. max, and register moves).
- The transpose costs nothing in the SASS: the D registers of layer L are used directly as the B operand of layer
  L + 1 (D holds `(row g, columns 2t, 2t+1)`, B holds `(k 2t, 2t+1, column g)`), no MOVM, no PRMT (V).
- After 6 layers (an even number of transposes after layer 0) `Y[r][c]` is indexed `r` = y, `c` = x of the patch.

### 7.4 The grid (threadgroup memory, gw x gh f16)

Patch `(q, p)` fills the cells `G[12 p + r][12 q + c]`, `r, c < 12`, from `Y[r][c]` (the top-left 12x12 of the 16x16
result; rows and columns 12-15 are dropped, V emulated):

```
S  = f16(bilinear sample of tex0.y at  u = (tx[12q + c] + float(b0 wx)) * rcp(float(W_in)),
                                       v = (ty[12p + r] + float(b1 wy)) * rcp(float(H_in)))      # TEX, lod 0, f16
s  = float(S) + float(Y[r][c])                                                                  # f32
G  = f16(s * |s|)
```

`tx[j]`, `ty[i]` are the cell centres in input pixels relative to the tile, `(j + 0.5) / R`, computed with immediates
`(s, h, h1)` that the compiler folded differently per program (V emulated, `tabrule.py`): for even `j`
`tx[j] = fma(float(j), s, h)`, for odd `j` `tx[j] = fma(float(j - 1), s, h1)`; `ty` likewise, except that 14c1, f407
and caf3 use `fma(float(i), s, h)` for every row.

| in | s | h | h1 (x) | h1 (y) |
|---|---|---|---|---|
| 10 (1.2) | 0x3f555556 | 0x3ed55556 | 0x3fa00000 | (not used) |
| 8 (1.5) | 0x3f2aaaab | 0x3eaaaaab | 0x3f800000 | 0x3f800000 |
| 7 (12/7) | 0x3f155556 | 0x3e955556 | 0x3f600000 | 0x3f600001 |
| 6 (2.0) | 0x3f000000 | 0x3e800000 | 0x3f400000 | 0x3f400000 |
| 4 (3.0) | 0x3eaaaaab | 0x3e2aaaab | 0x3f000000 | 0x3f000000 |

`S` is the bilinear base in the square-root domain; the network output is a delta to it; the grid holds the signed
square, i.e. linear light (I for the interpretation). The texture sampler (filter, address mode, sub-texel precision)
is the game's (R): the model uses exact bilinear weights and clamp to edge.

### 7.5 Unsharp mask (f16)

With `k1 = f16(fma(w18, 0.5, 1.0))` and `k2 = f16(w18 * -0.125)` (`w18` = push word 18 as f32), every cell becomes

```
t1 = f16(G[i][j] * k1)
t2 = f16(G[i][j+1] * k2 + t1)
t3 = f16(G[i-1][j] + G[i][j-1])
t4 = f16(t3 * k2)
t5 = f16(G[i+1][j] * k2 + t4)
H[i][j] = f16(t2 + t5)
```

i.e. `G + w18 / 2 * (G - mean of the 4 neighbours)` (weights sum to 1), at the grid edge **mirrored in x** (column -1
reads column 1, column gw reads gw - 2) and **clamped in y** (V emulated, every cell of the 1.2 A grid checked
separately). One barrier before and one after; H replaces G in threadgroup memory.

### 7.6 Output pixels

Tile of threadgroup `(wx, wy)`, end exclusive (integer arithmetic; V emulated, the pixel sets match):

```
x0 = 0 if wx == 0 else ((2 b0 wx + 1) W_out + W_in - 1) div (2 W_in)
x1 = W_out if b0 (wx + 1) >= W_in else ((2 b0 (wx + 1) + 1) W_out + W_in - 1) div (2 W_in)
```

(y likewise with `b1`, `H`). The 128 threads loop over 2x2 pixel quads of the tile. For each pixel `(x, y)`:

```
sxW = float(W_in) * R;              sx = sxW * rcp(float(W_out))
ox  = fma(-(float(b0 wx) * sxW), rcp(float(W_in)), fma(sx, 0.5, -0.5))
gx  = fma(sx, float(x), ox)                       # grid x of the pixel centre, cell centres at integers
ix  = floor(gx);   fx = gx - float(ix)            # F2I.FLOOR, FRND.FLOOR, FADD
(gy, iy, fy likewise with H, b1 wy)
a = H[iy][ix], b = H[iy][ix+1], c = H[iy+1][ix], d = H[iy+1][ix+1]     (indices clamped to the grid)
top = fma(fx, b, fma(-fx, a, a));  bot = fma(fx, d, fma(-fx, c, c))
v   = fma(fy, bot, fma(-fy, top, top))
g   = sqrt(max(v, +0))                            # back to the square-root domain
T   = tex0 RGB sample (bilinear, lod 0, f32) at ((float(x) + 0.5) * rcp(float(W_out)), (float(y) + 0.5) * rcp(float(H_out)))
r'  = g + (T.x - T.y);   b' = g + (T.z - T.y)     # chroma carried as the difference to Y'
g'  = fma(g, 1.3982103, fma(r', -0.2972595, b' * -0.10095078))       # sqrt G from Y', immediates 0x3fb2f88e etc.
lin = c' * |c'| for c' in (r', g', b')            # signed square: linear light
```

The shader clamps the indices per quad slot, some with `max(., 0)`, some with `min(., gw - 1)` / `min(., gh - 1)` (V);
clamping all four to the grid reproduces it for every tested pixel, including the first and last tiles.

### 7.7 Output encoding (per variant)

- **A**: `o = lin >= 0.0031308 ? fma(ex2(lg2(lin) * 0.41666666), 1.055, -0.055) : lin * 12.92` (`FSETP.GEU`, so the
  equal case and NaN take the power branch; immediates 0x3b4d2e1c, 0x3ed55555, 0x3f870a3d, 0xbd6147ae, 0x414eb852).
- **B**: `o = lin`.
- **C**: `o = lin`.

### 7.8 Dither (when `w19 != 0`)

```
h = ((x * 1597334677) ^ (y * 3812015819)) * 1597334677        # u32 wrap-around, x / y before the offset w4 / w5
d = int(h & 0xffff) - int(h >> 16)                             # -65535 .. 65535, triangular
A: o = fma(float(d), 5.983931e-08, o)                           # 1 / (255 * 65535)
B: o = fma(float(d), 1.4915955e-08, o)                          # 1 / (1023 * 65535)
C: q = float(d) * 1.5259022e-05;  o = fma(q, e, o)              # 1 / 65535 times one ulp of the target float:
   e = sat(as_float((as_uint(o) & 0x7f800000) - (k << 23))),   k = 6 for x and y, 5 for z (11/11/10-bit floats)
```

(3812015819 = -482951477 as u32.) The image write gets `(o_r, o_g, o_b, 1.0)` at `(x + w4, y + w5)`; alpha is always
1.0 (V). The quantisation to the image format is the write's (R for the format).

### 7.9 Push words (bank 24, 20 words; V: the only words the vr programs read)

| Word | Meaning |
|---|---|
| 2, 3 | `W_in - 1`, `H_in - 1` (int; the tile rule, the in-shader `rcp(W_in)`, the TLD clamp of the 3.0 programs) |
| 4, 5 | output offset added to the written pixel position (int) |
| 6, 7 | `W_out - 1`, `H_out - 1` (int) |
| 8, 9 | `1 / W_in`, `1 / H_in` (f32; only the TLD4 coordinates use them) |
| 18 | unsharp strength (f32) |
| 19 | dither on (int, != 0) |

Words 0, 1 and 10-17 are not read (V).

### 7.10 What was verified, and how

- Method: `cmpvr.py` runs one threadgroup in `CEmu` (all 128 lanes, the real weight blob, a random or a smooth
  synthetic (sqrt R, Y', sqrt B) texture) and the model `refvr.py` on the same inputs, and compares every image write
  (bits of all four channels) and, for the grid, every cell.
- `batchvr.py`: per program 12 threadgroups (first, last and an inner one) at 1536x864, 1280x720, 960x540 and 763x429
  in, 1920x1080 out, with output offsets (5, 2), unsharp 0.3 / 0.5 / 1.0, dither on and off. Result: **0 differing
  pixels** for all 15 vr programs (14c1, 49e6, c16c, 45f7, e4b4, f407, 3bed, e306, 17c6, f0c1, caf3, 7a5a, 4aa7,
  57cd, aec4; 16,800 to 71,500 pixels each).
- What the emulation cannot verify: the hardware's HMMA internal rounding, the MUFU approximations, the texture
  filter's sub-texel precision and address mode (section 13).

## 8. HMMA chains, operands and weight layouts

All tensor instructions are `HMMA.16816.F16` (f16 accumulate). No `.F32` HMMA.

| Group | HMMA | LDSM.x4 | LDSM.x4.T | MOVM | SHFL | other |
|---|---|---|---|---|---|---|
| agl_ndsr networks (22) | 88-384 | 6-12 | 6-12 | 48-192 (= HMMA / 2) | 0 (4 and 10 in the 4/3 pair) | TLD 8-24, TEX 8-40, SUST 8-40, BAR 6-12 |
| vr networks (15) | 144-576 | 0 | 0 | 0 | 0 | TLD4 3-7 (TLD in the 3.0 ones), TEX 29-101, MUFU, SUST 4, HMNMX2 60-240 |
| vr_light networks (15) | 176-192 | 0 | 0 | 0 (8 in the 4/3 pair) | 0 | TLD4 6-15, TEX 16-18, SUST 16-18, HMNMX2 |
| SMAA compute (5 + 2) | 0 | 0 | 0 | 0 | 3 in f8965bff | atomics in f8965bff |

Correction to the first pass: the vr programs do use `HMNMX2` (opcode 0x240; the op table names it after HADD2, so the
counts of section 4's table list them as `HADD2.f1`). That is the ReLU.

### 8.1 Chains (V emulated, warp 0 of 14c1 / e4b4 printed by `s3.py`)

Per patch and layer: 2 HMMAs `W1 . Z` (A = weights, B = the 8-column halves of Z), 2 HMMAs `H_a . W2` (A = the two
stage-1 D's side by side as one 16x16 A, B = weights, C = 0), 2 more for term b with C = the term-a D's. That is 8 per
patch and layer, 48 per patch. Program totals: `48 * npx * npy / 4` per warp minus shared stage-1 halves: where the
patch rows overlap by exactly 8 (in = 8) or the layer-0 halves coincide (in = 4), the compiler computes a layer-0
`W1 . X` half once for two patches (1.5: 192 - 6 = 186; 3.0: 576 - 16 = 560). Every count of section 4 follows.

Operand sources (V): weights come from `LDG.128` straight into registers in fragment order (no shared staging, no
LDSM); the input patch from `LDS` of the f16 window; stage 2's A is stage 1's D registers; layer `L + 1`'s B is layer
`L`'s D registers after `HMNMX2` (section 7.3). Lane dependence: only the fragment layout of ndve-native.md section 9.

### 8.2 vr weight element order (V emulated)

The blob is stored in mma fragment order: lane `l = 4 g + t` loads 16 bytes, so element index `8 l + 2 j + e` is
register `j`, half `e` of lane `l`. For sub-block `s`:

```
W1_s[r][k]  = blob[512 s       + 8 (4 (r % 8) + (k % 8) / 2) + 2 (r / 8 + 2 (k / 8)) + k % 2]
W2_s[k][n]  = blob[512 s + 256 + 8 (4 (n % 8) + (k % 8) / 2) + 4 (n / 8) + 2 (k / 8) + k % 2]
```

(W1 is an A fragment: registers (row g, k 2t), (row g + 8, k 2t), (row g, k 2t + 8), (row g + 8, k 2t + 8); W2 holds
two B fragments, columns 0-7 in registers 0-1 and 8-15 in registers 2-3 of each lane.)

### 8.3 agl_ndsr layout and the agl -> vr permutation (V for the 1.2 blob, `aglperm.py`)

agl_ndsr (and NDVE's resampler) store a layer as 1024 halves `[W1_0, W1_1, W2_0, W2_1]`, each 256 halves as 8x8
row-major tiles: `W1_t[r][k]` at `256 t + 128 (k / 8) + 64 (r / 8) + 8 (r % 8) + k % 8`, `W2_t[k][n]` at
`512 + 256 t + 128 (n / 8) + 64 (k / 8) + 8 (k % 8) + n % 8` (V from the emulated LDSM addresses of abef20ad: tile
order `[kb][rb]` as ndve-native.md section 8). The vr sub-blocks are the same matrices:

- layer `L` odd: `vrW1_{2L+t} = aglW1_{L,t}`, `vrW2_{2L+t} = aglW2_{L,t}`;
- layer `L` even: `vrW1_{2L+t} = (aglW2_{L,t})^T`, `vrW2_{2L+t} = (aglW1_{L,t})^T`.

All 6144 values of the 1.2 blobs agree under this map. So one canonical model serves every pack, in the agl/NDVE
convention: `X = window[row = y][col = x]`, `Y_L = W1_0 Z W2_0 + W1_1 Z W2_1` (same two-step chain), `Z_{L+1} =
relu(Y_L)` without a transpose. The vr form is the transposed computation (even layers see `X^T`), which is why vr
needs no MOVM. Both give the same `Y_5` (rows = y). Package B can therefore repack any vr blob to the agl layout and
use one kernel.

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

### 10.1 Ocarina of Time: what the capture shows (V, `NXKIT_DIAG=ndsr`, `scratchpad/oot-ndsr60.log`, one gameplay minute)

The game runs the **vr_light pack**, network **`fa3033d05c0a41d6`** (4/3, the B/C program, section 15), at **1536x864 ->
2048x1152**. That was the only (key, size) pair in the whole minute: the minute showed no change of resolution and no
other network.

| Dispatch | Key | groups | What it binds (format, size) |
|---|---|---|---|
| -3 edge detection | `f8965bffbdf51d78` | 48 x 36 (= 1536/32 x 864/24) | tex3 scene colour Rg11B10Float 1536x864; img0 = tex4 Rg16Uint 384x216; img1 = tex5 Rgba8Unorm 1536x864; ssbo 8 (0xa2000 bytes) and 9 (0x10) |
| -2 blending weights | `d17eefaff4f0bc23` | indirect (logged 0, 0, 0) | the same set; tex6 Rg8Unorm 192x128 (the pack's area texture) |
| -1 neighbourhood blending | **`a335c1db6270c355`** | 96 x 54 (= 1536/16 x 864/16) | writes **img2 Rgb9E5Float 1536x864** (TexId 6149) |
| 0 network | **`fa3033d05c0a41d6`** | **32 x 36** (= 1536/48 x 864/24) | tex0 = that img2 (Rgb9E5Float), sampler min/mag filter 1 (linear), wrap 7 (clamp to edge); ssbo 2 = 0x1800 bytes from the blob start (the 4/3 blob); **img1 Rg11B10Float 2048x1152** |

- The descriptor set is shared by all four dispatches (the same 32 texture slots, img0 to img3), so tex0 of the network
  is img2 of the neighbourhood blending (same TexId): the network reads `a335c1db`'s RGB9E5 output.
- **Push words of the network** (29, hex): `0 0 5ff 35f 0 0 7ff 47f` (w2, w3 = W_in - 1, H_in - 1; w6, w7 = W_out - 1,
  H_out - 1), `3a2aaaab 3a97b426` (1/1536, 1/864), `3a000000 3a638e39` (1/2048, 1/1152), `3a001559 3a63d1b9 3f7fd555
  3f7fb426 39aaaaab 3a17b426` (w12 .. w17: 1/(1535 * 4/3), .., 1535/1536, 863/864, 0.5/1536, 0.5/864; not read by any
  light program), `40 20` (**w18, w19 = 64, 32: the output tile**), `3f800000 3f800000` (**w20, w21 = 1.0: grid cells
  per output pixel**), `0 0` (w22, w23 = grid origin), `800 480` (w24, w25 = 2048, 1152), `3a000000 3a638e39`
  (w26, w27 = 1/2048, 1/1152) and `40200000` (**w28 = 2.5: sharpening strength**). Words 4 and 5 (output offset) are 0.
- The SMAA dispatches push the 64 words of their own block (texel sizes, `5ff 35f` and `0x28800` at words 22 to 26).
- Users of the images in the frame: img2 (TexId 6149) is written by the three SMAA passes (the edge pass and the
  weights pass have it bound as well), read by the network, and **read again by two draws after the network** (cmd 15966
  and 15985). So it cannot be fused away. img1 (TexId 634) is written only by the network and read by one draw
  (cmd 15985: the consumer of the upscaled picture).
- The selection rule (which ratio for which sizes) is still R. Only the exact 4/3 ratio was seen, with `w20 = 1`.

What used to be open in this section and is now settled for Ocarina: the pack (vr_light, not vr), the variant (C:
R11G11B10F out), the input format (RGB9E5) and producer (`a335c1db`), the sampler (linear, clamp to edge) and the
readers of the images.

**Step 1, already possible with today's `NXKIT_DIAG=comp`** (one gameplay minute, walking from a bright to a busy scene
so the dynamic resolution moves). Per frame expect this order of `program` keys, with these `groups`:

| Dispatch | Key | groups (x, y, z) |
|---|---|---|
| edge detection | `f8965bffbdf51d78` | `[ceil(W/32), ceil(H/24), 1]` (I) |
| blending weights | `d17eefaff4f0bc23` | indirect (from the edge count) |
| neighbourhood blending | `92918e9b7e2f89a4` (vr), `1fdc85178b60f6ce` (light), `ddc1143b` / `3426ccfe` / `a335c1db` otherwise | `[ceil(W/16), ceil(H/16), 1]` |
| network | one of section 4 | `[ceil(W_in/b0), ceil(H_in/b1), 1]` |

The network key gives pack, ratio and variant; its groups give the input size. Groups for the vr keys (x by y):

| Input | 14c1 / f407 | caf3 | 49e6 / 3bed / 7a5a | c16c | e306 | 4aa7 | 45f7 | 17c6 / 57cd | e4b4 / f0c1 / aec4 |
|---|---|---|---|---|---|---|---|---|---|
| 1536x864 | 40x30 | 40x23 | 50x28 | 57x32 | 28x22 | 28x44 | 67x38 | 33x38 | 50x38 |
| 1280x720 | 33x25 | 33x19 | 42x24 | 48x27 | 24x18 | 24x36 | 56x32 | 28x32 | 42x32 |
| 960x540 | 25x19 | 25x14 | 31x18 | 36x20 | 18x14 | 18x27 | 42x24 | 21x24 | 31x24 |
| 763x429 | 20x15 | 20x11 | 25x14 | 29x16 | 14x11 | 14x22 | 34x19 | 17x19 | 25x19 |

Grep: `grep 'DIAG comp' oot-work.log | grep -E 'f8965bff|d17eefaf|92918e9b|1fdc8517|ddc1143b|3426ccfe|a335c1db|<15 vr keys>|<15 light keys>|<22 agl keys>'`.
That alone answers: which pack, which variant (A/B/C), which ratio at which input size (the selection rule: is it the
smallest model ratio >= W_out / W_in, the nearest, or fixed?), and whether the network follows `92918e9b`.

**Step 2, package C's `NXKIT_DIAG=ndsr`** (once per distinct (network key, input size, output size)):

1. Network dispatch: the key, groups, all 20 push words as u32 and f32 (expect w2/w3 = W_in-1/H_in-1, w4/w5 = offset,
   w6/w7 = W_out-1/H_out-1, w8/w9 = 1/W_in, 1/H_in, w18 = unsharp strength, w19 = dither on; words 0, 1, 10-17 for
   completeness); tex0: GPU address, size, format (incl. sRGB flag), and whether it is the img2 of the preceding
   neighbourhood-blending dispatch (same address); tex0's sampler: min/mag filter, address modes u/v, normalized; img1:
   address, size, format (settles A = 8-bit UNORM, B = 10-bit UNORM, C = R11G11B10F); ssbo 2: address, length, SHA-256
   of the first 0x3000 bytes (expect one of the five vr hashes of section 5).
2. Neighbourhood blending dispatch: key, groups, 29 push words (row 4 = words 16-18, the float-bit offset; I from
   the MSL: rows 0, 3 = texel size and clamp rectangle, rows 5, 6 = tile bounds and output offset), tex3 (scene colour: size, format), tex5
   (weights, r32ui), img2 (size, format: the network's input format), sampler of tex3.
3. Edge detection and blending weights: push words, img0 format, ssbo 8/9 (address, size), the indirect argument buffer
   of `d17eefaf` (three words), tex6 (expect 192x128 RG8, SHA-256 `80bcd38b...` of section 3), ubo_ptr 10.
4. Readers: any later dispatch or draw in the frame that binds img2's address (decides whether the AA output can be fused
   into the network's load), and what reads img1 (the consumer of the upscaled image).
5. For `NXKIT_NDSR_COMPARE=1`: tex0 and img1 raw contents of two frames at two different input sizes.

**Step 3, only if the light or agl pack shows up**: the same for their dispatches (29 push words for light, 18 for
agl; the agl SMAA passes are draws, so the draw keys of section 4 and their render targets).

## 11. Cost

Per 16x16 patch: 48 HMMA of 16 x 8 x 16 = 2048 MAC, **98,304 MAC** (V). Per frame (I: one dispatch, every threadgroup
full):

- vr 1.2 (14c1, 12 patches per threadgroup, 40 x 30 threadgroups at 1536x864 in): 14,400 patches = **1.42 GMAC**.
- vr 3.0 (e4b4, 48 patches per threadgroup, 21 x 16 threadgroups at 640x360): 16,128 patches = 1.59 GMAC (less the
  shared layer-0 halves, 3 %).
- vr 12/7 variant B (e306, 48 patches, 21 x 16 at 1120x630): 1.59 GMAC.
- So about 1.4-1.6 GMAC per frame whatever the ratio; the threadgroup overlap of one input pixel costs 2-3 %. At the
  NDVE kernels' 2.1-2.5 TMAC/s on an M2 Max: **0.6-0.8 ms** for the network, plus the epilogue (one bilinear grid
  lookup, one texture sample, a sqrt, three signed squares and, for A, three pow per output pixel: about 0.1 ms at
  1920x1080, I). The SMAA passes are small (about 0.2-0.5 ms at 1536x864, I). The translated chain's time is R.

## 12. Where it belongs (recommendation)

A core crate, not the family layer: **`nxkit-ndsr` (L1, std only)**, next to `nxkit-ndve`, with `nxkit-ndve` later
taking its resampler from it.

- NDSR is not agl code only: the NDVE movie file embeds an NDSR pack, and its resampler `4f41ccf24f163e0d` is an NDSR
  network (config 0x18067521, same 0x800-byte layers, same weight layout and layer arithmetic as agl_ndsr, section 14).
  The library is Nintendo SDK knowledge used by agl and by `nn::ndve`, so its kernels belong with the other SDK network.
- Detection is by content (program keys and weight hashes), as for NDVE. It needs no title fact, and `nxkit-gpu` must
  not depend on `titles/` (layers L1 vs T).
- What the family layer `titles/families/agl` should hold: the fact that agl games use NDSR (the `ShaderRole::Upscaler`
  hook, with its archive match fixed to the three `.pck` names and the evidence upgraded to checked), and, if ever
  needed, a runtime hook that says which pack a title uses or turns the native path off for a title. No kernels.

## 13. Unknown after this pass

Settled in this pass (V emulated): the HMMA chains, ReLU positions (no biases), the weight element order of vr and
agl_ndsr and the map between them, the input window and patch layout, the grid, the bilinear base sample, the unsharp
mask and its edge rule, the tile rule, the resample, the chroma reconstruction, the three output encodings, the hash
dither, the meaning of push words 2-9, 18, 19, and which neighbourhood-blending output the vr networks read.

Still open:

- **Runtime (section 10)**: which pack, variant and ratio run at which input size; the selection rule; all push values;
  the formats of tex0, img1, img2; the samplers' filter and address mode (the model assumes bilinear, clamp to edge);
  whether img2 has other readers.
- **Hardware arithmetic** (as for NDVE, not readable from the code): the rounding inside one HMMA (the model rounds the
  exact sum once), the MUFU `rcp/sqrt/lg2/ex2` error (the model uses correctly rounded values; Metal's precise functions
  are within a few ulp, the 8-bit output hides that except for rare pixels), the texture unit's bilinear weight
  precision (NVIDIA uses 8 fractional bits; the model exact weights).
- **Not emulated bit for bit yet**: the 22 agl_ndsr programs (structure and layout V; the LDSM/MOVM input staging,
  the epilogue and variant B's sRGB decode). The tools of this pass run them (`s3.py` already gives their chains); the
  model needs their input element and epilogue.
- The SMAA compute passes beyond their constants: the edge-list format, the indirect arguments, the meaning of the
  192x128 area texture (only needed if package B fuses or replaces them; stage 1 keeps them translated).
- The meaning of config byte `b3` and header word +0x10 byte 3.

## 14. Reusing the NDVE resampler kernel (`crates/nxkit-ndve/src/msl/resample.metal`, `kernels/`)

The NDVE resampler `4f41ccf2` is the same network family in the agl convention (section 8.3): `rs_block` computes
`H_g = W1_g X` (g = 0, 1), `Y_j = H_0 W2_0j + H_1 W2_1j` with an f16 rounding after every K = 16 step (`hstep`), then
`X = max(Y, 0)`, with weights `wb + 256 g + 128 ki + 64 mi` (W1) and `wb + 512 + 256 g + 128 j (+ 64)` (W2): exactly
the agl_ndsr layer layout and arithmetic, and an NDSR layer is one `rs_block` once the vr blob is repacked to the agl
layout (section 8.3).

Reusable as is:

- `common.metal`: `hstep`, `to_half`, `relu_half`, `zero_acc`, the `fp math_mode(safe)` rule (the epilogue's fma order
  matters here too).
- `rs_block` for layers 0-4 (the input `X[b][k tile][n tile]` with rows = picture rows, as NDSR in the agl
  convention), the weight staging into threadgroup memory (12 KB, the same 0x3000 bytes), the `RS_NB` blocks-per-
  simdgroup tiling idea and its register budget.
- From `kernels/mod.rs`: the plan shape (weights uploaded once as `init`, push block and textures bound from the game's
  dispatch) and the test pattern (kernel vs CPU reference, ignored GPU tests).

What differs (new code):

- Layer 5: NDSR's last layer is a full 16x16 layer without ReLU, and the result used is `Y[0..11][0..11]` (12x12);
  NDVE's `rs_last` computes only rows 0-7 and columns 0-7 (8x8). Needs an `rs_block` without the ReLU and a 12x12
  store.
- Input: one channel of tex0 (Y' for vr, f16 luma of sqrt RGB for light, f32 luma of the texel for agl) in a window of
  `npx x npy` patches at stride `in` from `(b0 wx - m, b1 wy - m)`, read with the game's sampler (vr TLD4 / TLD with
  clamp); NDVE reads a precomputed luma buffer at stride 6 from `(6 bx - 5, 6 by - 5)`.
- Output stage: NDVE adds the 8x8 delta to a picture sample and dithers with a texture. NDSR writes a 12x12 block of a
  threadgroup grid (`f16(s |s|)` with the f16 bilinear base sample at the cell centre, per-network cell-offset
  immediates), runs the unsharp pass over the grid, then resamples the grid bilinearly to an **arbitrary** output size
  (the tile rule of section 7.6), takes the sqrt, rebuilds R and B from a colour sample and G from Y', squares, encodes
  (A/B/C) and dithers with an integer hash. None of this exists in `resample.metal`.
- Weights: the vr blob must be repacked (fragment order and the even-layer transposes of section 8.3); agl_ndsr's and
  NDVE's need no permutation.
- Ratio and size: NDVE is fixed (1920x1080 -> 2560x1440, 4/3, 6 -> 8). NDSR has five vr models (in = 10, 8, 7, 6, 4,
  out = 12) and three tilings each, and the input and output sizes change every few frames.

### 14.1 Speed of the native kernel (`crates/nxkit-ndsr/src/msl/ndsr.metal`, measured)

Harness: `crates/nxkit-gpu/tests/ndsr_bench.rs` (`cargo test --release -p nxkit-gpu --test ndsr_bench -- --ignored
--nocapture`): each of the 15 kernels alone in its own command buffer on the full picture 1536x864 -> 1920x1080, the
real texture sampler, synthetic weights and picture; GPU start to end of the command buffer (`gpu_ns`), minimum of 25
runs, best of three process runs. Apple M2 Max (38 cores), other GPU load on the machine (a game build) makes single
runs noisy by 10 to 30 %. `NDSR_BENCH_SRC=a.metal,b.metal` times complete sources side by side, round-robin.

| kernel | patches | before ms | after ms | simdgroups x patches at once |
|---|---|---|---|---|
| in10_a / b / c | 12 / 12 / 16 | 1.17 / 1.00 / 0.97 | 0.68 / 0.65 / 0.74 | 12x1 / 12x1 / 16x1 |
| in8_a / b / c | 16 | 1.50 / 1.40 / 1.36 | 0.82 / 0.68 / 0.74 | 16x1 |
| in7_a / b / c | 16 / 48 / 24 | 1.90 / 1.40 / 1.46 | 0.86 / 0.84 / 0.80 | 16x1 / 8x1 / 12x2 |
| in6_a / b / c | 16 / 32 / 32 | 2.50 / 1.89 / 1.89 | 1.12 / 1.05 / 1.04 | 16x1 / 12x1 / 12x1 |
| in4_a / b / c | 48 | 4.05 / 3.88 / 3.90 | 2.29 / 2.30 / 2.29 | 8x3 |

What the numbers say:

- The network is not 1.5 GMAC for every scale: one patch is 6 layers x 4 products of 16x16x16 = 98 304 MAC, a picture
  has `(1536 / in) x (864 / in)` patches, so in = 10 / 8 / 7 / 6 / 4 cost 1.3 / 2.0 / 2.7 / 3.6 / 8.2 GMAC. At the
  5.4 TMAC/s the in4 network reaches here (the M2 Max peaks near 6.8 for f32 FMA) the network alone is 1.5 ms for in4,
  0.5 for in10. The rest of the in4 kernel is the cell samples (0.45 ms) and the window, staging and unsharp (0.45 ms).
- The old kernel was latency bound, not MAC bound: one patch a simdgroup, four simdgroups a threadgroup and one
  threadgroup resident per core (the grid takes up to 31 KB of the 32 KB), so every wait for the window, a cell sample
  or a barrier idled the core. The gain came from (1) more simdgroups per threadgroup (8 to 16, 256 to 512 threads),
  (2) the unsharp pass spread over all threads (a block of 6 columns by a few rows a thread, read into registers, one
  barrier, write) instead of a column a thread with a barrier per row, (3) the base samples of all the cells of a
  group requested together, before the epilogue stores and barriers, (4) the division-free pixel loop, (5) the
  precise `exp2` / `log2` of the sRGB encoding replaced by the fast ones (the game's MUFU.EX2 / LG2; variant A only,
  3 to 18 % of those kernels; `ndsr_kernel.rs` counts the same pixels equal and the same largest difference) and (6) several patches per simdgroup where it
  pays (`NB` = 3 for in4, 2 for in7_c; elsewhere one patch a simdgroup with more simdgroups was equal or better).
- Weights in threadgroup memory matter more than anything else tried: in4 with its weights read from the device
  buffer is 3.5 ms, with them staged 2.3 ms. in7_b (96 x 72 cells) has room for 3 of the 6 layers only (24.5 KB
  without them) and takes the other three from the device; it is the kernel furthest from its MAC floor.
- 12 or 16 simdgroups are not free: the kernels declare `max_total_threads_per_threadgroup`, which makes the compiler
  fit the registers to the threadgroup size; in4 and in6_b slow down at 12 or more simdgroups (registers), in10 and in8
  speed up to 16. The choice per kernel is `simdgroups` and `patch_group` of `KernelSpec`, mirrored in the
  `NDSR_KERNEL` lines (a test compares them).
- The arithmetic is unchanged: all 15 kernels write the same bits as before for variants B and C, and A differs only by
  the `exp2` / `log2` change; the f16 rounding after every K = 16 step and the W2-first order of layers 0, 2, 4 are as
  before. `ndsr_kernel.rs` passes with its tolerances as they were.

## 15. The vr_light network program (V emulated for all 15 programs unless marked)

Scratch tools: `scratchpad/ndsr/light/`. `refl.py` is the model. `cmpall.py <key> <ct>` compares one threadgroup of the
emulator (`CEmu`, all lanes) against the model on every image write. `batchl.py` runs the batch of 15.7. `ls3.py` prints
the HMMA chains. `ltr.py` gives a one-lane trace. `cmpl.py` compares the threadgroup-memory stages (grid, luma,
sharpening). `perm43.py` checks the 4/3 blob. `gen_golden_light.py` writes the fixture of `reference/light.rs`.

### 15.1 The programs

The pack holds 7 blobs x 3 entries. Where two entries share a program, the program is called by the letter of its
first entry. Variant A encodes sRGB. B (= the B and C entries) writes linear values. The 3.0 blob has a third program
(C, linear, f32 luma).

| Ratio | in -> out | tile `b0 b1` | grid = output tile | layers | A | B (and C) | C |
|---|---|---|---|---|---|---|---|
| 1.2 | 10 -> 12 | 40 40 | 48 x 48 | 6 | 9327ea56aafa15b9 | 8df148d58b7cbe8e | |
| 4/3 | 6 -> 8 | 48 48 / 48 24 | 64 x 64 / 64 x 32 | 3 | 927ceaf3cf1be240 (256 threads, TLD, f32 luma) | fa3033d05c0a41d6 | |
| 1.5 | 8 -> 12 | 32 32 | 48 x 48 | 6 | 48d00da84b3484df | aad7ea7e5c02f41d | |
| 12/7 | 7 -> 12 | 28 28 | 48 x 48 | 6 | 6160882513197af3 | b106afba1a18faa9 | |
| 2.0 | 6 -> 12 | 24 24 | 48 x 48 | 6 | 2f9bca45ef842a3b | 04135bb812a0d66e | |
| 2.4 | 5 -> 12 | 20 20 | 48 x 48 | 6 | d8fffc05afaaa981 | 7c511cc23ff85d14 | |
| 3.0 | 4 -> 12 | 16 16 | 48 x 48 | 6 | bdb298c6aa71a298 | c8b20adc9d4978e0 | d99fbeeb53d38847 (TLD, f32 luma) |

Patches per threadgroup: `npx = (b0 + 1) / in`, `npy = (b1 + 1) / in`, as in vr. That is 4 x 4 for all but 4/3 (8 x 4 and
8 x 8). The emulation confirms the shared-control-section rule of section 4 for `d8ff` and `c8b2`: their keys follow
the entry order. Program `d8ff` runs the 2.4 blob and matches the model.

### 15.2 Window and network

- Window texel `(i, j)` = `(b0 wx - m + i, b1 wy - m + j)`, with `m = (16 - in) / 2`, clamped to the picture. The
  `TLD4.PTP` programs gather R, G, B (f16) at `u = (x + 1) w8`, `v = (y + 1) w9` (push words 8, 9). The `TLD` programs
  (`927c`, `d99f`) clamp to `[0, w2] x [0, w3]` themselves.
- Input element, f16 programs: `X = hfma(G, 0x39b8, hfma(R, 0x32cd, hmul(B, 0x2c9e)))`, every step rounded to f16 (B
  first). `TLD` programs: `f16(fma(B, 0.0722f, fma(R, 0.2126f, G * 0.7152f)))`.
- Network: the vr network of section 7.3 on the vr weight layout of section 8.2. It has 6 layers, or 3 for the 4/3
  blob, with ReLU and a transpose between layers and no ReLU after the last. After 3 layers the result is transposed
  relative to the 6-layer case. The grid cell is `N[out p + r][out q + c] = Y[c][r]` (3 layers) or `Y[r][c]` (6). In the
  canonical layout of section 8.3 both are `Y[r][c]` with rows along y.
- The 4/3 programs compute only the 8 used columns in the last layer. The `MOVM`s of the 4/3 pair move the result
  fragments to the grid. They do not change values.
- **The 0x1800 blob** is in the vr layout. Repacked by the map of section 8.3 (odd layers verbatim, even layers
  transposed and swapped), it equals agl_ndsr's 4/3 blob (`0xdbf00`) in all 3072 halves (`perm43.py`). So the
  "unknown weight permutation" of the plan is the known one.

### 15.3 Output pixels (the grid is the tile)

A threadgroup writes the tile of `w18 x w19` pixels starting at `(w18 wx, w19 wy)`. Pixels at or beyond `(w24, w25)`
are not written. The written position adds `(w4, w5)`. Per pixel `(xl, yl)` of the tile, `(X, Y)` the picture
position:

```
gx = fma(float(xl), w20, w22);  ix = floor(gx);  fx = f16(gx - ix);  if ix > gw - 2: ix = gw - 2, fx = 1   (y alike)
lerp(a, b, f) = hfma(f, b, hfma(-f, a, a))                                             # f16
up  = lerp(lerp(N[iy][ix], N[iy][ix+1], fx), lerp(N[iy+1][ix], N[iy+1][ix+1], fx), fy)
S   = f16 RGB of the bilinear sample of tex0 at (fma(X, w26, 0.5 w26), fma(Y, w27, 0.5 w27))   # TEX .F16
t_c = hadd(S_c, up);   lin_c = hmul(t_c, |t_c|)                                        # each channel gets the luma delta
L   = hfma(lin_B, 0x2c9e, hfma(lin_R, 0x32cd, hmul(lin_G, 0x39b8)))                     # G first here
```

`N` is never sampled below cell 0 when `w22, w23 >= 0` (the game writes 0). For a negative origin the SASS would read
outside the grid (I). The Rust reference and the kernel clamp to cell 0 there.

### 15.4 Sharpening and encoding

With `k1 = f16(w28 * 0.5)` and `k2 = f16(w28 * -0.125)` (no `+1`: this is a delta), and `L` clamped to the **tile** at
its edges:

```
d   = hfma(L[y-1][x], k2, hadd(hfma(L[y][x-1], k2, hmul(L[y][x+1], k2)), hfma(L[y][x], k1, hmul(L[y+1][x], k2))))
o_c = f32(hadd(lin_c, d))                       # HADD2.F32: -0 becomes +0
A:  o_c = the sRGB encoding of section 7.7 (FSETP.GEU, MUFU lg2 / ex2);  B, C: o_c as is
```

The image write gets `(o_r, o_g, o_b, 1.0)`. There is **no dither** in any light program. Push words read: 4, 5, 8, 9
(f16 programs) or 2, 3 (`TLD` programs), and 18 to 28. Words 6, 7 and 10 to 17 are not read.

### 15.5 What was verified, and how

- The network grid `N` (threadgroup memory after the last HMMA), the luma `L` and the sharpening delta `d` were each
  compared separately on `fa30` (`cmpl.py`), then every image write of every program (`cmpall.py`).
- `batchl.py` runs each of the 15 programs on 12 threadgroups (the first, the last and an inner one). There are four
  set-ups: 1536x864 with the game's push (exact ratio, strength 2.5, random texture); 1280x720 with `w20, w21 = 0.97,
  0.95`, `w22, w23 = 0.4, 0.7`, offset (5, 2), strength 1.0 (smooth texture); 960x540 with `w22 = 0.25`, strength 0.5;
  763x429 (partial last tiles) with offset (3, 0). Result: **0 differing pixels for all 15 programs** (21,618 to 43,996
  pixels each, all four channels as bits).
- `crates/nxkit-ndsr/src/reference/light.rs` ports the model. Its test reproduces 20 golden threadgroups of `refl.py`
  (5 programs, the grid and every pixel by digest).
- What the emulation cannot verify: the same unknowns as section 7.10 (HMMA rounding, MUFU, sampler precision), and
  the f16 `HFMA2` rounding, modelled as the exact value rounded to a double and then to f16.

### 15.6 The native kernel (`crates/nxkit-ndsr/src/msl/ndsr_light.metal`, `kernels/light.rs`)

There is one template, `ndsr_light`, with 15 instantiations (one per program, `LIGHT_KERNELS`). Steps:

1. The window's luma goes to threadgroup memory.
2. The network runs on NB patches per simdgroup, with the vr layer order of `ndsr.metal` (`ndsr_step` gained the layer
   count) and the weights staged in threadgroup memory. The result goes straight into the grid (8x8 tiles for 4/3).
3. Each thread keeps the linear colour of its pixels in registers and writes their luma over the dead window.
4. After one barrier come the sharpening, the encoding and the write.

The plan refuses a push block whose tile (`w18, w19`) is not the kernel's grid. The hook (`native_ndsr.rs`) runs the
vr_light programs that have a kernel. It accepts RGB9E5 / R11G11B10F / float4 input, and for B and C an R11G11B10F,
RGB10A2 or RGBA16F output. It tags plans by words 2 to 9 and 18 to 28.

The first GPU comparison of `ndsr_light_r12_a` (`ndsr_light_kernel.rs`) gave these results:
- **Exact inputs:** 0 pixels beyond 4e-3, largest difference 4.8e-7. That is the fast `exp2` / `log2` of the sRGB
  encode; the f16 epilogue gives the same bits.
- **Random weights:** 3.7 % of the pixels beyond 4e-3, largest 0.050. That is the f32 accumulation inside
  `simdgroup_multiply_accumulate` rounding a K = 16 sum to the other f16 neighbour, as in the vr kernels. The f16
  epilogue and the sharpening (strength 2.5) spread it further.

A CPU model of the same threadgroup reproduces this (`light/flip2.py`: every HMMA as sequential f32 fmas): 2.7 %
beyond 4e-3, largest 0.025, and none on exact inputs. The comparison test now accepts this: 1e-5 counts as equal, and
on random weights it allows 5 % of the pixels beyond 4e-3 and 0.125 at most. It prints every kernel before it fails.
The second run checked every kernel. On exact inputs `r127`, `r24` and `r3` differ in 0.4 to 1 % of the pixels: a
few non-exact sums of the six layers round differently, and those differences are not kernel errors. The CPU model
of `r24_b`'s whole picture (`light/fullflip.py`) gives 93 pixels not equal and largest 0.0039; the GPU gave 92 and
0.0039. On `r12_b` both give none. The test now allows up to 1 % of the pixels not equal on exact inputs (0.2 %
beyond 4e-3, 0.0625 at most), and 0.25 at most on random weights, where variant A's sRGB slope magnifies the dark
pixels. The first timing at 1536x864 -> 2048x1152 for Ocarina's `ndsr_light_r43_b` was 0.49 to 0.67 ms (minimum of 25).
