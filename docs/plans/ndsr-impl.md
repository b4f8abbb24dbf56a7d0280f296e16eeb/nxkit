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

This is the critical path; nothing bit-faithful can be written before it. It needs no repo code.

- Extend the NDVE emulator (scratch `ndve/emu.py`, copy to `ndsr/`): `TLD4` 0xb63 (four tokens, one component, the
  gather order), `MUFU` (symbolic `rcp/rsq/lg2/ex2/sqrt/sin/cos` by sub-op), any other missing ops the vr programs hit.
- Emulate one threadgroup of `14c10aa70b81f04e` (vr 1.2 A) and `e4b49569547e3d47` (vr 3.0 A), then one variant B, one
  vr_light and one agl_ndsr program (`abef20ad0d9a9b12`). Read from it, per network: the input block (`in x in` G
  texels -> K lanes, padding/clamp at the window edge), the HMMA chains (A row bases, B tile offsets in the blob, C
  start), ReLU and bias positions, the grid the network writes to threadgroup memory, and the epilogue expression tree
  (bilinear, sqrt, chroma difference, signed square, sRGB, dither hash bit by bit).
- Derive the weight permutation agl -> vr from the emulated B addresses (the multisets already match, section 5) so
  one canonical layout serves all three packs.
- Output: a per-network spec table (config, block shape, layer list, weight offsets) and the epilogue formula, marked V.
  Can be split between two people: vr/vr_light programs and agl_ndsr programs.

### A. Table, fingerprint, reference (`nxkit-ndsr`: `net.rs`, `fingerprint.rs`, `pack_file.rs`, `reference.rs`)

- `net.rs`: the 52 network programs of ndsr-native.md section 4 as a table: key, pack, ratio (in, out), step
  `(b0, b1)`, variant (sRGB A / B / C, sRGB decode for agl B), blob length, blob SHA-256. Fill the layer shapes from A0.
- `fingerprint.rs`: `fn find(keys: &[&str]) -> Option<(usize, &Net)>`; `fn weights_ok(net, blob) -> bool`.
- `pack_file.rs`: parses an NDSR pack (header, 8-byte entries, types of section 3). Tests and tools only: with
  `NXKIT_NDSR_PACK=<path>` a test checks the three packs' entry tables and blob hashes against `net.rs`.
  `nxb-split::engines::ndsr_shaders` stays the build-side parser; this one only reads, it shares no code with it.
- `half.rs`: reuse `nxkit-ndve`'s (re-export or move to a shared place in package 0).
- `reference.rs`: the network on the CPU from an RGBA input image and the push words to the output image, with the
  arithmetic of A0 (f16 per HMMA step of K = 16, rounding to f16 after every HMMA, bilinear and epilogue in f32 in the
  shader's fma order, the integer dither hash). Exposes the network grid before the resample so kernels can be checked
  in two halves. Tests: f16 edge cases, a hand case of one block, determinism on a 64x48 crop, the real blobs pass the
  fingerprint with `NXKIT_NDSR_PACK`.

### B. Kernels and packing (`nxkit-ndsr`: `pack.rs`, `msl/*.metal`, `kernels.rs`)

- `pack.rs`: one canonical weight layout for `simdgroup_half8x8` tiles from any of the three blob layouts (the
  permutation of A0), biases as f16 vectors.
- `msl/ndsr.metal`: one kernel, specialised by function constants (in block, out block, block count 3 or 6, variant):
  load the input window of G (or f16 luma for light, f32 luma for agl), run the block MLP on `simdgroup_matrix` with an
  f16 rounding after every K = 16 step (as NDVE's kernels do), write the ratio grid to threadgroup memory, resample
  bilinear to the output pixels, apply the epilogue of A0, write the image. Reuse NDVE's resampler kernel structure
  (`nxkit-ndve/src/msl/resample.metal`), which is already this network family at 4/3; the difference is the ratio and
  the arbitrary-size resample.
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
- `NXKIT_DIAG=ndsr`: once per distinct (key, sizes), print the capture list of ndsr-native.md section 10 for the
  network dispatch and the four SMAA dispatches before it (keys, grids, push words, texture/image formats and sizes,
  samplers, ssbo addresses). `NXKIT_NDSR_COMPARE=1`: run both on a sampled frame and print the largest difference per
  channel. Health lines `ndsr.native`, `ndsr.fallback` with the reason.
- Depends on package 0 only for types; can stub `nxkit-ndsr` as `native_ndve.rs` does in its tests.

### D. Family layer and build facts (`titles/families/agl/src/lib.rs`, `crates/nxb-build/src/host/header.rs`)

- agl `shader_role`: match the archive names `agl_ndsr.pck`, `agl_ndsr_vr.pck`, `agl_ndsr_vr_light.pck` (the place
  strings of the shader map), evidence checked against ndsr-native.md section 2; fix the test's archive name.
- `upscaler_keys`: include the vr and vr_light compute keys (today it takes only `/agl_ndsr.pck#`), or split it into
  `upscaler_keys` per pack. Nothing reads it at runtime yet, so this is a correctness fix for later users.
- No kernels and no runtime replacement here (section 12 of ndsr-native.md).

### E. Translator (`nxb-xlate`: tensor pass, only if A0 finds gaps)

- The agl_ndsr networks use MOVM and LDSM like the NDVE resampler, already handled. The vr/vr_light networks use
  `LDS` fragments and `TLD4`; if A0 finds a lane-dependent pattern the translated MSL gets wrong (as the dotted rows
  were), fix it here so the fallback path is right too. Package D of ndve-impl.md (round after every HMMA) already
  covers these shaders.

## 3. Order

0 first (small). Then A0, C, D, E in parallel; A and B start on the table, packing infrastructure and kernel skeleton at
once and fill in the layer semantics when A0 delivers. The first in-game run needs 0, A0, A, B, C; the first capture
(`NXKIT_DIAG=ndsr`, package C) can run before the kernels exist and should, because it decides which pack and variant
matter.

## 4. How we test it in the game

1. `NXKIT_DIAG=ndsr`: which pack, keys, sizes and formats; fill section 10 of ndsr-native.md.
2. `NXKIT_NDSR_COMPARE=1` on sampled frames across the dynamic-resolution range (walk through Kakariko and Hyrule Field).
3. GPU time with `NXKIT_PACE_DIAG=1`, native against `NXKIT_NDSR=0`.
4. By eye: edges and text at the lowest resolution, shimmer at resolution changes.

## 5. What "right" means

As for NDVE: the CPU reference first (bit for bit where the f32 order matches, else within 1 f16 ulp in the network
grid and 1 unit of the 8-bit output), the translated chain second (compare the largest difference; the translated
network can be noisy), the eye third. The HMMA internal rounding remains unknown without a console picture. The dither
is an integer hash of the pixel position and must match exactly.
