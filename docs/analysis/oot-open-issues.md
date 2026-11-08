# Open Ocarina of Time defects in the shader translator

This note covers three open defects of Ocarina of Time (`0400486024DEC000`) on Apple GPUs: dotted rows, coloured speckles and GPU page faults.
The analysis was done on 2026-11-06 on an Apple M2 Max. The game was not run.
The evidence comes from user reports and the `nxb-xlate` code.
It also comes from the MSL that `nxb-xlate` makes for the shaders of the game.

## Shaders of the game that use tensor cores

The game has 89 neural network shaders (the NDSR upscaler `agl_ndsr_vr` and the `Ndve` movie networks) and 2 task shaders.
71 of these shaders use `HMMA.16816.F16`.
`nxb-xlate` translates `HMMA.16816.F16` to an inlined helper that uses `simdgroup_matrix`. Refer to `crates/nxb-xlate/src/msl/hmma.rs`.
The `crates/nxb-xlatedev` tool `xlate-msl` writes the MSL of a list of shader keys. All 71 shaders compile with `xcrun metal` before and after the fix.

## Dotted rows on M3 Max

### Cause

The cause is known. An `HMMA` outside a chain wrote its result into slot 0 of the chain accumulators. A chain that started later read that slot as its C. This gave rows of wrong pixels on the GPUs where the SIMD-group keeps the matrices.
Several testers confirmed that the fix removes the rows on M3 Max and M4 Max.

### Fix in nxb

`nxb` has the fix since commit `47ef3a3` (port of `13b432b4`). A slot is read only when the call is the C of a chain. A slot is written only when the call is the D of a chain. A call outside every chain does not touch a slot.
The test `the_hmma_helper_touches_a_slot_only_for_a_chain` in `crates/nxb-xlate/src/msl/gaps_tests.rs` covers it. No new change was necessary.

### In-game check

On an M3 or M4, load the save at the Kokiri Forest balcony. Look at the fog and the light shafts in Kokiri Forest. There must be no regular dotted rows.

## Coloured speckles on M1, M1 Max and M2 Pro

### Cause

The cause is proven offline for the arithmetic. The link to the speckles on the screen is a strong hypothesis.
The old helper loaded A, B and C as `simdgroup_half8x8` and called `simdgroup_multiply_accumulate` with a half result.
On the M2 Max this product rounds to half after each step of the sum. It gives infinity when a partial sum is larger than 65504, also when the final sum is small.
Ampere does not do this. `HMMA.16816.F16` calculates the products exactly, sums them in at least FP32 and rounds D to half once. The scalar path of `nxb-xlate` (`crates/nxb-xlate/src/xform/tensor.rs`) also uses FP32.
The script `tools/hmma-precision/run.sh` shows the difference on the GPU of the Mac. These are the values on the M2 Max:

| case | half product | float accumulator | exact |
|---|---|---|---|
| 40000 + 40000 - 40000 - 40000 | inf | 0 | 0 |
| 2048 + 7 x 1 | 2048 | 2055 | 2055 |
| 65504 + 16 - 16 | inf | 65504 | 65504 |
| 300 x 300 - 300 x 300 | inf | 0 | 0 |
| 0.5 + 4096 - 4096 + 0.25 | 0.25 | 0.75 | 0.75 |

An infinity or a large error in one 8 x 8 tile of a network gives a wrong colour in a small group of pixels. This matches the speckles.
The speckles stay when the MetalFX upscaler setting is on. MetalFX replaces only the NDSR upscaler. The `Ndve` movie networks still use `HMMA`. Users report the speckles on the title screen and in the tapestry intro, where these networks run.
The speckles are reported only on M1 and M2. One M1 Max report of the page faults also mentions purple and magenta artefacts. A possible explanation is that the M3 and later GPUs sum a half product with more precision. This was not tested, because only an M2 Max was available. `tools/hmma-precision/run.sh` shows it on other Macs.

### GPU test of the emitted helper

A GPU test took the helper from the MSL of the shader `0253bd17deceee79` without changes. It ran the old and the new helper on random cases for three distributions of the input values on the M2 Max. Each case gives 128 half values.
It compared each value with an exact FP64 reference that is rounded to half. For a chain the reference keeps FP32 between the calls, as the new helper does.
The inputs are random, so the counts change a little from run to run. These are the counts of one run:

| case | old helper | new helper |
|---|---|---|
| D from registers, normal values: exact results | 31.3 % | 99.96 % |
| D from registers, normal values: results more than 16 ulp off | 22833 of 1048576 | 1 of 1048576 |
| D from registers, large values: infinity or NaN where the reference is finite | 665 | 0 |
| chain, normal values: exact results | 28.0 % | 99.95 % |
| chain, normal values: results more than 16 ulp off | 12984 of 524288 | 0 of 524288 |
| chain, large values: infinity or NaN where the reference is finite | 613 | 0 |

The few new results that are not exact come from the order of the FP32 sum.
Against a reference that rounds D to half after each call, as Ampere does, the new chain is exact for about 73 % of the values. The difference is the extra precision of the FP32 slot.

### Fix in nxb

C, D and the chain slots are `simdgroup_float8x8`. A and B stay `simdgroup_half8x8`. Each product is a mixed-precision `simdgroup_multiply_accumulate` with half A and B and a float accumulator. The C from the registers stays a half matrix and goes directly into the first product.
D is stored as floats in the B and C words of the scratch block. Each lane reads its two floats and packs them to half once.
The helper does not use the lane layout of a `simdgroup_matrix`. All data moves through `simdgroup_load` and `simdgroup_store`, so the fix is the same on all Apple GPU families.
A chain keeps FP32 in its slot from one call to the next. Ampere rounds D to half after each call. The result of a chain can therefore be more accurate than on the Switch. It does not overflow where the Switch does not.
On the M2 Max the cost is about 1 %. Each call from registers took 1.691 ns against 1.674 ns before. Each call in a chain took 1.366 ns against 1.350 ns before.
A scalar path that does not use `simdgroup_matrix` is not necessary. The new helper is correct on all families and keeps the speed.
`TRANSLATOR_ID` is `msl-2.16`.

### Tests

- `the_hmma_helper_accumulates_in_fp32` in `crates/nxb-xlate/src/msl/gaps_tests.rs` checks the float matrices, the mixed-precision products, the float store and the one pack to half.
- The tests `the_hmma_helper_runs_on_simdgroup_matrices` and `the_hmma_helper_touches_a_slot_only_for_a_chain` and the chain test in `crates/nxb-xlate/src/msl/tensor_tests.rs` check the new declarations.
- The ignored test `gaps_metal_compiles_the_samples` compiles the samples with `xcrun metal`.

### In-game check

Build the game with this `nxb` and run it on an M1 or M2. Look at the title screen and at the tapestry intro, once with the default upscaler and once with the MetalFX upscaler setting. There must be no coloured speckles. The frame time must stay the same.
On an M3 or M4, check that the picture does not change and that the dotted rows do not come back.

## GPU page faults after 15 to 45 minutes

### Cause

The cause is on the host side, not in the translator. `cpu_unc_drop` hands a retired texture to the serial entries of a package. The texture left the residency set while other serials of the package still drew with it. The GPU then read memory that was not resident and gave a page fault.
The fix is that a retired texture leaves the residency set only when no serial entry holds it.

### Fix in nxb

There is no change in `nxb`. The fix is in the runtime.

### In-game check

Play for 45 minutes or more on a runtime that has `fa9cfb84`. There must be no GPU page fault and no hang.

### Translator code that was examined

Before the cause was known, the two task shaders `99280b7b8aae78df` and `47f9b38490dcf9a2` were examined. They load from guest memory through the raw load helper. The helper does a binary search in the pool table of 8192 entries (`GMEM_MAP_LOG2` in `crates/nxb-xlate/src/opt/gts_raw.rs`) and returns 0 when the address is not in the table.
In `99280b7b8aae78df` the number of loop passes is `((min(end - start, 256) - 1) >> 5) + 1` with an unsigned shift. If two misses give 0 for both start and end, the count is 2^27. Ampere does the same arithmetic, so the translation is correct. No miss was shown, and the upstream fix explains the faults without one.
