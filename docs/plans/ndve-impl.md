# nn::ndve natively: implementation plan

The network is described in [ndve-native.md](ndve-native.md). This plan says how nxkit runs it with its own Metal kernels
instead of the game's translated tensor shaders, and how we know the result is right.

Goal: the movie network in at most 6 ms of GPU time a frame on an M2 Max (now about 16 ms translated), with the same
picture as the translated chain within the limits of section 5.

## 1. Shape of the solution

- **`nxkit-ndve` (L1, pure Rust, no I/O, no Metal).** It knows the network: the layer table, the fingerprint, the weight
  packing, the Metal kernels as MSL text, the native passes as data ([`plan::Plan`](../../crates/nxkit-ndve/src/plan.rs)) and a
  CPU reference. It is SDK knowledge, so it sits in the core, not in `titles/`.
- **`nxkit-gpu` runs it.** When the dispatches of a frame contain the network's key sequence, and the weights bound to
  them pass the fingerprint, the encoder encodes the plan in place of that run of dispatches. Everything else stays as it is.
  If anything does not match, the game's dispatches run as today. `NXKIT_NDVE=0` turns the native path off.
- **No game file is read.** The game uploads the weight blob into GPU memory and binds it (ssbo 2 and 3 of every layer,
  the resampler's weight buffer). `nxkit-gpu` reads those bytes from guest memory through the bindings of the matched
  dispatches and hands them to `nxkit-ndve`. The fingerprint is computed over the same bytes.
- **No game code is patched.** The game still records and submits its dispatches; only the encoder chooses other work for them.

## 2. Work packages

They are cut so that no two touch the same files. The crate skeleton and the shared types in `plan.rs` are fixed; a
change to `plan.rs` needs the agreement of packages B and C.

### A. Network, fingerprint, reference (`nxkit-ndve`: `net.rs`, `fingerprint.rs`, `file.rs`, `half.rs`, `reference.rs`)

- `net.rs`: the layer table of section 4 of ndve-native.md as constants: kind (3x3 s1, 2x2 s2 down, 2x2 s2 up, final),
  input and output size and channels, weight and bias offsets in the blob, ReLU, skip source.
- `fingerprint.rs`: the game's key sequence (passes 3 to 16), the blob length 0x3c320 and SHA-256, the resampler weight
  length 0x3000 and SHA-256. `fn matches(keys: &[&str]) -> Option<Match>` finds the run in a frame's dispatch keys;
  `fn weights_ok(blob: &[u8], resampler: &[u8]) -> bool`. SHA-256 from `nxb-model`.
- `file.rs`: parses the decompressed `.dat` (header, model records, network tables). Only for tests and tools: it lets a
  test take the real weights from a file named by `NXKIT_NDVE_DAT` and checks the offsets of ndve-native.md section 2.
- `half.rs`: f16 from and to f32, round to nearest even, with subnormals and signed zeros.
- `reference.rs`: the whole chain on the CPU, from NV12 planes and the two push blocks to the 2560x1440 RGBA8 output,
  with the arithmetic of ndve-native.md sections 5 to 8: f16 tensors NHWC; per HMMA step of K = 16 the 16 products and C
  summed in f32 in a fixed order, then rounded to f16; `relu(f16(D + bias))`; zero padding; the final f16 sums in the
  order of section 6; the resampler with the window of section 8. It is slow and that is fine. It also exposes each
  layer on its own, so a kernel can be checked one layer at a time.
- Tests: the f16 conversion against known values; one layer on a small tensor against a hand-computed result; the full
  chain on a 64x32 crop is deterministic; with `NXKIT_NDVE_DAT` set, the real file parses and passes the fingerprint.

### B. Kernels and packing (`nxkit-ndve`: `msl.rs`, `pack.rs`, `kernels.rs` building the [`Plan`])

- `pack.rs`: the weights of each layer reordered for `simdgroup_matrix` 8x8 tiles in the order the kernels read them,
  biases as f16 vectors. A pure function of the blob, run once.
- `msl.rs`: the kernels as one MSL source. One kernel per layer kind, specialised by function constants (channels,
  ReLU, skip, stride), plus the colour conversion with the space-to-depth packing fused into layer 1, the depth-to-space
  and the final sum fused into layer 12, and the resampler.
  - Activations NHWC f16 in device buffers, as in the game. Each threadgroup computes a tile of output pixels for all
    output channels as an implicit GEMM: A = input pixels x (taps x cin), B = packed weights, on `simdgroup_half8x8`
    operands with `simdgroup_float8x8` accumulators.
  - Rounding: after every K = 16 step the accumulator is rounded to f16 (through `thread_elements()`), as the hardware
    does after every HMMA. Bias and ReLU in f16 in the epilogue.
  - Zero padding by bounds checks on the A load, not by a padded copy.
  - Fusion beyond the steps above (for example layers 6 and 7, or keeping a layer's output in threadgroup memory for the
    next) is allowed when it keeps the rounding of the reference. Note in the code where it is done.
- `kernels.rs`: `fn plan(weights: &[u8], resampler: &[u8]) -> Plan` with the buffers (packed weights with `init`,
  activations, the two skips), the steps, grids and bindings.
- Checks without a GPU: the MSL compiles with `xcrun -sdk macosx metal -c` (a test that skips when `xcrun` is missing);
  the plan's bindings reference existing buffers; buffer sizes match the layer table.

GPU time of the kernels, `cargo test --release -p nxkit-gpu --test ndve_bench -- --ignored --nocapture` (synthetic
weights and picture, 1920x1080 to 2560x1440, M2 Max, minimum of 80 runs, ms; every step alone in a command buffer; the
picture and the output hash are the same before and after):

| step | before | after |
|---|---|---|
| colour conversion (new pass) | - | 0.05 |
| layers 1 + 2 | 1.35 | 0.88 |
| layers 3 / 5 / 8 / 10 | 0.093 / 0.084 / 0.095 / 0.139 | 0.093 / 0.080 / 0.092 / 0.125 |
| layers 4 / 6 / 7 / 9 | 0.337 / 0.338 / 0.338 / 0.337 | 0.320 / 0.316 / 0.315 / 0.320 |
| layers 11 + 12 | 1.34 | 1.16 |
| luma pass | 0.020 | 0.020 |
| resampler | 1.27 | 1.15 |
| whole plan, one command buffer, a barrier between steps | 5.78 | 4.92 |

The floor of this design: a kernel that only runs the 8x8 products on registers takes 0.25 ms per 3x3 layer (4.9
TMAC/s) and 0.28 ms with the f16 round of every K = 16 step (4.3 TMAC/s). The network is 15.7 GMAC, 3.7 ms at that
rate, so the 3x3 layers (0.32 ms) and the resampler (1.15 ms for 5.1 GMAC) are within 15 % of it. What was left to win
was the occupancy of the fused pairs (threadgroup memory: with a 16 halves pixel pitch three threadgroups fit a core
instead of two), the picture conversion and the weight loads. Below about 4.5 ms only fewer MACs or fusing more steps
(the drain at the end of each of the 13 dispatches is about 0.03 ms) would help. What does not work: an f16 accumulator
(6 times slower), 8 simdgroups in the pairs with the output channels split (slower), a pair tile of 10 rows, 8 rows of
a 3x3 tile per simdgroup.

### C. The hook in `nxkit-gpu` (new module `native.rs` and its call sites; `diag.rs` for the capture)

- A general interface for a native replacement of a run of compute dispatches: find a run by program keys in the
  frame's dispatch list, check it, then encode a `Plan` in its place. NDVE is its first user; the interface names no network.
- Reading the bound weight bytes from guest memory through the dispatch's storage buffer bindings; caching the
  fingerprint result and the built plan by the blob's address and the memory's generation, so a frame costs one lookup.
- Encoding a `Plan`: one compute pass, the plan's buffers made once, game resources resolved from the matched
  dispatches' bindings, barriers between dependent steps. The game's intermediate images (`GameResource::Intermediate`)
  are still written when the game reads them later; check with the frame which of them are read.
- `NXKIT_NDVE=0`: off. `NXKIT_DIAG=ndve`: once per movie, print the capture list of ndve-native.md section 10
  (bound buffer addresses and sizes per dispatch, push constants, texture formats and sizes, sampler states, the keys).
  With `NXKIT_DIAG=ndve` and `NXKIT_NDVE_COMPARE=1`, run both the game's chain and the plan on a sampled frame and
  print the largest difference per channel and the count of differing pixels.
- The health report counts `ndve.native`, `ndve.fallback` with the reason.

### D. HMMA rounding in the translator (`nxb-xlate`: `msl/hmma.rs`, `xform/tensor.rs` and their tests)

- The translator keeps the accumulator of an HMMA chain in f32 and rounds once at the end of the chain. The hardware
  rounds D to f16 after every `HMMA.16816.F16` (ndve-native.md section 5). Round after every HMMA of an F16 chain;
  keep F32 HMMAs as they are. Bump `TRANSLATOR_ID`.
- This changes the MSL of every shader with HMMA (Ocarina: the movie network and the game's upscaler), so only those
  compile again at the next build.

## 3. Order

A, B, C and D start at the same time. B and C depend only on `plan.rs`. The first run in the game needs A, B and C.

## 4. How we test it in the game

1. `NXKIT_DIAG=ndve`: capture the runtime values once and fill section 10 of ndve-native.md.
2. `NXKIT_NDVE_COMPARE=1`: native against translated on sampled frames.
3. GPU time with `NXKIT_PACE_DIAG=1`, native against `NXKIT_NDVE=0`.
4. By eye: the title movie and the opening.

## 5. What "right" means

There is no picture from the console. Three references, in this order:

1. The CPU reference (`reference.rs`), which follows the arithmetic read from the shader code. The kernels match it per
   layer bit for bit where the order of the f32 sum is the same, and otherwise within 1 f16 ulp per element.
2. The translated chain after package D. It is noisy: the translated network has a race between lanes, so the same frame
   gives different pixels between runs. Compare by the largest difference, not bit for bit.
3. The eye.

What the code does not tell us is the rounding inside one HMMA (the order of the 16 products). If a console picture
becomes available, it decides.
