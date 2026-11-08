# nn::ndve movie network: what it is

Game: Zelda Ocarina of Time (0400486024DEC000). File: `Movie/Ndve/Ndve-Nvn2-vid_seq5.dat.zs`.

Claim marks:

- **V** = verified from file bytes or shader code. "Emulated" means: I ran the SASS of the shader in a lane-exact Python emulator (scratchpad only, not in the repo) with symbolic memory, and read the HMMA operand addresses and store addresses from it.
- **I** = inferred, or taken from nxnative PR #1837 and not re-derived.
- **R** = only answerable at runtime (see section 10).

Scratch tools (not in the repo): `/private/tmp/claude-501/-Users-nzaljic-repos-me-nxkit-rt/2bd3ad49-183b-493a-8d88-aa4b15fad0cc/scratchpad/ndve/` (`ndve.dat`, `emu.py`, `rep2.py`, `chains.py`, `lst.py`).

## 1. Summary

The network is a U-Net over the movie picture packed 2x2 into 16 channels. It has 12 convolution layers with 16, 32 and 64 channels, all in f16 NHWC, all as HMMA.16816.F16. Layers 1-11 end in `relu(acc + bias)`. Layer 12 has no ReLU and adds the converted RGBA8 picture back. A small 6-block HMMA network then resamples 1920x1080 to 2560x1440 from the luma channel (a learned delta added to a bilinear sample of the picture).

About 10.6 GMAC per frame (V, from the shapes): layers 4+9 and 6+7 are 1.19 GMAC each, which matches the measured 5.6 ms and 5.7 ms for the two shader pairs.

## 2. The file (V)

zstd frame with dictionary id 1 (the first member of `romfs/Pack/ZsDic.pack.zs`, a SARC; members are 0x20000 bytes each at pack data offset 0xa0). Decompressed: 2,038,760 bytes, SHA-256 `27f940cb2f52b8f8df822b686a2fd4f938d8df4fca9cbf68b0b4bfeeac3e1399`.

| Offset | Content |
|---|---|
| 0x00 | `NDVE`; u32 0x0ff00005; u32 0; u32 0x106000 (NDSR pack length); u32 0xebbc0 (bytes after the pack); u32 2 (model count) |
| 0x18 | 2 model records of 8 bytes: (0x01000000, f32 1.3333 = 4/3), (0x01000004, f32 1.0) |
| 0x28 | NDSR pack, 9 entries (see below) |
| 0x106028 | Network A table: u32 9, u32 1, u32 0xa0, u32 0x3c320 (weight bytes), then 9 x (ctl offset, ctl length, code length, 0) |
| 0x1060c8 | Network A weights and biases, 0x3c320 bytes, SHA-256 `50bac9b08e056e16d297353113368b38acdda167749a094a6db88ecc31dfc023` |
| 0x1423e8 | Network A: 9 compute shaders (NVN2 control section followed by code) |
| 0x189558 | Network B table (same format), weights at 0x1895f8, 9 shaders from 0x1c5918 |
| 0x1efe90 | colour conversion `49dff010b9832251` |
| 0x1f0d10 | colour conversion `8e98d86c0e4a7b53` |
| 0x1f1ba8 | 0x40 trailer bytes |

NDSR pack entries (offsets relative to pack start 0x28): 12-byte constant at 0x1050c0; a 1 MiB blob at 0x200 (SHA-256 `b9ecdfb7...3c80`, high entropy, purpose unknown); shader `c2fc2ddd604c25e7` (code 0x300); shader `4f41ccf24f163e0d` (code 0x1a00 at 0x100500, control at 0x104fe0); resampler weights 0x3000 bytes at 0x101f00 (file 0x101f28, SHA-256 `44af61fd...1dab`); 4-byte entries at 0x1050cc and 0x1050d0.

Network A shader file offsets: ba079f68 0x1423e8, a8b4fb48 0x149558, e2eebd7b 0x14dbc8, e6be82d8 0x1502e0, 9442573e 0x15d380, 0a56002c 0x168fb8, d15370a3 0x16f270, 7f7ff0f3 0x1810f0, 5a7d69b0 0x183148.

Control section fields used (V): +0x38 instruction bytes, +0x58/0x5c/0x60 workgroup size, +0x64 shared bytes, +0xa9 push words (x4 bytes), +0xb1/0xb2/0xb3 count of UBOs / SSBOs / textures. SSBO i sits at c0 offset `0x10 + pushbytes(aligned 16) + 0x10*i`, so every layer shader sees: ssbo0 = output tensor, ssbo1 = input tensor, ssbo2 = weights, ssbo3 = bias, ssbo4 = skip tensor (up-convolutions only). Verified for all 9 shaders by emulation (buffer ids of every LDG and STG).

## 3. Networks A and B, and the second converter (V static, use R)

- Network A weight blob and network B weight blob are **byte-identical** (V). Layer shapes and the 9 shader roles are the same.
- B's shaders are other tilings of the same layers: HMMA counts per shader A / B: e6be82d8 576 / 288, 9442573e 464 / 64, d15370a3 768 / 64, 5a7d69b0 108 / 72 (SUST 12 / 8); the other five have equal counts and local sizes but not equal code bytes.
- Model records: kind 0x01000000 with scale 4/3 (A), kind 0x01000004 with scale 1.0 (B). Scale 1.0 has no resampling step in the measured sequence, so B is the same-size path (I).
- Nothing in the file says when B or `8e98d86c` runs (R). `8e98d86c`: local 16x8, 3 textures, 64 push bytes, 4 TLD and 5 SUST (against 49dff010: 16x2, 60 push bytes, 4 TEX, 4 TLD, 4 SUST). It is another conversion variant (input or output format), not seen in the measured frame.
- The game dispatches only A's keys and `49dff010`, `4f41ccf2` (task statement). 

## 4. Graph, per dispatch

Weights offsets are from the weight blob start. Every weight tile is f16 `[tap][cin][cout]`, every bias f16, padded to 0x100 bytes (the last, layer 12, is 0x20). All of this is V: HMMA B addresses read in emulation have row stride = `cout * 2` bytes, tile bases `tap * cin * cout * 2 + kstep * 16 * cout * 2 + nblock * 16`, and the byte sizes add up to 0x3c320.

| Pass | Key | TGs | Threads | Layer | Op | In -> out (pixels x channels) | Weights | Bias |
|---|---|---|---|---|---|---|---|---|
| 3 | 49dff010 | 60x270 | 16x2 | - | NV12 -> RGBA8 | Y, CbCr -> 1920x1080x4 | - | - |
| 4 | ba079f68 | 900 | 32 | 1 | 3x3 s1 | RGBA8 texture (space-to-depth) -> 960x540x16 | 0x0 | 0x1200 |
| 5 | a8b4fb48 | 720 | 32 | 2 | 3x3 s1 | 960x540x16 -> 960x540x16 (S16) | 0x1300 | 0x2500 |
| 6 | e2eebd7b | 97 | 64 | 3 | 2x2 s2 | 960x540x16 -> 480x270x32 | 0x2600 | 0x3600 |
| 7 | e6be82d8 | 120 | 32 | 4 | 3x3 s1 | 480x270x32 -> 480x270x32 (S32) | 0x3700 | 0x7f00 |
| 8 | 9442573e | 70 | 128 | 5 | 2x2 s2 | 480x270x32 -> 240x135x64 | 0x8000 | 0xc000 |
| 9 | 0a56002c | 60 | 128 | 6 | 3x3 s1 | 240x135x64 -> same | 0xc100 | 0x1e100 |
| 10 | 0a56002c | 60 | 128 | 7 | 3x3 s1 | same | 0x1e200 | 0x30200 |
| 11 | d15370a3 | 85 | 64 | 8 | 2x2 s2 up | 240x135x64 -> 480x270x32, skip = S32 | 0x30300 | 0x34300 |
| 12 | e6be82d8 | 120 | 32 | 9 | 3x3 s1 | 480x270x32 -> same | 0x34400 | 0x38c00 |
| 13 | 7f7ff0f3 | 8100 | 32 | 10 | 2x2 s2 up | 480x270x32 -> 960x540x16, skip = S16 | 0x38d00 | 0x39d00 |
| 14 | a8b4fb48 | 720 | 32 | 11 | 3x3 s1 | 960x540x16 -> same | 0x39e00 | 0x3b000 |
| 15 | 5a7d69b0 | 5400 | 32 | 12 | 3x3 s1, no ReLU, + picture | 960x540x16 -> 1920x1080 RGB10A2 | 0x3b100 | 0x3c300 |
| 16 | 4f41ccf2 | 80x180 | 128 | - | resampler | RGB10A2 1920x1080 -> RGBA8 2560x1440 | pack 0x101f00 | - |

What is V and what is I here:

- Layer type, kernel size, stride, channel counts, weight and bias offsets: V (emulated chains and HMMA counts: 3x3 16->16 = 9 taps x 1 K-step, 32->32 = 9 x 2, 64->64 = 9 x 4, 2x2 down = 4 taps x cin/16, 2x2 up = 4 taps x cin/16 per pixel parity).
- Image sizes (960x540 etc.): V from address arithmetic where checked (pitches 960 px x 32 B in layers 2/11/12, 480 px x 64 B in layer 4/9, pitch of 30720 B); the 240x135 level from threadgroup counts and 9442573e's input pitch.
- Which dispatch's output feeds which input and which tensor is the skip: I. Strong support: skip pixel size in d15370a3 is 64 B (= 32 ch, S32 level) and in 7f7ff0f3 is 32 B (= 16 ch, S16 level). R: bound buffer addresses per dispatch.
- Tile per threadgroup (V where emulated): 5a7d69b0 = 16x6 px (`ct = tx + 60*ty`, checked at ct=5399: x0 = 944, y0 = 534); 7f7ff0f3 = 16 input px x 1 row (`ct = tx + 30*y`); e2eebd7b/e6be82d8/9442573e/0a56002c/d15370a3 run 2-4 warps. ba079f68 (72 HMMAs per loop iteration) and a8b4fb48 (36 per iteration) loop inside the threadgroup; their tg-to-tile map was not recovered (not needed for a reimplementation).

## 5. Verified layer semantics (by emulation of the SASS)

**3x3 stride 1 (layers 1, 2, 4, 6, 7, 9, 11, 12).** Output pixel (x,y), channel n:

`acc = sum over t = 0..8, kb over cin/16 blocks, of  in(x + (t%3) - 1, y + (t/3) - 1)[16 kb .. 16 kb + 15] . W[t][16 kb ..][n]`, accumulated as an HMMA chain in tap-major, K-block-minor order, starting from C = 0.

- Tap order `t = 3*dy' + dx'` with dy', dx' in {0,1,2} meaning offsets -1,0,+1 (V: A row base advances 1 pixel per tap, one image row after three taps).
- Weight element order `[t][cin][cout]`, cout fastest, f16 (V).
- A operand row r = output pixel x0 + r. Channels = K index. A is stored NHWC, pitch `W * C * 2` bytes (V), **no border**.
- **Padding is zero on all four sides** (V): out-of-image taps give a zero A row. Left/top shown at ct=100 for a8b4fb48, right/bottom at ct=5399 for 5a7d69b0 (taps with dx'=2 are zero in row 15 at x=960; taps with dy'=2 are zero for the last row). The mask is `IMAD Rx = predicate(0/1) * loaded word`, so the padding is exactly +0.0 halves.

**2x2 stride 2 down (layers 3, 5).** `t = 2*dy + dx`; input pixel (2x+dx, 2y+dy); weights `[t][cin][cout]`; chain over taps then K-blocks; no padding (V: e2eebd7b taps at +0, +32 B, +30720 B, +30752 B of a 32 B/px tensor; 9442573e likewise with 64 B/px).

**2x2 stride 2 up (layers 8, 10).** For input pixel (x,y), tap t = 2*dy + dx gives output pixel (2x+dx, 2y+dy). The HMMA chain starts with **C = the skip tensor value at that output pixel** (loaded from ssbo4 straight into the accumulator registers, f16), then adds cin/16 K-steps (V: 7f7ff0f3 chains C0 = skip base +0x0, +0x20 B, +30720 B, +30752 B for t = 0..3; B base `t * 32 * 16 * 2`; stores go to ssbo0 at the same offsets as the skip reads). d15370a3 identical with cin 64.

**Epilogue layers 1-11 (V).** Per output element: `HFMA2.RELU (D * 1.0 + bias)` in f16x2: one f16 rounding of `D + bias`, then max with 0 (HFMA2 bit 79, constant 0x3c003c00 for the multiplier). Result f16, written with 32-bit STG (two channels per lane) to ssbo0, NHWC.

**Accumulator precision (V for the type, I for internals).** Every HMMA is `.16816.F16`: A, B, C and D are f16; D is rounded to f16 after each HMMA (K = 16), including between chain steps. The internal precision of one HMMA (order, truncation of the 16 products plus C) is a hardware property and cannot be read from the code (R only through hardware; see section 11).

## 6. Pack and unpack steps (V)

**Layer 1 input (space-to-depth, 2x2 -> 16 ch).** ba079f68 reads the RGBA8 picture with TLD (integer coordinates) and converts the float with `F2FP.PACK_AB` (round to nearest f16). Packed pixel (X,Y) channel `k = 4*(2*sy + sx) + c` is picture texel (2X + sx, 2Y + sy), component c = R,G,B,A (emulated A fragments). The tap shifts are in packed-pixel units. The texel read is c/255 as float (RGBA8 unorm), then rounded to f16.

**Layer 12 output (depth-to-space, 16 -> 2x2 x 4).** 5a7d69b0 produces, for output pixel (2X + sx, 2Y + sy) with `s = 2*sy + sx`, component c=0,1,2,3 from network channel `n = 2s, 2s+1, 8+2s, 9+2s`. Per component, in f16:

`out_c = ((f16(picture_c) + D_n) + bias_n) + (-0.0)`, then converted to f32 with `HADD2.F32` and written with SUST (formatted store, mask 0xF = RGBA) to a 1920x1080 image. The picture is the same texel read with TLD (RGBA8 at the output pixel), rounded to f16 with F2FP.

Differences to PR #1837: the PR sums in f32 (`(t + d) + bias` with `t` unrounded). The SASS does two f16 additions with `t` rounded to f16 first (V from the emulated expression tree). The last bias values are all +-0.0 in this file (V), so only the first difference matters.

**Image write (I).** RGB10A2 unorm conversion happens in the SUST / Metal image write (clamp to [0,1], round to 10 bits; alpha 2 bits). The format of the image is from PR #1837 (R to confirm).

## 7. Colour conversion 49dff010 (V from MSL and SASS)

- Threads 16x2, TG covers 32x4 px; each thread does a 2x2 pixel block. Grid 60x270 TGs x 2x2 = 1920x1080.
- Y plane (texture A, tex register 1): integer `read(x, y).x`. CbCr plane (texture C, register 2): **bilinear `sample` with a sampler** at `uv = ((x + 0.5) / W, (y + 0.5) / H)` where W, H are the Y plane size (so chroma is upsampled by the sampler; sampler addressing mode comes from the game, R).
- Push constants (bank 24, 60 bytes, float4 rows of the 15 words): rows 0-2 = matrix (words 0,1,2 / 4,5,6 / 8,9,10), row 3 = offsets (words 12,13,14). Words 3, 7, 11, 15 are not read.
- With `Y' = Y + w12`, `U' = Cb + w13`, `V' = Cr + w14` (Cb = sample.x, Cr = sample.y):
  - `r = fma(V', w2, fma(Y', w0, U' * w1))`
  - `g = fma(V', w6, fma(Y', w4, U' * w5))`
  - `b = fma(V', w10, fma(Y', w8, U' * w9))`
  - all f32, in this order; result `write(float4(r, g, b, 1.0))` to an RGBA8 image (quantized by the image write).
- The translator also multiplies coordinates by `support_buffer._m5[..]` scale factors (identity at 1:1); not part of the game's code.
- R: the 12 float values (expected a BT.709-style limited-range matrix and offsets about -16/255, -128/255, -128/255; not verified).

## 8. Resampler 4f41ccf2 (V structure, I window mapping)

- 128 threads (4 warps) per TG; one warp = one 8x8 output block; 46 HMMAs per warp (V).
- Input: the 1920x1080 RGB10A2 picture (texture register 0, `read`, integer coordinates) and a dither texture (texture register 3, read with coordinates masked by `& 0x1ff`, V). Output: 2560x1440 RGBA8 image (image register 1).
- Luma (V): `Y = fma(B, 0.0722, fma(R, 0.2126, G * 0.7152))` in f32 (constants 0x3d93dd98, 0x3e59b3d0, 0x3f371759), rounded to f16 (`F2FP`) as the HMMA B operand.
- Network (V, emulated warp 0): 6 blocks, weights 0x800 bytes each at `block * 0x800`:
  - stage 1: `H_g = W1_g . X` for g = 0,1; W1_g is a 16x16 f16 matrix stored as four 8x8 tiles, tile order `[kb][rb]` (offsets +0x0, +0x80, +0x100, +0x180 within the 0x200-byte W1_g at +0x0 and +0x200);
  - stage 2: `Y_j = H_0 . W2_0j + H_1 . W2_1j` for j = 0,1 (a chain of two HMMAs; the D of stage 1 feeds the A operand of stage 2 without leaving the registers); W2_gj are 16x8 tiles of two 8x8 sub-tiles at `+0x400 + 0x200 g + 0x100 j`;
  - between blocks: `X = max(Y, 0)` in f16 (`HMNMX2`);
  - last block (block 5): only g-stage and j = 0 (6 HMMAs): `Y` is the 8x8 delta, **no ReLU**.
  - all accumulation f16, chain start C = 0.
- Epilogue (V from MSL): `sample` of the picture at the output pixel centre with a linear sampler (`uv = ((x + 0.5) * w10, (y + 0.5) * w11)`), the TEX unit returns f16 (V: TEX has the f16-result bit set); `v = (-0.0) + (s + delta)` per channel; when `w20 != 0`: `k = rint(fma(v, 255.0, dither*2 - 1))`, `out = k * (1/255 as 0x3b808081)`; the same delta is added to R, G and B; alpha = 1.0; the row is flipped when `w19 != 0` (`y_out = w18 - y`).
- Push words read (V from MSL): w2, w3 (int max x, y of the luma window, 1919 and 1079), w10, w11 (float 1/2560 and 1/1440 in PR's runtime checks), w18, w19 (flip base and enable), w20 (quantize and dither enable). Words 0, 1, 4-9, 12-17 are never read by the shader.
- Window mapping (I, from PR #1837): luma window 16x16 at `(6 bx - 5 .. , 6 by - 5 ..)` clamped to `[0, w2] x [0, w3]`, output block (bx, by) covers pixels `8 bx .. 8 bx + 7`. My emulation of the window coordinates did not converge (the lane-to-texel map of its TLD and shuffle code showed implausible coordinates), so this stays I. The PR's CPU test gives the same arithmetic order.
- The weights are 6144 f16 values (range -3.25 to 3.35).

## 9. HMMA patterns for the translator (V, counts per shader, whole shader)

All HMMAs are m16n8k16 f16. Fragment layout (V, consistent with every emulated address fit): lane l, g = l >> 2, t = l & 3. A regs 0..3 = (row g, k 2t..2t+1), (row g+8, k 2t), (row g, k 2t+8), (row g+8, k 2t+8); B regs 0..1 = (k 2t.. 2t+1, n g), (k 2t+8.., n g); C/D regs 0..1 = (row g, n 2t..2t+1), (row g+8, n 2t..).

| Shader | HMMA | LDSM x4 | LDSM x4.T | LDSM x2.T | LDS | STS | LDG | STG | BAR | other |
|---|---|---|---|---|---|---|---|---|---|---|
| ba079f68 | 72 | 36 | 0 | 18 | 0 | 37 | 20 | 16 | 0 | F2FP 38, TLD 19 |
| a8b4fb48 | 36 | 18 | 0 | 18 | 0 | 26 | 28 | 8 | 0 | |
| e2eebd7b | 32 | 16 | 4 | 0 | 0 | 12 | 14 | 16 | 8 | |
| e6be82d8 | 576 | 144 | 36 | 0 | 0 | 67 | 71 | 64 | 0 | |
| 9442573e | 464 | 232 | 8 | 0 | 0 | 66 | 68 | 116 | 58 | |
| 0a56002c | 144 | 72 | 36 | 0 | 33 | 75 | 46 | 8 | 5 | |
| d15370a3 | 768 | 96 | 16 | 0 | 46 | 66 | 450 | 384 | 48 | |
| 7f7ff0f3 | 16 | 2 | 8 | 0 | 0 | 10 | 28 | 16 | 0 | |
| 5a7d69b0 | 108 | 54 | 9 | 0 | 0 | 23 | 25 | 0 | 0 | F2FP 24, TLD 12, SUST 12, HADD2 96 |
| 4f41ccf2 | 46 | 12 | 12 | 0 | 4 | 27 | 24 | 0 | 12 | MOVM 24, SHFL 1, HMNMX2 20, TLD 8, TEX 2, SUST 2 |

How operands get into registers (V):

- **A (activation, 3x3/2x2 layers):** `LDG.128 -> (IMAD mask by 0/1 for padding) -> STS.128 -> LDSM.x4` (non-transposed). Four row-address vectors per LDSM: lanes 0-15 give the 16 pixel rows (shared pitch 48 B for 16-channel input, i.e. padded), lanes 16-31 the same rows +16 B (K 8-15). One LDSM.x4 yields the four A registers of one HMMA in order. Several HMMAs reuse an A tile (once per N block), no re-load.
- **B (weights):** `LDG.128 -> STS -> LDSM.x4.T` (or `.x2.T` in the 16-channel layers). One x4.T gives two HMMAs' B pairs. The transpose makes the row-major `[cin][cout]` weights into the B fragment. Weights are staged at the start of the shader (all 8 x4.T loads before the first HMMA in 7f7ff0f3).
- **C:** `RZ` at chain start in conv/down layers; the previous D inside a chain (D registers feed C directly, no MOVM); in up layers the C registers come from `LDG` (skip tensor) moved with `IMAD.MOV`.
- **D -> A:** only in the resampler (stage 1 D registers are the stage 2 A operand directly, 22 of 46 calls).
- **Between HMMAs:** nothing for chain steps. After the last HMMA of a chain: `HFMA2.RELU` on both D registers (bias in registers, loaded by `LDG` once), then `STG.32` per register. Final layer: `F2FP` + `HADD2` + `HADD2.F32` + `SUST`. Resampler: `HMNMX2` between blocks.
- **Per-lane dependence:** every address is lane-id arithmetic (`g`, `t`) via the LDSM row addresses and the STG addresses (lane writes channels `2t`, `2t+1` of row `g` and row `g+8`). Epilogue bias is indexed by `n = 8 j + 2t`. Fragment data are never shuffled except in the resampler: 24 `MOVM` (8x8 b16 transpose) and 1 `SHFL` there. No other shader uses SHFL, MOVM or any lane-dependent operation on fragment registers. The translator's current tensor pass sees both MOVM and SHFL only in `4f41ccf2`.
- 7f7ff0f3 and d15370a3 must keep C in f16 registers loaded from memory; the pass that turns C into an accumulator slot must read `LDG` data (halves) as the initial accumulator.

## 10. What to capture at runtime (R)

1. File identity: SHA-256 of the decompressed `.dat` (or of the weight blob at 0x1060c8, length 0x3c320) and the shader key sequence in section 4.
2. Per dispatch 4-16: the bound ssbo1 (input), ssbo0 (output), ssbo4 (skip) base addresses, to prove which output is which skip (expected S16 = output of pass 5, S32 = output of pass 7), and ssbo2/ssbo3 offsets from the blob start (expected the table in section 4). For pass 9/10 and 7/12 the same shader runs twice with different bindings.
3. Dispatch 3 push constants (bank 24 words 0-2, 4-6, 8-10, 12-14) and the Y/CbCr texture formats and the sampler of the CbCr plane.
4. Dispatch 16 push constants (all 21 words; expected w2 = 1919, w3 = 1079, w10/w11 floats, w18/w19, w20), the dither texture (size, format, contents) and the sampler of the picture.
5. The format of the image written by pass 15 and read by pass 16 (expected RGB10A2).
6. A reference frame pair (input NV12, output RGBA8 2560x1440) from real hardware, if ever available, to settle the HMMA internal rounding. Not obtainable from the Mac.
7. Whether network B keys (`329e4b0f0b3f2bc6`, `7653fe5f6ef0b2f2`, `b6e34b46dea44278`, `3f18f89630c05813`, `539ad3b353758de0`, `d72077c4b6cc264f`, `ab9a92b55950ec02`, `c41f451b88a050f6`, `55ee6dc89a2b64e2`) or `8e98d86c0e4a7b53` ever appear; log the dispatch keys of every movie once.

## 11. Fingerprint (V)

A game runs exactly this network if all of these hold:

- decompressed file SHA-256 above, size 2,038,760; header words 0x0ff00005, 0x106000, 0xebbc0, 2; model records (0x01000000, 4/3), (0x01000004, 1.0);
- weight blob length 0x3c320, SHA-256 `50bac9b0...c023` (copy 1 at 0x1060c8, copy 2 at 0x1895f8, equal);
- shader keys of section 4 in order (pass 3 to 16), network A weights offsets per dispatch;
- resampler weights SHA-256 `44af61fd...1dab`;
- picture sizes 1920x1080 RGBA8 in, RGB10A2 1920x1080, RGBA8 2560x1440 out (PR #1837 checks the same).

## 12. What this implies (short)

- Weights and biases are in the game's file as plain f16; no decoding step. Layer table is fixed.
- A native version needs per layer: conv with zero padding, tap order `[t][cin][cout]`, f16 chain per HMMA with rounding to f16 after each K=16 step, `relu(f16(D + bias))`; skip as initial accumulator; 2x2 space-to-depth and depth-to-space as in section 6; final sum in f16 with the order of section 6; resampler MLP as in section 8.
- The bit-faithful part that the code cannot tell us is the rounding inside one HMMA (section 5, last paragraph). The current translator accumulates a whole chain in f32 and rounds once at the end; the hardware rounds to f16 after every HMMA, so both differ from each other in the low bits.

## 13. Unknown after this pass

- Resampler luma window mapping (only the PR); HMMA internal rounding; contents and role of the 1 MiB pack blob; when network B and `8e98d86c` are used; all push-constant values; sampler states; texture and image formats (I from PR); tile-to-threadgroup mapping of ba079f68 and a8b4fb48 (loop inside the threadgroup).
- Emulator coverage: all 9 network A shaders and 4f41ccf2 ran; 49dff010 was read from MSL, not emulated.
