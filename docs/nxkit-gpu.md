# nxkit-gpu

`nxkit-gpu` turns a held NVN frame into Metal 4 work. The input is the `Frame` and the `PassPlan` of `nxkit-nvn`, the resource table and the descriptors. The output is command buffers on the Metal 4 queue of `nxkit-metal`. The crate is layer L2, builds on macOS only and has no `unsafe` code. It knows no game and no engine. Everything a title decides arrives as an argument: the scale, the shader directory and the thread count.

## The backend

`GpuBackend` implements `nxkit_nvn::backend::Backend` over `Gpu` and the present path of `nxkit-metal`. The host makes it with `GpuBackend::new(&ctx, guest_memory, &GpuSettings)` and installs it with `nxkit_rt::nvn::install`. `GpuSettings` has the shader folder, the optional pipeline archive, the thread count and the render scale (2.0 by default, so a 1080p game renders at 4K).

A frame goes through these steps:

1. `Backend::submit` gets the commands of a recording. The walker (`walk/`) reads the command words of the guest and adds one `DrawDetail` per draw to the `FrameDetail` of the frame.
2. `nvnQueuePresentTexture` or a CPU wait flushes the frame. `Backend::flush_frame` takes the detail, checks that it holds as many draws, clears and dispatches as the frame, runs `encode_frame` and applies the `EncodeReport` to the resource table. A count that differs is logged and the frame is encoded without detail.
3. `Backend::present` takes the texture that the game presented and puts it in a queue of at most three entries. It calls `Pacer::game_frame_ready`.
4. The display link calls the `Pacer` on each tick. The `Pacer` is the `TickSink` of the backend. For a tick that shows a frame, `tick` takes the queued texture, draws it into the drawable with a full-screen triangle (`blit.rs`, nearest sampling, with an sRGB variant) and presents the drawable at `target_presentation_timestamp`. A tick that holds or misses drops the drawable.
5. `Backend::acquire` blocks while three frames are queued. `Backend::wait_fence` waits for the last batch of the encoder.

The blit is a second command buffer on the queue. Its first command is `WaitForEarlierPasses`, which orders it after the passes of the frame.


```rust
let mut gpu = Gpu::with_config(&ctx, guest_memory, GpuConfig { shader_dir, threads, archive, gob_order })?;
gpu.register_pool(gpu_addr, len)?;           // a texture or sampler pool of the game
gpu.texture_created(id);                     // the game made or finalized a texture again
gpu.set_icb_candidates(handles);             // from RecCache::icb_candidates
gpu.recording_dropped(handle);               // the cache dropped a summary
let report = gpu.encode_frame(&frame, &plan, &table, &descriptors, &detail, scale)?;
let stats = gpu.wait()?;                     // GPU time of the last frame
```

`Gpu::new(&ctx, guest)` uses `GpuConfig::default()`. `Gpu::metrics()` gives the `Metrics` of `nxkit-metal` with CPU encode time and GPU time of every command buffer (R16). `Gpu::reload_shaders` forgets programs and ICBs.

`encode_frame` returns an `EncodeReport`. The caller owns the resource table, so the caller applies the report: `ResourceTable::synced` for each id in `deswizzled`, `gpu_wrote` for each id in `gpu_wrote`, and `synced` for each id in `copied_back` after `wait`. The counters are in `report.stats` (`EncodeStats`). A command that was not encoded is counted by reason in `stats.skipped` (`Skip`), so a missing feature is a number and not a silent hole (R15).

## What the recorder supplies: `FrameDetail`

`Frame` holds what the analysis needs and not what a draw binds. `nxkit_gpu::detail::FrameDetail` carries the rest. It has one `DrawDetail` for each `Cmd::Draw` (program keys, blend, depth and stencil, raster state, topology, vertex attributes and streams, index buffer, uniform buffers, texture handles, viewport, scissor, blend colour and stencil reference), one entry for each clear, copy and dispatch, and the runs of draws that came from a reused recording. The walker of the backend fills it from the recordings. It is a second pass over the command words, next to the recording that the front end made for the frame analysis.

## From NVN state to Metal

1. `translate.rs` maps blend, depth, stencil, raster, topology, samplers and vertex formats. The enum values come from `nvn.h` of nxnative (listed in `THIRD_PARTY.md`).
2. `format_map.rs` is the one table from NVN formats to Metal formats. A format that Metal lacks maps to a wider one or to a uint format that a copy kernel fills. The row says which.
3. `programs.rs` loads the Metal library of a shader hash and its `_b.json` reflection from the shader directory (R1). The reflection gives the entry name, the binding list and the compute threadgroup size.
4. `pipelines.rs` builds a `RenderPipeline` for the translated state and keeps it in the `PipelineCache`. The key is a hash of the program keys, the colour formats with blend and write mask, the vertex layout, the topology class, the sample count and alpha-to-coverage. Two draws with the same translated state share one pipeline, whatever their NVN state was. Depth and stencil are separate objects that Metal binds per draw.

## Bindless textures and buffers

A translated shader has one argument buffer per stage at `[[buffer(0)]]`. Member `arg_id` is at byte `8 * arg_id` and holds a GPU address or a resource ID. The encoder writes into it:

- A `cbuf` binding gets the GPU address of the uniform buffer in guest memory (R3). A buffer that the guest rewrote in the same frame is copied into the per-batch `Scratch` first, because the frame is encoded after the guest wrote all versions. Every other buffer is bound by its guest address.
- A `tex` binding gets the resource ID of the texture view that the handle names (`texture_id | sampler_id << 20`). A `samp` binding gets the resource ID of the sampler. A `samp_tex` binding gets both.
- A `tex_pool` or `samp_pool` binding gets the address of a host table that maps descriptor ids to resource IDs. The encoder writes the entries of every handle that the frame uses.
- A `drvcb` binding gets the driver constant block: the combined texture handles, the base instance and, for compute, the dispatch size and the uniform buffers.

No bind call runs for each texture. Vertex streams are bound with `SetBuffer` at the stream slots, with the guest address and the offset. The vertex and fragment stages have separate argument tables.

## One ICB for a reused recording (R17)

`RecCache::icb_candidates` names the recordings that are stable. A run of draws from such a recording is encoded once into an `IndirectCommandBuffer` and executed with `ExecuteIcb` in later frames. A run qualifies when it has two draws or more, its viewport, scissor, stencil reference and blend colour are the same for all draws, and it binds no uniform buffer that the guest rewrote. The ICB holds pipeline, argument buffers, vertex buffers, depth stencil, bias, clip, winding, fill mode and the draws. The encoder sets viewport, scissor, stencil reference and blend colour outside the ICB. The signature of an ICB is a hash of the draw details, the resolved resource IDs and the targets. A different signature builds the ICB again. `recording_dropped` removes it. The state after an `ExecuteIcb` is treated as undefined, so the encoder sets its state again.

## Mesh and indirect draws

The walker details every draw kind but transform feedback and `DrawTexture`. `DrawArgs` has the indirect address, the index buffer, the count buffer, the maximum and the stride of each kind. A mesh draw names its task, mesh and fragment shaders in `ProgramRef` and binds the buffers of the task and mesh stages (banks 3 and 4 of the walker, the slots of the tessellation stages that the recorder gives NVN2 stages 6 and 7) and the textures of the bank they share. A `BindProgram` with stage bit 6 or 7 sets the task or mesh slot. The task and mesh stages read the addresses of their storage buffers from the c0 tables at 0x670 and 0x770 (`mesh::storage_table`, called from the driver block fill).

- `DrawArraysIndirect` and `DrawElementsIndirect` draw with Metal's indirect draws straight from guest memory. NVN's argument records have the layouts of `MTLDrawPrimitivesIndirectArguments` and `MTLDrawIndexedPrimitivesIndirectArguments`.
- `MultiDraw{Arrays,Elements}IndirectCount` and `DrawMeshTasksIndirect` go through a compute pre-pass (`indirect.rs`) before the render pass. It copies the records of a multi-draw, zeros past the GPU-written count, and turns the `{count, first}` of an indirect mesh draw into Metal's `{x, y, z}` grid with the first workgroup after it. The render pass draws from the copies. The CPU never waits for the GPU. An ICB would need the same kernel and GPU encoding of the whole draw state, so the pre-pass is the simpler choice. A multi-draw copies at most 4096 draws.
- A mesh draw runs a Metal mesh pipeline (`Pipelines::mesh`, built in the background like a render pipeline, `skip.pending` until it is ready). The task shader is the object function, the mesh shader the mesh function. Metal needs a fragment function, so a program without a pixel shader gets an empty one and draws depth only. The mesh stage shares the argument table of the vertex stage and the object stage has its own. The first workgroup of the draw is at buffer 29 of the stage that starts the grid.
- Without a task shader the grid is the mesh grid, at most 1024 threadgroups wide. A larger draw is cut into grids of 1024 with their own first workgroup (at most 64 grids). An indirect draw without a task shader gets 16 grids from the pre-pass, and those past the count are empty.
- A render pass with an indirect or mesh draw waits for the compute pre-pass before its vertex, object and mesh stages. The buffers it reads (arguments, count, index and storage buffers) are buffer hazards of the barrier planner. With `oldbarriers` it waits for all earlier dispatches, copies and fragment work instead.

`EncodeStats::kinds` counts these draws by class, encoded and skipped by reason, with the Metal draws they became, the pre-passes and their records, and what was clamped (`NXKIT_DIAG=mesh`).

## Barriers only where data flows (R19)

`barriers.rs` plans one `Dependency` per pass, and only when the pass has a real hazard with earlier work. `hazards.rs` tells it what each pass touches: the stages that bind each sampled texture (from the draw details) and the buffer ranges by guest address. A pass that touches nothing that an earlier pass touched gets no barrier. The state carries over to the next frame: the passes have global numbers, so the first pass of a frame waits for the last writer of the previous one. `EncodeStats` has `barriers_emitted` and `barriers_avoided`.

The rule for each kind of hazard. The `after` stages are the stages of the earlier passes that wrote (or, for a write after a read, read) the resource. The `before` stages are the stages of this pass that touch it. Nothing else is held.

| hazard | after | before | visibility |
|---|---|---|---|
| attachment written, then sampled by fragment shaders | fragment | fragment | device |
| attachment written, then sampled by vertex (or task, mesh) shaders | fragment | vertex (object, mesh) | device |
| attachment loaded or written again (load action `Load`, blending, a second pass into it) | writer's stages | fragment | device |
| sampled texture, then written as attachment | the stage that sampled | fragment | none |
| dispatch writes an image or a storage buffer, a later dispatch reads or writes it | dispatch | dispatch | device |
| dispatch writes a storage buffer, a draw reads it (storage, uniform, vertex, index or indirect buffer) | dispatch | the stage that binds it | device |
| a draw reads a buffer, a later dispatch or copy writes it | the stage that read | dispatch or blit | none |
| copy writes a texture or a buffer (`TextureToBuffer`), a later pass uses it | blit | stages of that pass | device |
| copy reads, a later pass writes | blit | stages of that pass | none |

Buffers are followed by ranges of guest address (`Ranges`, disjoint, touching ranges merge, so the state is coarser and never wrong). A storage buffer counts as written when the reflection of the shader says `rw` (a binding without it counts as written); a dispatch or a draw that only reads it is a read. Uniform, vertex and index buffer reads are rounded to 4 KiB pages. Uniform buffers with a snapshot are CPU data and not followed.

A barrier orders the work of its own pass only. The planner keeps, for each pair of stages, the newest point up to which a stage of some earlier pass already waits for the stage of the passes before it. A hazard whose writer is covered that way adds nothing to the `after` set of the barrier, so the barrier waits for as few stages as it can. A pass with no hazard of its own needs no barrier, and a pass with a hazard needs its own barrier even when an earlier read of the same data had one: the earlier barrier ordered only that pass.

`NXKIT_KNOB=oldbarriers` restores the planner of before. `NXKIT_DIAG=barr` prints the hazards of the frame next to the barriers in force.

## Tile memory

The load and store actions and the memoryless choice come from the `PassPlan`. A render pass takes them as they are. An attachment that the plan marks memoryless and that no command samples, copies, presents or reads back gets a memoryless texture, with store action `DontCare`. The pass merging of the analysis (a clear of an untouched attachment, a `SetRenderTargets` that keeps its attachments) stays one Metal render pass.

## Parallel encoding

The passes of a frame are cut into runs of neighbouring passes with similar weight. Each run goes into its own Metal 4 command buffer and its own thread (`jobs.rs`, a small job system on `std::thread`, R10). The first command buffer does the texture uploads. The batch commits the buffers in order. A pass encodes in a run with its own argument tables. Barriers refer to earlier passes by queue scope, so they also cover earlier command buffers of the same queue.

## The render target scale (R19)

`encode_frame` takes a scale. The caller derives it from the title's `upscaler` answer. For native 4K at a 1080p game it is 2.0.

A texture is scaled when all of these hold.

- The scale is not 1.
- A render pass had the texture as an attachment, in this frame or an earlier one. The game creates its targets at output size and renders into them, so every texture rendered into becomes scaled.
- No CPU read of the texture has been seen. A CPU read pins the texture at its own size for good, because guest memory holds the unscaled bytes.

A texture that the game only uploads is never an attachment and keeps its size. Viewports, scissors and copy regions between scaled targets scale with the factor. A copy between a scaled and an unscaled texture is not encoded and counts as `Skip::Copy`. A texture that becomes scaled is created again and its old contents are lost.

## Textures

`textures.rs` makes one Metal texture for each `TexId` and keeps it while its description stays the same. A texture in `GuestNewer` state, or a new texture in `InSync` state, is deswizzled from guest memory by the `Deswizzler` in the first command buffer before the frame uses it. A scaled texture whose guest copy is newer cannot take that upload, because the deswizzle writes the guest layout at the native size. See "Guest writes to a scaled texture" below. Render targets stay in Metal (R4). `Gpu::encode_frame` copies a texture back to guest memory with the `Swizzler` only when a `CpuRead` command names it and the GPU wrote it, in this frame or before. A scaled, compressed or widened texture is not copied back.

### Guest writes to a scaled texture

When the CPU writes a texture that the GPU renders at the scaled size (`Residency::GuestNewer` and `Res::scaled`), the data takes two steps (`scaled.rs`, called from the upload loop of `encode_frame_inner`).

1. The plain upload (`Transfer::upload`) deswizzles all levels and layers into a staging texture of the native size. The staging textures come from a pool by `TextureDesc`. A texture goes back to the pool when its frame is submitted. It is handed out again after `BATCHES_IN_FLIGHT` frames, when the batch ring has waited for the batch that used it, and it is dropped after 240 frames without use. A texture that the CPU writes every frame therefore makes no new Metal texture.
2. One compute pass at the end of the first command buffer waits for the earlier passes (`WaitForEarlierPasses`) and runs `nxkit_metal::Resampler` for each level and layer: a linear filter for colour formats that a shader reads as floats, the nearest texel for integer formats. The pass writes the scaled texture with compute stages, and the barrier planner is seeded the same way as for the plain upload (`Stages::DISPATCH | Stages::BLIT`).

The whole texture is scaled, not only the part the CPU wrote, because the resource table does not say which levels changed. The kernel needs a target with `shader_write` and a 2D or 2D array kind. Depth, stencil, compressed, 3D and shared-exponent formats have none, so a CPU write to such a scaled texture is still lost (`upload.scaled-lost`). A texture that became scaled this frame and is only `InSync` is not uploaded: it starts empty, as before.

A scaled upload counts as `uploads` and `scaled_uploads` (`upload.scaled` in the report). It is not a loss.

The store also keeps a texture at the native size when the CPU writes it much more often than the GPU renders into it (`Heat` in `textures.rs`). For each texture it counts the frames with a newer guest copy (`writes`) and the frames with an attachment use (`renders`) while the scale is not 1. When `writes >= 8` and `writes / 4 >= renders`, the texture is native for good, until the game creates it again. Then it takes the plain upload and no scale. This is a heuristic, not a fact about the game: the numbers are a first guess, and `textures_kept_native` counts the decisions. A texture that is written and rendered in about every frame stays scaled.

## Diagnostics

The encoder has opt-in diagnostics (`NXKIT_DIAG`), knobs for experiments (`NXKIT_KNOB`) and a render health report that counts every draw, upload, view and copy that was dropped (`NXKIT_DIAG=health`). [diagnostics.md](diagnostics.md) lists them all.

## Measurement (R16)

`EncodeStats` counts draws, skipped commands by reason, ICB executions and builds, pipeline switches and builds, barriers emitted and avoided, render and compute passes, command buffers, textures created, uploads, copy-backs and the CPU time of the encode in nanoseconds. The `Metrics` of the batch ring hold the GPU time of every command buffer.

## Cost of the walk

The walk runs on a helper thread beside the decode of the same submit, and the game thread waits for the longer of the two (`walk-join` in `NXKIT_DIAG=frame`). `NXKIT_DIAG=walkprof` prints, for each submit, the time in reading records, in the other records, in building the detail of draws, in snapshots and in allocation (see `docs/diagnostics.md`).

What keeps the walk short:

- Snapshots (the copies of the uniform and storage buffers that the overlap takes) come from a pool of buffers by size. The pool is a queue in the order of issue. Frames are dropped in that order, so the front buffer is the first that is free, and the check is one look. A frame that needs more buffers of one size than the pool keeps (2048) allocates the rest.
- The newest snapshot of each range `(address, size)` is kept for the frame. A range that comes again in a later submit of the frame is read aside and compared. When the bytes are the same, the draws share the buffer (the preload of the encoder uploads one block), and no pool buffer is used.
- The knob lookups (`overlap`, `nosnap`, `nosnapsbuf`) are read once per submit, not once per bound buffer.
- A snapshot buffer is an `Arc<[u8]>` per range, not a slice of one block per frame. A block that several `Arc` share needs `unsafe` or a changed `UboDetail`, and the consumers (`bindless.rs`, the preload in `gpu.rs`) read `Arc<[u8]>`. The pool gives the same saving in the steady state: no allocation, no page faults.

The walk of one submit stays on one thread. A recording starts from the state that the recording before it left, and an `UpdateUniform` of one recording changes the bytes that the snapshots of the next one read, so the recordings are not independent. A split would need the final state of the first half before the second half starts.

`DrawDetail` keeps five vectors and an `Arc` per draw. They are shared with the encoder, so the walker cannot reuse them. The `banks equal` counter of `walkprof` shows how many banks of a draw equal those of the draw before. It is the figure to read before a dirty bit per bank is worth its code.

## Limits

The walker leaves some of the detail out:

- `FrameDetail.copies` and `runs` stay empty. A copy uses the whole first level and no draw runs as an ICB.
- Images are not in the detail. Transform feedback and `DrawTexture` draws are placeholders without a binding.
- An indirect draw puts base instance 0 into the driver block, because the GPU reads the real one with the arguments.
- The arguments of an indirect draw must be final when its render pass starts. A draw of the same pass that writes them through a storage buffer is not seen.
- The multisample state reads only alpha to coverage.
- `UpdateUniform` writes into guest memory when the walker reads it. A frame that the GPU still runs can read the new data.

These are known gaps. Each shows up as a `Skip` count or is a limit of the first version.

- Partial clears (a scissor cuts the target) are not encoded.
- Triangle fans, quads, adjacency and patches, and byte indices, are not translated.
- Multisampled attachments are downgraded to one sample. The pass descriptions do not resolve.
- The binding kinds `cbuf`, `tex`, `samp`, `samp_tex`, `tex_pool`, `samp_pool` and `drvcb` are filled. A program with another kind is skipped (`Skip::Binding`). The shader variants `_bv`, `_bp`, `_bx` and `_bf` and pull shaders are not used.
- The texture view has no channel swizzle (counted as `swizzle_ignored`). A cube view ignores the layer range.
- A texel fetch with integer coordinates and `textureSize` see the scaled size of a scaled target.
- The pool tables are written while earlier frames may still run. A frame in flight can read an entry that a later frame rewrote.
- An unbound pool slot reads as zero.
- The depth format is not part of the pipeline key.

## Unverified

Each item names the check.

| claim | check |
|---|---|
| The pool table layout (16 byte head, 8 bytes per element) matches the shaders | compile a shader with a pool binding and read the index expression in its Metal source |
| The driver constant block fill is complete (`C2_TEX`, dispatch size, compute uniform buffers, base instance) | compare with the constants of `nxb-xlate` `abi.rs` for each shader that uses `drvcb`, and run a game |
| A Metal 4 argument table keeps the values set before a draw | draw twice with different tables in one pass and read the result |
| A barrier at queue scope covers earlier command buffers of the same queue | run a render to texture in one command buffer and a sample in the next |
| A barrier inside a render pass comes after the attachment load | render with a load and a barrier and read the result |
| The viewport y origin of NVN matches Metal | render a known triangle |
| The channel order of the 4 bit formats and the block copy of compressed formats into uint textures | upload a known texture of each format and read it back |
| The state after `ExecuteIcb` is undefined as assumed | compare a pass that runs an ICB and then draws without setting state |
| The types that the encoding threads share are `Sync` | the first build |

The code has not been compiled or run. The first build and the first frame of Breath of the Wild will show the remaining errors.

## Numbers that shaped the design

Super Mario Bros. Wonder frame 7200 on nxnative has 601 draws in about 11,000 records and 565 recordings. 62 percent of the draws are in recordings called more than once, which is the case for ICBs. Binds are the largest group: 1,976 vertex buffers, 1,848 small uniform buffers and 1,365 textures, which is the case for bindless tables and guest-address binds. There are 67 render target changes and 217 program switches. nxnative needs 4.5 ms of 4.7 ms CPU for translation, 14,700 full barriers and 591 split submits for one frame. The target here is to encode the whole frame in parallel with few barriers and re-run the reused recordings from ICBs.
