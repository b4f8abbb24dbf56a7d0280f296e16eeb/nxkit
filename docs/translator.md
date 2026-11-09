# Shader translator

This document describes the translator of the shader step. One Rust translator turns the machine code of a shader into
Metal Shading Language (MSL). It runs in the process of the shader step. It needs no .NET runtime and no external translator program.
The decision is D20 in [decisions.md](decisions.md).

## Function

The chain for each shader is:

1. Switch GPU machine code (Maxwell, or Ampere for NVN2).
2. `nxb_xlate::msl::translate`: decode, control flow, SSA, passes and the structured program, then MSL.
3. `metal` and `metallib`, or the own front end for the shaders that it accepts.

The games contain only machine code. The shader step writes one job for each shader with the code, the `c1` jump-table segment,
the answer table (`shader_facts.tsv`) and the options. The job writer is `nxb_shader::job`. The translator takes the job and the code.

## Crates

| crate | content |
|---|---|
| `nxb-maxwell` | the decoders for Maxwell and Ampere |
| `nxb-xlate` | the translation and the MSL back end `nxb_xlate::msl` |
| `nxb-shader` | the job writer, the chain version, the guards and the side library cut |
| `nxb-stages` | the shader step: it calls the translator, compiles the libraries and writes the files |

## Contract

The call is `nxb_xlate::msl::translate(&job, &code, &ctx)`. It returns `Translated` or `Failure`.

- `Context` has the chain version V, the source (`<package>#<index>`), the paired shader of the game's program (`pc_paired`), the answer-table flag `no_facts`, the flags `dual_source` and `fetch_outputs_only` and the `partners`.
- `partners` is used by vertex shaders. It holds the `StageInputs` (the user varyings) of the paired pixel shaders. The translator pads the vertex outputs from them.
- `Translated` has the libraries, the `StageInputs` of a pixel shader and the optional `Side`.
- A `Library` has a suffix (`_b`, `_bf`, `_bp`, `_bv`, `_bg*`, `_bl*`, `_bgv`, `_bx`), the MSL text and an optional `Reflection`. The first library is `_b`.
- The `Reflection` is the typed content of `<key>_b.json`. `to_value()` gives the JSON value that the step writes with the same writer as before.
- `Side` names the alpha test entries and the logic op entries of a pixel shader. The step cuts them from the `_b` library into `_bx` at the metallib level.
- `Failure.reason` is one of the reason strings of [shader-step.md](shader-step.md). The step counts it as the refusal of the translator.

The output keeps the contract of the engine host: the argument buffer at `[[buffer(0)]]` with `[[id(n)]]` members, the entry `main0` and its extra entries,
the library suffixes and the fields of `_b.json`. The facts in `_b.json` come from the emitter. Nothing parses MSL text.

## Render scale

The front end ports the render scale of Ryujinx. A pixel shader divides the pixel position by `render_scale[0]` of the `support_buffer` (binding 0). A texel fetch with integer coordinates multiplies each coordinate by the scale of its texture, and a texture size query divides by it (`xform/texture.rs`, helpers `TexelFetchScale` and `TextureSizeUnscale`). Image loads, stores and atomics of a 2D image take the same multiplication, so a store lands on the scaled image. A texture or image of another dimension, an array of textures and a bindless handle keep their coordinates. Point sampling with normalized coordinates needs no change, and neither do derivatives.

`render_scale` has one float for each of 73 entries, 16 bytes apart from byte `SUPPORT_RENDER_SCALE` (192) of the support buffer. Entry 0 is the scale of the pass. Entry `1 + i` is the scale of the texture or image with descriptor index `i`. Images follow the textures. The host writes `frag_scale_count` (byte 176) as 0, so a vertex, pixel and compute shader number their entries alike. The constants are `SUPPORT_RENDER_SCALE`, `SUPPORT_RENDER_SCALES` and `SUPPORT_SIZE` in `msl/abi.rs`, and a test compares them with the front end.

The reflection tells the host what to fill, so a shader that scales nothing costs nothing:

- `pass_scale: true` (top level): the shader reads `render_scale[0]`.
- `rscale: n` on a `tex` or `img` binding: a helper reads `render_scale[n]` for this resource. The host writes the scale of the bound texture there.

The scaling code is in the MSL only for the operations the shader has. A shader without them has neither field.

## Entry functions

`msl/entry.rs` writes one function around a translated body. `EntryName` names it: the function name, its `Form`, the return type, the stage input (`StageIn`), extra `Param`s and prologue statements. The form is an entry function or a `static` body function `dm_body_<name>` with the parameters but no attributes.
The shared memory of a body function is a parameter, because its caller owns it. `msl/library.rs` builds the libraries: `_b` with its reflection, `_bv` and `_bp` for a vertex shader, and `_bf` and the side units for a pixel shader.
A variant writes its entries with `EntryName { field: .., ..EntryName::new("main0") }`.
Pixel entries and the wrapper of a task or mesh shader call a body function. The end of a vertex entry comes from `Plan::vertex_epilogue`: the depth conversion. A viewport mask that a vertex shader writes becomes `gl_ViewportIndex`, the lowest set bit capped at 15. `main0_z` and `main0_lz` keep the depth range of the source.

## Vertex variants

The emitter produces everything a vertex shader needs from one trace. The code is in `crates/nxb-xlate/src/msl/variant/`.

- The vertex outputs are padded to the inputs of every paired pixel shader. A varying the pixel shader reads and the vertex shader does not write gets a default. The locations are in `vs_pad`.
- An attribute that the shader only uses as an integer is declared `uint`. The emitter probes this and builds the shader again.
- The entries `main0_z`, `main0_discard`, `main0_l`, `main0_lz`, `main0_gs` and `main0_tf` are further entries of the `_b` library. `entries` and `msl_entry_kind` in `_b.json` name them.
- `main0_tf` is the transform feedback capture of a vertex job that has `tf`. It runs the vertex function and stores the captured user outputs to the feedback buffers. It returns nothing. `entry_tf` in `_b.json` names it and `tf_strides` has the bytes per vertex of each of the four buffers (0 for a buffer without capture).
  The buffer `b` is bound at slot 8 + b as `dm_tf<b>`. The state `dm_tf_capture` is at slot 14. Vertex `v` of instance `i` writes record `(i - base_instance) * count + v - first`, and only if the record is below `dm_tf.args.x`.
  An output that the shader does not write is stored as 0. Component w of such an output is stored as 1.0. `_bv` and `_bp` have no `main0_tf`.
- `_bv` is the library with shadow inputs for integer attributes. The shadows sit at attribute indices 16 to 30. The field `vin` in `_b.json` lists them.
- `_bp` is the library that pulls the vertices from a buffer with `dm_fetch`. The field `pull` in `_b.json` names the entries and the buffer slot 16.
- `raster` has `pos_attr`, `pos_w`, `uv_from_attr`, `uv_affine` and `screen`. They come from the stores of the main function. A shader with a branch, a loop or a call has no raster facts.

## Compute kernels

A kernel with more than 32 KB of shared memory takes its arrays from a device buffer at `buffer(30)` (`spill`). The reflection field `tg_spill` has the bytes of one workgroup slice. The stores that only the first invocation of a dispatch makes leave the kernel, and the reflection field `first_store` lists them as `{reg, word, value}` for the host. A task or mesh shader keeps its shared memory in the threadgroup and is refused past the limit.

## Pixel variants and side units

A pixel shader gets extra entries around one static body function `dm_body_main0`. Each entry is a thin function that calls the body and then changes the result.

| entry | purpose | `_b.json` field |
|---|---|---|
| `main0_a` | alpha test, state at `[[buffer(30)]]` | `alpha` `{entries}` |
| `main0_l` | logic op, state at `[[buffer(29)]]`, function constants 200 and up | `logic` `{entries}` |
| `main0_u` | integer colour outputs | `entry_u` |
| `main0_m` | mixed outputs, with a mask for the integer targets | `entry_m`, `uint_mask_m` |
| `main0_d` | dual source blending | `entry_d` |

`Translated.side` names the alpha and logic entries. The step cuts them from `_b` into `_bx` with `nxb_shader::side::cut`.

The library `_bf` is a second build of the pixel shader for framebuffer fetch. Its entry is `main0_f` and its result struct is `main0_out_f`. The host specializes `main0_f` by name. A sample of a 2D colour texture is replaced by the attachment `fbK` when its function constant `FX_i` selects it.
The coordinate must be a varying (optionally divided by `w`) or a pixel position times uniforms.
`_b.json` gets the field `fetch` with `calls` (`fc`, `tex_reg`, `varying_locn`, `proj` or `frag_pos`), `mixed_regs` and `outputs`.
Only samples in the main function are replaced. A shader without such a sample gets `_bf` only when `Context.fetch_outputs_only` is set. The step sets it for the keys in the profile list `bf_want.tsv`.
`Context.dual_source` makes the translator build `main0_d`. The step sets it for the keys in the profile list `dual_want.tsv`.

A discard in a helper function sets the flag `dm_kill`. Every caller returns at once after the call, as the main function does after its own discard.
A fragment shader interlock (`FSIBegin`, `FSIEnd`) has no statements. The images and storage buffers that the shader writes carry `raster_order_group(0)` in the argument buffer.
`raster.sample_mask` is false because the IR has no sample mask output. `first_store` belongs to compute kernels and has no pixel form.

Every fragment entry gets `[[early_fragment_tests]]` when `early_tests` is true. That is the case when the shader forces the early depth test, or when its texture pool marks IDs and the shader has no discard, depth write or memory write (`pool::early`). The rule lives in `entry::early_tests`, and the base entries, the side entries and the fetch entry use it.

## Blend factor 0

On the console a blend term whose factor is exactly 0 contributes 0, also when the colour is NaN or Inf. The Apple GPU blender computes `0 x NaN = NaN`. A game that writes `Inf * 0 = NaN` with alpha 0 under `SRC_ALPHA` blending into a float target puts the NaN into the target, and a later pass spreads it as very bright blots.

Every pixel shader has the argument buffer member `constant uint* dm_bz` (binding kind `blendz`). It points to one 32-bit word that the host fills for each draw: 3 bits per colour target `i` at bit `3 i`, the kind of the source RGB factor of that target.

| kind | source RGB factor | the factor is 0 when |
|---|---|---|
| 0 | blending off, an integer format, a colour the shader does not write, the equation MIN or MAX, any other factor | never |
| 1 | `ZERO` | always |
| 2 | `SRC_ALPHA` | the alpha of the output is +0 or -0 |
| 3 | `ONE_MINUS_SRC_ALPHA` | the alpha of the output is exactly 1 |
| 4 | `SRC1_ALPHA` | the alpha of the second source is +0 or -0 |
| 5 | `ONE_MINUS_SRC1_ALPHA` | the alpha of the second source is exactly 1 |

When the word is not 0, `main0` calls `dm_body_main0` and then, for every float colour output, writes RGB 0 if `dm_bz_zero` says the factor is 0. The tests are on integer bits (`as_type<uint>`), so fast math cannot fold them away. Alpha is never changed, because it is also a factor and feeds alpha to coverage. Integer outputs, pads, the second source (`index(1)`) and the side entries (`main0_a`, `main0_l`, `_bf`) get no rule. `main0_m` and `main0_d` have the rule for their float members; `main0_d` passes the alpha of its second source as the second factor of kinds 4 and 5. The host sets the kinds 4 and 5 never, because it uses no dual source entry.

The rule does not cover a destination term with factor 0 (the shader does not see the destination), the factors `DST_*`, `SRC_ALPHA_SATURATE` and the constant colour factors, and the colour of `SRC1_COLOR` per channel. These stay at kind 0. With a stale NaN in the destination, `NaN x 0` can still differ from the console.

The feature revision `blendz` (now 1) is in the key of every pixel shader that is not a mesh stage job, so these shaders translate again. `TRANSLATOR_ID` does not change. A translation without the binding (an old store) works: the host does not fill what the program does not have, and the rule is off.

## Ampere global memory

`ATOMG.CAS` (opcode 0x3a9) is a compare and swap. Rb holds the expected value and Rc (bits 64 to 71) the new value. Rd gets the old value.
`ATOMG` and `RED` with `INC` or `DEC` become a compare and swap loop. `INC` gives 0 when the old value is at least the operand. `DEC` gives the operand when the old value is 0 or above the operand.
`ATOMS` with `INC` or `DEC` is the same loop on shared memory.
`ATOMS.CAS` with bit 87 clear gives the old value. With bit 87 set it is `CAST` or `CAST.SPIN`, and Rd is 1 when the swap happened, else 0.
Bit 76 of `LDG` and `STG` and bit 71 of `ATOMG` and `RED` mark the uniform register as a memory descriptor. A descriptor is not added to the address.

A raw global access whose buffer is not known at translation time looks up the bound storage buffers at run time (`opt/gts_raw.rs`).
Each lookup keeps the entry it chose last in a zeroed local word `gmem_last_N`. The next access first checks that entry.
When the address is outside it, the lookup tries the hint and then searches all entries. The end of the chosen entry is checked once more after the search.

## Ampere ALU details

`IADD3` sets its first carry predicate when the 64-bit sum is at least 2^32 and its second when it is at least 2 * 2^32. The `IADD3.X` of the high word adds both.
`PLOP3` computes both truth tables from the sources before it writes a destination. The first destination may be a source.
`POPC` and `FLO` take bit 63 as a bitwise not only in forms 1, 5 and 6. In the immediate form it is the immediate's top bit.
`LEA.HI` with `.SX32` (bit 73) takes the sign extension of A as the high word instead of C.
`LEA` with a negated B (bit 63, forms 1, 5 and 6) adds ~B with `.X` and ~B + 1 without it. The + 1 counts in the carry predicate.
A half-pair operand has a swizzle in A (bits 74 to 75), in the 32-bit slot (bits 60 to 61) and in the 64-bit slot (bits 81 to 82). A constant has one too. Only an immediate has none.
The values are 0 as is, 1 `.F32` (the 32-bit float in both lanes), 2 the low half twice and 3 the high half twice. `MUFU.F16` reads bits 60 to 61 as a half select, where an odd value picks the high half.
`HSETP2` and `HSET2` with `.FTZ` (bit 80) set a lane below 2^-14 in magnitude to zero before they compare.
`I2F` from a 32-bit integer to a 32-bit float and `F2F.F16.F32` apply the rounding in bits 78 to 79 (`.RM` 1, `.RP` 2, `.RZ` 3).
The result rounded to nearest is compared with the exact value, and its bits step by one to the neighbour that the mode asks for.
`F2F` reads the half back through the mask `0xffff`, because the optimizer folds an unpack of a pack into the packed float. `I2F` to a half and from a narrow integer rounds to nearest.
`MUFU.SIN` and `MUFU.COS` take turns and use only the fraction. The translator emits sin or cos of (t - rint(t)) * 2pi. The difference t - rint(t) is exact for every float and lies in [-0.5, 0.5], where Metal's fast sin and cos are accurate.

## Shared memory byte stores

A store of a byte or half word to shared memory is a call of the helper `SharedStore8_<id>` or `SharedStore16_<id>(byte offset, value)`. The helper's own body is a compare-and-swap loop on the word.
The emitter writes one Metal store instead: a `uchar` store, or an aligned `ushort` store through a `may_alias` type. The pointer is in `threadgroup` memory, or in `device` memory when the kernel spills.
Each byte is its own memory location, so neighbouring threads need no atomic. Two threads that store the same byte leave one of the two values, as on the console.
A task or mesh workgroup stores its primitive index bytes this way. With the loop, four threads contended for each word.
A call passes each argument as a variable, because the parameters are references.

## Pixel shader depth output

A pixel shader reads its colours from four registers for each render target with any component enabled. Component c of the n-th such target is in R(4n + c). The sample mask register follows, then the depth.
The depth register is 4 times the number of enabled targets plus 1 (`depth_register` in `ir/emitter_ret.rs`).

## Tensor cores

`LDSM` and `HMMA.16816.F16` run on `simdgroup_matrix` code. The tensor pass (`xform/tensor.rs`) turns each `HMMA` into a call of the helper `dmsh_hmma16816_f16`.
The helper gets 17 word arguments and the out argument D1. Argument 10 holds flags, and arguments 11 to 16 hold A's `LDSM` address, offsets and row stride.
The flags are constants at each call. The tensor pass sets them for HMMA chains in compute shaders.

- A chain is a run of calls where D of one call is C of the next. Its accumulator stays in a slot `acc[k]` between the calls. Bits 8 and up give k.
  The helper computes C and D in matrices of its own. It reads a slot only for a chain's C and writes it only for a chain's D. A call outside every chain touches no slot.
- A call with the same A as the call before loads no A.
- C = 0 is a multiply.
- A that an `LDSM` read is loaded from shared memory at the `LDSM`'s address. This needs exactly one shared memory besides the scratch, and the address must be the same in the whole SIMD-group.

In a compute shader an `LDSM` becomes one `MatrixLoad` per matrix. The tensor pass reads the address from it and then lowers it to shuffles, as in other stages.
A divergence analysis (`xform/uniformity.rs`) decides which values are the same in the whole SIMD-group.
An `LDSM` sits between two SIMD-group barriers. A warp often stores the rows and reads them with `LDSM` without a `BAR` between. The Switch runs these accesses in order. Metal orders the store of another lane only through a barrier.
Each function that calls the helper declares `dm_acc[16]` and `dm_am[4]` and passes them by reference. The helper is always inlined.
A and B are half matrices. C, D and the slots are float matrices, so the products and the sum are FP32. D is rounded to half once, when a lane reads it.
Ampere does the same. Apple's half `simdgroup_matrix` product rounds to half after each step. On an M2 Max it gives infinity when a partial sum is larger than 65504, and its error grows with cancellation.
A chain keeps FP32 in its slot from one call to the next. Ampere rounds D to half after each call, so the result of a chain can be more accurate than on the Switch.

## Output size

The shaders of a big Ampere title are far larger than those of a Maxwell title: the median `_b` library is 120 KB of MSL against 33 KB, and the median pixel library 118 KB against 13 KB. The text is plain code. The raw global memory helpers, the bindless lookups, the HMMA blocks and the task and mesh payload are each below 1% of it. What fills the text:

| part | share of the text before the changes |
|---|---|
| declarations of the locals (`float v12 = 0.0f;`, one per IR value) | 25 to 32% |
| arithmetic, constant buffer reads and casts | 35 to 50% |
| copies `v = v` (the lowering of phi values) | 3 to 7% |
| compare scaffolding (`.FTZ` flush, `isnan`) | 2 to 5% |
| a vertex library: the whole main function once for each of `main0`, `main0_z`, `main0_gs` (and `_l`, `_lz`) | 2/3 of the library |

These choices keep the text short:

- A local that has one assignment and no read before it, inside the block of the assignment, has no initial value, and the locals of one type and value share a declaration (`msl/decls.rs`). The lean geometry kernel reads the declarations line by line and keeps the old form (`Options.plain_decls`).
- The entries of a vertex library call one function `dm_body_main0`. It returns `out` without the depth fix. Each entry adds only its tail: the depth fix, the layer, the capture of `main0_gs`. `main0_discard` and `main0_tf` keep their own body.
- A `.FTZ` comparison flushes an operand only when the other operand cannot decide the result for a denormal as it does for zero. A NaN, an infinity or any normal constant is such an operand, so a comparison with a literal needs no flush (`flush_denormal_pair`).
- The constant folding folds `isnan` of a constant. The pass matched the integer form of the instruction only, so the float form never folded, and `isnan(0.96f)` stayed in the text.

The effect on a sample of 190 shaders of Ocarina of Time: the `_b` text is 44% smaller (vertex 75%, pixel 20%, compute 24%). The optimized IR of a vertex library is 50% smaller, of a pixel library 0.4% smaller. The `metallib` stays as large as it was (within 2%), because `metallib` inlines the body function into every entry again, and Apple's back end sees the same code for a pipeline. The gain is in the front end: the own front end and `air-opt` take about 3 times less time for a vertex library, and the translation store and the `.metal` files shrink. Compile time of a pipeline and the size of the library can only fall with fewer operations in the entry that the pipeline uses.

## Identity and the cache

The step stores each translation in the store. The key `nxb.xlate.v2` has the translator identity, the job, the code, the chain version V, the source, the paired shader,
the answer-table flag, the dual-source and fetch-outputs-only flags and the varyings of the partners.
The translator identity is a hash of the label `nxb-xlate msl`, V, `nxb_xlate::msl::TRANSLATOR_ID` (now `msl-2.19`) and `NXB_TEST_XLATE_SALT`.
V is a hash of the engine files that `dm/gfx/tools/dmsh_chain.py` lists in `FILES`, and the options string `debug=True reduced=True`. No Rust source goes into V.
A change of the translator translates again only when someone increases `TRANSLATOR_ID` by hand. Without the increase, the store gives the old MSL. A failure is not stored.
A change that alters the MSL only of shaders that use one feature of `nxb_xlate::msl::revision::FEATURES` (now `hmma`, `meshpos` and `blendz`) increases that feature's revision instead of `TRANSLATOR_ID`. The key of a shader whose code uses the feature has the field `features` (`hmma=1`, `meshpos=1`, `blendz=1`), so only these shaders translate again. A shader that uses no feature has no such field.

## 64 bit floats

Metal has no `double`. The retry loop of `nxb_xlate::driver::retry` translates a job again with `softfloat64` set when the structured program uses 64 bit floats.
The passes `opt/double_to_float.rs` and `xform/softfloat.rs` then lower them to 32 bit. The back end never sees a 64 bit float after the retry.

## Output

The MSL is not byte-identical to the output of the former C# translator and `spirv-cross`. No game ran with it yet. Unit tests and Metal compile tests on built samples cover it.
The own front end and the Apple tools compile the MSL as before.

## Geometry, task and mesh shaders

A geometry shader runs as a compute kernel. One job of stage `gs` gives the libraries `_b` (a copy of `_bg0`), `_bg0` to `_bg4` and `_bgv`. Each has its own `.json`.
The kernels `_bg0` to `_bg4` are one per input topology. They write the output vertices and the index buffer. The library `_bgv` draws that output.
The `.json` of a kernel has the member `geometry` (`vertices`, `invocations`, `output`, `index_stride`, `stride` in words). The `.json` of `_bgv` has `geometry_reader`, `geometry_output` and `entry_z`.
A passthrough geometry shader also gets the lean kernels `_bl0`, `_bl1` and `_bl3`, one for each input topology without adjacency. The engine uses a lean kernel when it has one and the full kernel `_bg<t>` when not.
A lean kernel writes only the viewport mask, one word per output vertex, and the index buffer. The host then draws the vertices with the game's vertex shader.
It reads only the capture words that it uses. These words are a record of their own, in the order of `capture_reads`.
Its `.json` is the `.json` of `_bg<t>` with the key `<key>_bl<t>`. In `geometry`, `stride` is 1, `lean` is true and `capture_reads` lists the capture words. `drv_reads` keeps only the driver constants that the lean kernel reads.
The lean kernel is a transform of the MSL of `_bg<t>`. The output stores and the capture reads must use literal offsets. If they do not, the shader gets no `_bl<t>` and `notes` of `_bg<t>` gives the reason (`lean geometry: ...`). For `_bl0` the note is also in `_b`.
A geometry job with `tf` gives `_bgv` the entry `main0_tf` like a vertex job, with `entry_tf` and `tf_strides` in its `.json`. The capture takes its vertex from `dm_tf.order[record]`, so the records follow the order of the kernel output.

A task or mesh shader is a job of stage `cs` with the member `mesh_stage`. The code has no SPH. The job carries `stage_memory`, `shared` and `local` as the engine lays them out,
and the translator refuses a job whose layout differs. The body of the shader is one function. The wrapper is a Metal `[[object]]` function for `ts` and a `[[mesh]]` function for `ms`.
The `.json` has `stage` `ts` or `ms`, `msl_entry_kind` null and `mesh_stage` (`kind`, `threads`, `payload`, `task`). A mesh function also has `entry_z`.
The translator fails with `mesh stage failed` when the shader uses shared memory of its own or the sizes do not fit Metal.

The attributes of a mesh shader are all the blocks of 32 vertices, then all the blocks of 32 primitives. A vertex block has a word per vertex for each component of the SPH. The point size (byte 0x35, bit 3) is the first component, ahead of the position. It becomes `[[point_size]]`.
A primitive block has the components of the per-primitive SPH. A mesh of 128 vertices and 64 primitives has four vertex blocks, then two primitive blocks.
The control section (+0x5f) gives the vertices of a primitive. 1 is a point, 2 is a line and 3 is a triangle. This sets the topology of the Metal mesh and the index bytes of a primitive. Another value stops the build with `mesh stage failed`.

The stage memory ends with the primitive bytes. The count is at +3. The index bytes start at +4. The memory reserves 4 plus the vertices of a primitive times 32 bytes for each block of 32 primitives of `max_p`.
The guest writes the index bytes of each lane in each primitive block, and a count of 0 makes every lane write. A mesh of 126 primitives has four blocks, so lanes 126 and 127 stay inside the array.

A mesh shader has pixel shader partners, as a vertex shader has. The split pairs a mesh key with the pixel key of its variation. A pixel shader may read a location that the mesh shader does not write. Metal then refuses the pipeline. The mesh function declares such a location as an output `dm_pad<N>` with the value (0, 0, 0, 1). The hardware reads the same default.
The position of the mesh function is `[[position, invariant]]`, as the position of a vertex shader is. A game draws one geometry with two mesh shaders, a depth pre-pass and a colour pass with the depth test EQUAL, and Metal gives equal depths from two shaders only for an invariant output. The `meshpos` feature revision marks the mesh shaders (`ms`) for this change.
A mesh output is as wide as its pixel shaders read. Metal counts the declared outputs against a mesh limit of 32 KB, so a `float4` for every attribute can be too much. An output has the components that the shader writes, and a w that a pixel shader reads and the shader does not write. That w holds 1.
A Metal fragment input wider than the output it links to reads 0 in the missing components. This is the hardware default for x, y and z. A `StageInputs` varying carries `read`, the components that its pixel shader reads. The bits come from the interpolation map of the pixel shader. A map that names no component counts as all four. Without partners every output is a `float4`.
The pipeline archive leaves the pairs of a mesh shader out. The host builds mesh pipelines at the draw.

## Open points

- The MSL back end is the part that fills `translate`. A shader that it does not translate gets `Failure` with the reason `not ported` until then.
