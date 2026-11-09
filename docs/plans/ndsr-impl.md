# agl NDSR natively: implementation plan

What the upscaler is: [ndsr-native.md](ndsr-native.md). This plan says how nxkit runs it with its own Metal kernels in
place of the game's translated network dispatch, and how we know the result is right. It follows
[ndve-impl.md](ndve-impl.md) and reuses its machinery.

Goal: the gameplay upscale (SMAA + network) in at most 1.5 ms of GPU time a frame on an M2 Max at 1536x864 in, with
the picture of the translated chain within the limits of section 5. The network alone is about 1.5 GMAC (I).

## 1. Shape of the solution

- **`nxkit-ndsr` (new, L1, std only).** The NDSR library: pack format, network table (key -> ratio, config, weight
  blob, variant), fingerprint, weight packing, Metal kernels as MSL text, the plan, a CPU reference. It is SDK
  knowledge shared by agl and `nn::ndve` (ndsr-native.md section 12), so it sits in the core, not in `titles/`.
- **`nxkit-gpu` runs it** through the existing `native.rs` interface (`Replacement`: `find`, `blobs`, `accepts`,
  `plan`, `resources`). A new `native_ndsr.rs` is its second user. `NXKIT_NDSR=0` turns it off.
- **Stage 1 replaces only the network dispatch** (one dispatch, one weight blob, one texture in, one image out). The
  SMAA passes stay translated: they have no HMMA, are cheap, and their outputs may be read elsewhere. Stage 2 (optional,
  after a capture shows the AA output is read only by the network) fuses neighbourhood blending into the network's load.
- **Variable keys.** Unlike NDVE, the run is one of 15 (vr) or 22 (agl) keys and changes with the dynamic resolution.
  `find` matches any key of the table; the plan cache key is (program key, blob hash, input size, output size).
- **No game file is read**, no game code is patched; weights come from guest memory through ssbo 2 of the matched
  dispatch.

Prerequisite, shared types: `nxkit_ndve::plan::{Plan, Step, Buffer, Binding, Source, GameResource}` is NDVE-specific
(`GameResource::LumaPlane` etc.). Package 0 moves the generic part so both networks can use it.

## 2. Work packages

Cut so that no two touch the same files. Package 0 is small and goes first (one day); A to E then run in parallel.
`nxkit-ndsr/src/plan.rs` is fixed by package 0; a change needs the agreement of B and C.

### 0. Shared plan types (`nxkit-ndve/src/plan.rs`, `nxkit-gpu/src/native.rs` signatures, new `nxkit-ndsr` skeleton)

- Make `GameResource` generic: `Texture { dispatch, slot }`, `Image { dispatch, slot }`, `Push { dispatch }`,
  `Intermediate { index }` (or a `u16` id plus a name the replacement defines). Keep `nxkit-ndve` compiling by mapping its
  variants to these in `native_ndve.rs`. Alternative with less churn: let `Replacement` carry an associated resource
  enum; pick whichever keeps `native.rs` free of network names.
- Create `crates/nxkit-ndsr` with `lib.rs`, `plan.rs` re-export, empty modules for A and B, `Cargo.toml`
  (`layer = "L1"`, depends on `nxb-model` for SHA-256), and add it to the workspace and to `nxkit-gpu`'s deps.

### A0. Finish the recovery (scratch tools, then `docs/plans/ndsr-native.md` sections 7, 8, 13)

**Status: done for the vr and vr_light packs, structure only for agl_ndsr.** All 15 vr network programs are recovered
bit for bit (ndsr-native.md section 7: a Python model, `scratchpad/ndsr/refvr.py`, equals the lane-exact emulation of
the SASS on every output pixel of 12 threadgroups per program at four input sizes). The agl -> vr weight permutation is
known (section 8.3), so one canonical layout (agl's, which is also NDVE's) serves all packs.

A0b, vr_light: done. All 15 programs are bit for bit (ndsr-native.md section 15: `light/refl.py` equals the
emulation on every pixel of 12 threadgroups per program at four set-ups). The 0x1800 blob is the vr layout over
3 layers; repacked, it is agl's 4/3 blob. Ocarina of Time runs `fa3033d05c0a41d6` (section 10.1). The reference, the
kernels (`ndsr_light.metal`, `kernels/light.rs`) and the hook are in place. The GPU test against the reference is
`crates/nxkit-gpu/tests/ndsr_light_kernel.rs`.

Left:

- agl_ndsr (22 programs): only if a capture shows the agl pack in gameplay.
- Nothing here blocks the vr path.

### A. Table, fingerprint, reference (`nxkit-ndsr`: `net.rs`, `fingerprint.rs`, `pack_file.rs`, `reference.rs`)

- `net.rs`: the 52 network programs of ndsr-native.md section 4 as a table: key, pack, ratio (in, out), step
  `(b0, b1)`, variant (vr: A sRGB 8-bit / B linear 10-bit / C linear R11G11B10F; agl: B adds an sRGB decode), blob
  length, blob SHA-256, and per vr program the immediates the bit-exact path needs (section 7.4 and the section 4
  table): the ratio bits `R`, the cell-offset triple `(s, h, h1x, h1y)` and whether `ty` uses the odd-row form. The
  layer shape is the same for all (6 layers x 2 terms of 16x16, section 7.3; 3 layers for the 0x1800 blobs, I).
- `fingerprint.rs`: `fn find(keys: &[&str]) -> Option<(usize, &Net)>`; `fn weights_ok(net, blob) -> bool`.
- `pack_file.rs`: parses an NDSR pack (header, 8-byte entries, types of section 3). Tests and tools only: with
  `NXKIT_NDSR_PACK=<path>` a test checks the three packs' entry tables and blob hashes against `net.rs`.
  `nxb-split::engines::ndsr_shaders` stays the build-side parser; this one only reads, it shares no code with it.
- `half.rs`: reuse `nxkit-ndve`'s (re-export or move to a shared place in package 0).
- `reference.rs`: a port of `scratchpad/ndsr/refvr.py` (ndsr-native.md section 7, step for step): window, patches, the
  6-layer network (f16 after every HMMA), grid with the f16 base sample and signed square, unsharp, tile rule,
  resample, chroma, encode A/B/C, hash dither; the sampler as a parameter (bilinear, clamp to edge by default). Exposes
  the grid before and after the unsharp pass so kernels can be checked in three parts. Tests: f16 edge cases, golden
  values for a few threadgroups produced by `refvr.py` (checked in as small fixtures, no game data), the tile rule at
  the first and last tiles, determinism on a 64x48 crop, the real blobs pass the fingerprint with `NXKIT_NDSR_PACK`.

### B. Kernels and packing (`nxkit-ndsr`: `pack.rs`, `msl/*.metal`, `kernels.rs`)

- `pack.rs`: the canonical layout is agl_ndsr's (= NDVE's: per layer `[W1_0, W1_1, W2_0, W2_1]`, 8x8 row-major tiles,
  ready for `simdgroup_load`); vr blobs map to it by ndsr-native.md section 8.3 (fragment order, and the even layers'
  transposes). No biases exist.
- `msl/ndsr.metal`: one kernel, specialised by function constants (in, npx, npy, variant): load the window of Y' (or
  f16 luma for light, f32 luma for agl), run 5 layers of NDVE's `rs_block` plus one without ReLU (ndsr-native.md
  section 14: `common.metal`'s `hstep` and `rs_block` are reusable as is), keep `Y[0..11][0..11]`, build the f16 grid
  with the base sample, the unsharp pass, then the per-pixel resample, chroma, encode and dither of section 7.6-7.8.
  The epilogue is new code; NDVE's has nothing of it.
- `kernels.rs`: `fn plan(net, blob, in_size, out_size, push) -> Plan`: the packed weight buffer (`init`), one step with
  grid `ceil(W / b0) x ceil(H / b1)` or the kernel's own tiling, bindings to the game's tex0, sampler and img1.
- Checks without a GPU: MSL compiles with `xcrun -sdk macosx metal -c` (skip without `xcrun`); bindings reference
  existing buffers. With a GPU (ignored test): kernel vs `reference.rs` per network on random inputs.

### C. Hook and capture in `nxkit-gpu` (`native_ndsr.rs` only, plus its line in `native.rs`'s list)

- `impl Replacement for Ndsr`: `find` via `nxkit_ndsr::fingerprint::find` over all keys; `blobs` = ssbo 2 of the
  matched dispatch, 0x3000 or 0x1800 bytes; `accepts` = hash check; `plan` = `kernels::plan`; `resources` = tex0 +
  sampler, img1, push bank 24 of that dispatch.
- The cache key includes the input and output sizes (dynamic resolution changes them every few frames); keep the last
  few plans (one per ratio) so a resolution change costs no rebuild.
- `NXKIT_DIAG=ndsr`: once per distinct (key, sizes), print step 2 of ndsr-native.md section 10 for the network
  dispatch and the three AA dispatches before it (keys, grids, push words, texture/image formats and sizes, samplers,
  ssbo addresses, whether tex0 is the neighbourhood-blending img2, the readers of img2 and img1).
  `NXKIT_NDSR_COMPARE=1`: run both on a sampled frame and print the largest difference per channel. Health lines
  `ndsr.native`, `ndsr.fallback` with the reason.
- `accepts` also checks the bound tex0 has the format the variant expects (the `92918e9b` output) and img1 one of the
  variant's formats; anything else falls back.
- Depends on package 0 only for types; can stub `nxkit-ndsr` as `native_ndve.rs` does in its tests.

### D. Family layer and build facts (`titles/families/agl/src/lib.rs`, `crates/nxb-build/src/host/header.rs`)

- agl `shader_role`: match the archive names `agl_ndsr.pck`, `agl_ndsr_vr.pck`, `agl_ndsr_vr_light.pck` (the place
  strings of the shader map), evidence checked against ndsr-native.md section 2; fix the test's archive name.
- `upscaler_keys`: include the vr and vr_light compute keys (today it takes only `/agl_ndsr.pck#`), or split it into
  `upscaler_keys` per pack. Nothing reads it at runtime yet, so this is a correctness fix for later users.
- No kernels and no runtime replacement here (section 12 of ndsr-native.md).

### E. Translator (`nxb-xlate`: tensor pass, only if A0 finds gaps)

- The agl_ndsr networks use MOVM and LDSM like the NDVE resampler, already handled. The vr/vr_light networks have two
  patterns A0 found that the translated MSL must get right (check against `refvr.py` with `NXKIT_NDSR_COMPARE=1`
  once package C exists, or by reading the MSL of 14c10aa7): (1) the **D registers of one HMMA used directly as the B
  operand** of the next (a matrix transpose by fragment layout, no MOVM; section 7.3), and (2) weights loaded by
  `LDG.128` straight into A/B fragment registers (the blob is in fragment order). Also `TLD4.PTP` with per-texel
  offsets (Metal has no gather with four offsets; the translation must fetch four texels) and `S2UR` ctaid reads on the
  uniform path in 12 of the 15 programs. Package D of ndve-impl.md (round after every HMMA) already covers the
  rounding.

## 3. Order

0 first (small). A0 is done for vr, so A, B, C, D, E can all start; A0b (vr_light, agl) runs alongside. The first
in-game run needs 0, A, B, C. Before any of that, one gameplay run with today's `NXKIT_DIAG=comp` answers which pack,
variant and ratios run (ndsr-native.md section 10 step 1, no code needed); if it shows the light or agl pack, A0b moves
onto the critical path.

## 4. How we test it in the game

1. `NXKIT_DIAG=ndsr`: which pack, keys, sizes and formats; fill section 10 of ndsr-native.md.
2. `NXKIT_NDSR_COMPARE=1` on sampled frames across the dynamic-resolution range (walk through Kakariko and Hyrule Field).
3. GPU time with `NXKIT_PACE_DIAG=1`, native against `NXKIT_NDSR=0`.
4. By eye: edges and text at the lowest resolution, shimmer at resolution changes.

## 5. What "right" means

As for NDVE: the CPU reference first (bit for bit where the f32 order matches, else within 1 f16 ulp in the network
grid and 1 unit of the output format), the translated chain second (compare the largest difference; the translated
network can be noisy), the eye third. The reference itself is checked against the emulated SASS (bit for bit, A0). The
HMMA internal rounding, the MUFU error and the sampler's sub-texel precision remain unknown without a console picture;
`hstep` (two f32 8x8x8 products, then f16) differs from the reference's single exact rounding only in rare
double-rounding cases. The dither is an integer hash of the pixel position and must match exactly.
