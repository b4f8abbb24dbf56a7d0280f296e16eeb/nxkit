# nxkit-gpu

`nxkit-gpu` turns a held NVN frame into Metal 4 work. The input is the `Frame` and the `PassPlan` of `nxkit-nvn`, the resource table and the descriptors. The output is command buffers on the Metal 4 queue of `nxkit-metal`. The crate is layer L2, builds on macOS only and has no `unsafe` code. It knows no game and no engine. Everything a title decides arrives as an argument: the scale, the shader directory and the thread count.

## The backend

`GpuBackend` implements `nxkit_nvn::backend::Backend` over `Gpu` and the present path of `nxkit-metal`. The host makes it with `GpuBackend::new(&ctx, guest_memory, &GpuSettings)` and installs it with `nxkit_rt::nvn::install`. `GpuSettings` has the shader folder, the optional pipeline archive, the thread count and the render scale (2.0 by default, so a 1080p game renders at 4K).

A frame goes through these steps:

1. `Backend::submit` gets the commands of a recording. The walker (`walk/`) reads the command words of the guest and adds one `DrawDetail` per draw to the `FrameDetail` of the frame.
2. `nvnQueuePresentTexture` or a CPU wait flushes the frame. So does a CPU event signal that releases held submits with at least 64 draws, dispatches and copies (`FlushReason::Release`; `NXKIT_RELEASE_FLUSH=0` turns it off, `NXKIT_RELEASE_FLUSH_MIN` sets the 64): a game that is serial per frame signals the event when its main work is recorded and presents some ms later, and the GPU would idle until then. The release flush runs no held op, its number is the fence of the released submits (a fence of a held queue is made again when its op runs), and the present flushes what comes after. `NXKIT_DIAG=frame` prints the flushes by reason with the mean time from the start of the flush to the first commit and from there to the GPU start. `Backend::flush_frame` takes the detail, checks that it holds as many draws, clears and dispatches as the frame, runs `encode_frame` and applies the `EncodeReport` to the resource table. `encode_frame` streams the command buffers: a frame with at least 8 passes gets a small lead chunk (one eighth of the weight), and the first command buffer (uploads) and the lead chunk are committed as soon as they are encoded, so the GPU starts while the other chunks are still encoded (`Batch::stream_prefix`, one more commit boundary of about 0.2 ms; `NXKIT_STREAM_COMMIT=0` turns it off; not with `NXKIT_PRIVATE=1`, which cuts at its own signals). A count that differs is logged and the frame is encoded without detail.
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

1. `translate.rs` maps blend, depth, stencil, raster, topology, samplers and vertex formats. The enum values come from `nvn.h` (listed in `THIRD_PARTY.md`).
2. `format_map.rs` is the one table from NVN formats to Metal formats. A format that Metal lacks maps to a wider one or to a uint format that a copy kernel fills. The row says which.
3. `programs.rs` loads the Metal library of a shader hash and its `_b.json` reflection from the shader directory (R1). The reflection gives the entry name, the binding list and the compute threadgroup size.
4. `pipelines.rs` builds a `RenderPipeline` for the translated state and keeps it in the `PipelineCache`. The key is a hash of the program keys, the colour formats with blend and write mask, the vertex layout, the topology class, the sample count and alpha-to-coverage. Two draws with the same translated state share one pipeline, whatever their NVN state was. Depth and stencil are separate objects that Metal binds per draw.

### Archive coverage

`Pipelines` asks the archive of the build first (`PipelineCache::render_traced`), then compiles. `coverage.rs` counts the answer for each run: archive hits, misses with the compile time, and, with `NXKIT_DIAG=stats`, `health` or `pipe`, why each miss missed (`PipelineCache::explain_miss`). The numbers are in the `stats` report and the reason `pipeline.archive-miss` is in the health report. Refer to [diagnostics.md](diagnostics.md#archive-coverage). The cache does not write or learn pipelines at run time: everything has to be in the archive when the game starts. The archive is not read whole: `GpuSettings::archive_shards` names the folder of the batch archives and the index, and the cache opens a shard for the first pipeline in it (`NXKIT_SHARD_LRU` open at most). See docs/shader-step.md, "The pipeline archive as shards".

### Specialization, background builds and the runtime cache

Plans: [plans/pipelines-at-build-time.md](plans/pipelines-at-build-time.md), [plans/pipeline-prewarm.md](plans/pipeline-prewarm.md).

- **Specialization.** The archive holds one unspecialized base for each shader pair. Metal 4 turns a base into a pipeline for a colour state (formats, blend, write mask) without a compile (`cache/specialize.rs`). A base of a point-size vertex function is made without a primitive class. `NXKIT_PIPELINE_SPECIALIZE=0` never specializes. A base that the archive lacks is compiled only with `NXKIT_PIPELINE_BASES=1` (off: one base per vertex layout was as rare as its pipelines, measured on one title, and its compile took 0.1 to 10 s where the full compile took 1 to 6 ms).
- **Background builds.** A missing pipeline builds on threads (half the cores, 2 to 8, `NXKIT_PIPELINE_THREADS`) at user-initiated QoS (`NXKIT_PIPELINE_QOS`). A draw of the latest frame takes the job first. The draw is left out until the pipeline is ready (`skip.pending`). `NXKIT_PIPELINE_ASYNC=0` builds in the encode. `NXKIT_DIAG=pipe` shows the backlog.
- **Runtime cache and prewarm.** `pipelines/record.rs` captures every pipeline into `rt-*.mtl4archive` files and writes the descriptions into `pipelines.seen`. `prewarm.rs` makes the list before the first frame (budget 20 s, `NXKIT_PIPELINE_PREWARM_WAIT`). Details: [shader-step.md](shader-step.md) "The runtime pipeline cache". Assumption not yet checked: that the serializer writes only the new pipelines on each flush.
- **Vertex pulling.** A vertex shader with a `pull` field in its `_b.json` draws through its `_bp` library. The encoder fills a `dm_vlayout` block (for 16 streams the GPU address, stride and divisor, and 16 attribute words) and binds it at buffer 16; the pipelines have no vertex layout, so one archived base per pair serves every layout. It is off unless `NXKIT_PULL=1`; otherwise `stage_in` stays. `NXKIT_DIAG=pipe` counts pulled and `stage_in` draws. Built and tested; not yet measured in a game.

## Bindless textures and buffers

**One copy per batch slot.** The CPU changes the tables while earlier frames may still run on the GPU (`BATCHES_IN_FLIGHT` = 3 batches), so a table in one place would let a frame read an element that a later frame cleared or rewrote. The host keeps a master of each table (`tex_mirror`, `samp_mirror`: the non-zero resource ID of each element, with `n`) and one copy of both tables for each batch slot. `publish`, `clear_texture`, `release_gone` and the `n` raises write the master only and note the ID in a log (`bindless/changes.rs`). Each slot has a cursor into the log. After the batch ring has waited for the previous batch of the slot (`BatchRing::begin`) and the frame has made its last write to the master, `Bindless::sync(slot)` copies only the elements noted since that slot last synced, and `n` when it changed; a slot that fell behind a cut log (1 Mi entries) gets its whole table. The log keeps only what the slowest slot has not taken, a few hundred entries in the steady state. `Bindless::pools(slot)` gives the addresses that the argument buffers of the frame carry. The bitmap `seen` is shared by all copies: shaders only set bits in it, they never read it for a lookup. The copies cost 3 times the table: 8 MiB for the texture table (2^20 elements) and 32 KiB for the sampler table (4096 elements) each, 24 MiB in all, of which only the touched pages are resident.

An argument buffer that a recorded ICB keeps holds the address of the table of the slot that built it. An ICB whose draws have a `tex_pool` or `samp_pool` binding is therefore kept once per slot (`icbs.rs`, key of recording and table address) and replays only in a frame of the same slot; an ICB without such a binding is shared.

A translated shader has one argument buffer per stage at `[[buffer(0)]]`. Member `arg_id` is at byte `8 * arg_id` and holds a GPU address or a resource ID. The encoder writes into it:

- A `cbuf` binding gets the GPU address of the uniform buffer in guest memory (R3). A buffer that the guest rewrote in the same frame is copied into the per-batch `Scratch` first, because the frame is encoded after the guest wrote all versions. Every other buffer is bound by its guest address.
- A `tex` binding gets the resource ID of the texture view that the handle names (`texture_id | sampler_id << 20`). A `samp` binding gets the resource ID of the sampler. A `samp_tex` binding gets both.
- A `tex_pool` or `samp_pool` binding gets the address of a host table that maps descriptor ids to resource IDs. The encoder writes the entries of every handle that the frame uses, the textures that shaders read through the table and every registered sampler (see "The pool tables" below).
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
- A render pass with an indirect or mesh draw waits for the earlier dispatches, copies and fragment work before its vertex, object and mesh stages, because the barrier planner follows textures only.

`EncodeStats::kinds` counts these draws by class, encoded and skipped by reason, with the Metal draws they became, the pre-passes and their records, and what was clamped (`NXKIT_DIAG=mesh`).

## GPU counters and render conditions (`queries`)

NVN has no query objects. A game resets a counter of a queue (`nvnCommandBufferResetCounter`), draws, and has the GPU write the counter into a 16-byte `NVNcounterData` (`nvnCommandBufferReportCounter`: the 64-bit counter, then the 64-bit GPU time in report ticks). An occlusion test reports samples passed before and after its boxes, at `addr` and `addr + 16`, and `SetRenderEnableConditional` compares the two counters later (`nxkit_nvn::rendercond`).

- The front end (`nxkit_nvn::counters`) cuts the commands of each queue that uses a counter into slots, which end at every reset and report of the queue. `Frame::counters` holds the command ranges of the slots and, for each report, the slots it sums: the slots of its queue since the last reset of its counter in the frame, or since the start of the frame. A report across frames thus starts again at 0 in each frame; the begin and end reports of a test are in one frame.
- `queries::prepare` gives every render pass with draws of a slot that counts samples one offset per slot in a zeroed visibility result buffer (a small pool of shared buffers). No two passes share an offset, so the passes of the parallel command buffers never add to the same 8 bytes. The pass gets the buffer with `MTLVisibilityResultTypeAccumulate` (`CommandBuffer::begin_render_pass_counted`) and each draw sets the offset of its slot (`RenderCommand::SetVisibility`, `MTLVisibilityResultModeCounting`); a draw of no slot turns counting off. A recording run with a counted draw is drawn one by one, not as an ICB.
- The encoder counts the draws it encoded per slot. A slot with a draw that was not encoded (a pipeline still compiling, a lost pass) is not counted, so its reports do not land and a condition does not hide an object because its boxes were not drawn.
- Vertices (`INPUT_VERTICES`, `VERTEX_SHADER_INVOCATIONS`) and primitives (`INPUT_PRIMITIVES`, `CLIPPER_INPUT_PRIMITIVES`, `PRIMITIVES_GENERATED`) are added up on the CPU from the arguments and topology of the draws of the slots (vertex shader invocations without the post-transform cache). An indirect or mesh draw makes the slot unknown. The other counter types (the invocations of the other stages, tessellation and geometry primitives, transform feedback, zcull) are unknown.
- When the GPU finished the frame (`GpuBackend::write_counters`, at the wait for its fence), every report whose slots were all counted gets its `NVNcounterData` in guest memory (counter and the GPU end time of its own fence) and lands in `nxkit_nvn::counters::Landed` with the sequence number of the report. All 16 bytes must lie in guest memory (`queries::write_report`), the same check that the timestamp and `ReportValue` writes get. `ReportValue` writes its 32-bit value zero-extended as the counter, as the GPU writes the payload of a four-word report.
- A report right after a reset of its counter in the frame, with no draw in between (the begin report of a test), is 0 and known when it is recorded.
- A render condition reads a counter once landed: the value of the report the table names when it landed, else the last value that landed at the address (the report of an earlier frame: one frame of latency), else the last value that landed at the same place of the report in an earlier frame (its queue and its number among the reports of the queue in the frame: a game that moves its query memory every frame reports the same tests in the same order), else the condition is unknown and renders. `NXKIT_RENDERCOND_STALE=addr` drops the place, `NXKIT_RENDERCOND_STALE=0` both fallbacks, `NXKIT_COUNTERS=0` turns the emulation off (every GPU counter unknown again, as before). `NXKIT_DIAG=rendercond` counts the conditions that read landed and stale counters, the reports by counter type, and what the backend wrote.

### The time of a report

- A tick of the time in a report depends on the console (`Platform::report_ticks_from_ns`). On the Switch it is 625/384 ns (614.4 MHz). On the Switch 2 it is a system tick, 32 ns (31.25 MHz), and `nvnDeviceGetTimestampInNanoseconds` multiplies by 32. The host takes the console from the `sdk` module at load (`nxkit_abi::platform`). There is no setting per game.
- A report carries the times of its own batch. The batch of a frame (fence) is kept until the GPU finished it. A `TIMESTAMP_TOP` (kind 0x10) then gets the GPU start of that batch, every other timestamp its GPU end. Frames that finish in one wait each get their own times, so the difference between the reports of two frames is the time between their batches.
- A time is never 0 and never goes back. The host keeps the last time written. A batch without GPU work (an empty frame) takes it, and a batch that starts before the end of the one before it (overlap) starts there. Within a frame, `TIMESTAMP_TOP` is at most `TIMESTAMP`.
- The base is the host clock that Metal reports GPU times on (`mach_absolute_time`). It is the base of the system tick and of `nvnDeviceGetCurrentTimestampInNanoseconds`, so a game can compare a report with the current time.
- The times are the start and end of the whole batch. A report in the middle of a frame has the times of the frame, not of its position.
- The reports are written in the host wait that drains the frame, with the clock lock held, so a wait that returns for a fence has the reports of that fence in guest memory. With the overlap on, a wait for the newest frame returns when the frame before it is done: a game that reads the reports of the newest frame sees the old values.
- `NXKIT_DIAG=gputs` prints each report written and each read of `nvnDeviceGetTimestampInNanoseconds` (`docs/diagnostics.md`).

The counter type numbers (`SAMPLES_PASSED` 1, the statistics 2 to 0xf, `TIMESTAMP_TOP` 0x10) follow the order of `NVNcounterType`; the per-type counts of `NXKIT_DIAG=rendercond` show which ones a game reports.

## Barriers only where data flows (R19)

`barriers.rs` tracks, for each texture, the stages that wrote it and the stages that read it. Before a pass it emits one `Dependency` only when the pass reads something that an earlier pass wrote, or writes something that an earlier pass wrote or read. The `after` stages are the stages of the earlier writers and the `before` stage is the first stage that needs the data (fragment for a render pass that samples in the fragment stage). A pass that touches nothing that an earlier pass touched gets no barrier. The state carries over to the next frame, so the first pass of a frame waits for the last writer of the previous one. `EncodeStats` has `barriers_emitted` and `barriers_avoided`.

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

A texture that the game only uploads is never an attachment and keeps its size. Viewports, scissors and copy regions between scaled targets scale with the factor. A copy between a scaled and an unscaled texture is not encoded and counts as `Skip::Copy`. A texture copy whose destination box has another size than the source box (`nvnCommandBufferCopyTextureToTexture` with two region sizes) stretches the source onto it with a host kernel in the copy pass (`passes/scale.rs`): destination texel `d` takes the source at `(d + 0.5) * src_size / dst_size`, the nearest texel, or with `NVN_COPY_FLAGS_LINEAR_FILTER_BIT` (bit 0 of `CopyDetail::flags`) a bilinear mix clamped to the source box. Integer formats always take the nearest texel. Depth, stencil, compressed and 3D textures, a source a shader cannot read and a destination it cannot write (`shader_write`) are refused with a reason (`DIAG copy`, `skip.copy`). The health report counts the layers it stretched as `copy.scaled` and `copy.scaled-linear`. A texture that becomes scaled is created again and its old contents are lost.

### Shaders at a render scale

A guest shader works in the pixel units the game made its targets in. The translator inserts the conversions (refer to [translator.md](translator.md)), and the host writes the factors into the support buffer of each stage (`bindless.rs`, `support_block`). Entry 0 of `render_scale` is the scale of the pass: `PassCtx.scale`, 1 when the pass renders at native size. Entry `rscale` of a `tex` binding is the scale of the texture in that slot: `GpuSettings.scale` when the texture is scaled (`Resolved.scaled`), else 1. The pixel position is thus divided by the pass scale, and integer texel fetches and size queries use the scale of their texture. `frag_scale_count` stays 0.

When every factor of a stage is 1 (always so at `scale` 1), the stage binds the shared static support buffer and nothing changes at native size. Otherwise the stage gets a 1392 byte copy from the batch scratch buffer. The argument buffer memo of a pass includes the pass scale.

Limits:

- Only the pixel position, integer coordinates and size queries are converted. `uv = gl_FragCoord.xy * invSize` works, as the position is divided and the constant is a native size. A size that the shader gets by another path (a uniform the game wrote from the size of a scaled target, a compute thread id) stays native or scaled as it is.
- A texture that is not a 2D texture, an array of textures, a bindless handle (`tex_ubo`, pools) and a combined handle that the shader reads at run time keep scale 1. The shader then sees the scaled texture with native coordinates.
- Image bindings are filled (see "Storage images and raw global memory"). The translator scales image loads, stores and atomics, and entry `rscale` of an `img` binding is the scale of its texture. A store of a native coordinate writes one texel of each `scale` by `scale` block. Dispatch sizes that the game computes from the native size then cover only part of a scaled image. A rule that scales the grid when every written image has the same factor can use the image binds of the detail now, but there is no `scalegrid` knob yet.
- A copy-back and an upload of a scaled texture keep their limits (see above).

## Textures

`textures.rs` makes one Metal texture for each `TexId` and keeps it while its description stays the same. A texture in `GuestNewer` state, or a new texture in `InSync` state, is deswizzled from guest memory by the `Deswizzler` in the first command buffer before the frame uses it. A scaled texture whose guest copy is newer cannot take that upload, because the deswizzle writes the guest layout at the native size. See "Guest writes to a scaled texture" below. Render targets stay in Metal (R4). `Gpu::encode_frame` copies a texture back to guest memory with the `Swizzler` only when a `CpuRead` command names it and the GPU wrote it, in this frame or before. A scaled, compressed or widened texture is not copied back. A texture that the kernel cannot write is deswizzled into a staging buffer first and copied into the texture with a blit (`upload/buffered.rs`): a compressed texture (BC, ASTC), because Metal cannot copy an integer texture into a compressed one, and a `Depth16Unorm`, `Depth32Float` or `Stencil8` texture, which has no integer view and no shader write (the shadow map). A 3D level goes in whole, with all its depth slices. `Depth32Float_Stencil8` has no upload. One buffer holds all levels of a texture, each at a 256 byte aligned offset, and `upload/staging.rs` pools the buffers (a buffer is free when only the pool holds it, up to 8 buffers, sizes rounded to a power of two up to 4 MiB). The CPU does not clear them, because the kernel writes every byte that the copy reads.

A game that encodes block compressed textures on the GPU renders into them through a view of an unsigned integer format of the block size (agl's BC4 encoder renders a 64x64x64 BC4 texture plane by plane through an `RGBA16Uint` view). Metal can neither render into nor make an uncompressed view of a compressed texture, so such a colour attachment renders into a block shadow (`textures/blocks.rs`): a 2D texture of the view format with one texel per block, kept per texture, level, layer or depth plane and view format. After the pass `encode/blocks.rs` copies the shadow into the blocks of the compressed texture through a buffer, and before a pass that loads the attachment the other way. The barrier plan counts the blit stage as a writer of a compressed attachment, and a compressed texture is never a render target and never scaled. An attachment view of a compressed texture is never made.

### Guest writes to a scaled texture

When the CPU writes a texture that the GPU renders at the scaled size (`Residency::GuestNewer` and `Res::scaled`), the data takes two steps (`scaled.rs`, called from the upload loop of `encode_frame_inner`).

1. The plain upload (`Transfer::upload`) deswizzles all levels and layers into a staging texture of the native size. The staging textures come from a pool by `TextureDesc`. A texture goes back to the pool when its frame is submitted. It is handed out again after `BATCHES_IN_FLIGHT` frames, when the batch ring has waited for the batch that used it, and it is dropped after 240 frames without use. A texture that the CPU writes every frame therefore makes no new Metal texture.
2. One compute pass at the end of the first command buffer waits for the earlier passes (`WaitForEarlierPasses`) and runs `nxkit_metal::Resampler` for each level and layer: a linear filter for colour formats that a shader reads as floats, the nearest texel for integer formats. The pass writes the scaled texture with compute stages, and the barrier planner is seeded the same way as for the plain upload (`Stages::DISPATCH | Stages::BLIT`).

The whole texture is scaled, not only the part the CPU wrote, because the resource table does not say which levels changed. The kernel needs a target with `shader_write` and a 2D or 2D array kind. Depth, stencil, compressed, 3D and shared-exponent formats have none, so a CPU write to such a scaled texture is still lost (`upload.scaled-lost`). A texture that became scaled this frame and is only `InSync` is not uploaded: it starts empty, as before.

A scaled upload counts as `uploads` and `scaled_uploads` (`upload.scaled` in the report). It is not a loss.

The store also keeps a texture at the native size when the CPU writes it much more often than the GPU renders into it (`Heat` in `textures.rs`). For each texture it counts the frames with a newer guest copy (`writes`) and the frames with an attachment use (`renders`) while the scale is not 1. When `writes >= 8` and `writes / 4 >= renders`, the texture is native for good, until the game creates it again. Then it takes the plain upload and no scale. This is a heuristic, not a fact about the game: the numbers are a first guess, and `textures_kept_native` counts the decisions. A texture that is written and rendered in about every frame stays scaled.

### Copy-back of render targets (`copyback.rs`)

A texture that a frame writes is copied back into guest memory at the end of the frame only when the CPU may read it: its storage lies in a memory pool without `NVN_MEMORY_POOL_FLAGS_CPU_NO_ACCESS` (0x1). The encoder keeps the pools with their flags (`Backend::register_pool`) and, since `nvnMemoryPoolMapVirtual`, the virtual windows (`Backend::map_pool_window`, `unmap_pool_window`).

A game builds its render target heap as a virtual pool (`VIRTUAL | CPU_NO_ACCESS`) mapped onto a physical pool (in Ocarina of Time flags 0x18c: `PHYSICAL | COMPRESSIBLE | GPU_NO_ACCESS | CPU_CACHED`). Both name the same bytes at the same addresses here. Textures are created through the virtual pool, so its flags are what the game may do with them. Before the window was registered only the physical pool was known, its flags lack `CPU_NO_ACCESS`, and about 18 textures (53 MB, about 2 ms of GPU time) were copied back each frame that nothing reads. A window is registered with the flags of its virtual pool for exactly the mapped range (`phys + poff`, `size`) and is newer than the physical pool, so it wins for those addresses. It is dropped when the range is unmapped (a request without a physical pool) or the virtual pool is finalised; a physical pool is never touched by that.

Decision for a GPU-written texture (`CopyBack::why`, in this order):

1. `NXKIT_COPYBACK=0`, a frame-local texture, a texture the game presents, and every format that is not plain colour (depth, stencil, block compressed) are not copied back (`off`, `frame-local`, `presented`, `format`).
2. No pool holds the whole storage: not copied back (`no-pool`).
3. The newest pool that holds all of it has `CPU_NO_ACCESS`: not copied back (`window-cpu-no-access` for a virtual window, `pool-cpu-no-access` otherwise).
4. Else, if the storage straddles several windows, none holds it whole, the windows without CPU access together cover every byte and no window the CPU may read touches it: not copied back (`window-cpu-no-access`).
5. Else it is copied back (`copied`), also when the pool it lies in has `GPU_NO_ACCESS` or `COMPRESSIBLE`. `GPU_NO_ACCESS` says that the GPU does not use that pool itself, not that the CPU cannot read it, and the CPU can read a physical pool through its `CPU_CACHED` mapping: a copy-back that is not needed costs GPU time, one that is missing loses what the game reads.

`NXKIT_DIAG=copyback` prints `DIAG copyback decision <TexId> fmt .. WxH at .. size ..: <reason> (pool flags ..)` for the first decision of each texture and for each change of it (at most 1200 textures), and `DIAG copyback decisions: copied N, ..` with the textures counted by their latest reason before each summary line.

## Storage images and raw global memory

### `img`

The walker follows `BindImage` into `State::image` (the slot layout of the textures: bank times 32 plus the index) and puts the bound handles into `DrawDetail::images` (vertex and fragment, or task, mesh and fragment) and `DispatchDetail::images`. A recording that binds an image, or draws while one is bound, is not a walker template (`Track::truncated`). The handle is a texture id without a sampler.

The reflection gives an `img` member its slot (`reg`), its type, its access (`access::read`, `write` or `read_write` in `msl_type`), an `atomic` flag and `rscale`. `images.rs` pairs the two for each stage that has image binds:

1. Before the textures of the frame are made, `images::uses` finds the texture of each image. They count as used and not memoryless, and a texture with atomics gets `Use::atomic` for good (`TextureStore` remembers it).
2. After the handles are resolved, `images::resolve` makes one view for each (handle, declared type, atomic code) with `TextureStore::image_view`: one level (the base level of the descriptor), the layers of the descriptor, no swizzle, in the declared type (`texture2d`, `texture2d_array`, `texture3d`). The resource ID goes into `Env::images`; `fill_args` writes it. An image that cannot be bound (nothing bound, a texture a shader cannot write, a view Metal refuses) writes resource ID 0 and counts `bind.img-unbound`.
3. Atomics. Metal runs texture atomics only on `R32Uint` and `R32Sint` textures made with the atomic usage. `nxkit_metal::Texture::new` gives that usage to every shader-writable texture of those formats. With `TextureDesc::shader_atomic`, a 4 byte colour 2D or 2D array texture of another format (RGBA8, BGRA8, RG16, R32F, RGB10A2, ...) is stored as `R32Uint`, and its own format is a view over it, so rendering, sampling and the guest bytes see the same data. `Texture::atomic_view` gives the `R32Uint` or, for a shader that declares `int`, `R32Sint` view. The texture is made again, and its contents are lost, when the first atomic image use is found; a texture that atomics ran on stays atomic.
4. Barriers and write tracking. A dispatch already lists every bound image as written (`Cmd::Dispatch::writes`). A draw does not, so `images::pass_images` collects, for each render pass, the textures its draws write (an image whose shader access is not `read`, or that has atomics) and read through images. The barrier planner treats them as writes and reads of the pass, and `gpu_wrote` and the copy-back dirty set include them (`EncodeReport::gpu_wrote`).

### `gmem_map` and `gmem_log`

A shader that reads a guest pointer from memory has no storage buffer for it. The translator searches the table of the memory pools and accesses `address + delta` through a physical pointer, and each store sets a bit in a write log.

The table has 8192 entries of 32 bytes, sorted by start and disjoint: start (u64), end (u64, exclusive), delta (u64), a hint (u32), 4 bytes of padding. Entry 0 is empty, unused entries start at `!0`. A pool that overlaps an earlier one keeps its part past the end of the earlier one. The hint of entry `chunk & 8191` is `chunk << 13 | entry index` for each 2 MiB chunk (`chunk = address >> 21`) that lies wholly inside an entry, 0 for none. The delta is the Metal GPU address of the guest memory buffer minus the guest GPU address, from the `AddressMap`: 0 in nxkit, where the GPU address of the game is the Metal address. The pools are those of `register_memory_pool`. The host keeps every registered pool until its removal arrives (a removal takes the oldest registration with that start), also when pools overlap or share a start, and the table is their union. The table is built again, into a new buffer that the batches keep alive, when one comes or goes. A pool beyond entry 8191 is left out and counted (`gmem.pools-over`), and so is a pool that the pool snapshot does not resolve (`gmem.pool-unresolved`); raw loads into either read 0.

The log is a bitmap of 8192 bits (1024 bytes): bit `i` of word `i / 32` is set by a store through entry `i`. Each draw or dispatch with a `gmem_log` binding takes a zeroed log from a ring of 1024 logs per batch slot (`gmem.logs-shared` counts the draws past that; they share the last log). When the batch ring gives the slot out again, the GPU is done with it: `Gmem::learn` reads the logs against the table that the batch bound, clears them and adds the pools to `Gpu::gmem_written_pools`. A pool is large, and the log does not say where in it a store landed, so the host does not mark textures from it. Guest memory is the buffer itself, so a store reaches it at once, and a CPU read sees it after the fence.

The bindings are counted as `bind.img`, `bind.gmem-map` and `bind.gmem-log` (not losses). `NXKIT_DIAG=gmem` prints the table size, the registrations, those that overlap an earlier one, those left out and each pool learned.

### The pool tables

A shader that indexes the whole ID range of the driver pool has a `tex_pool` or `samp_pool` binding. The member is a pointer to a table that the host keeps (`nxb-xlate`, `msl/pool.rs`: `strukt`, `element` and `MARK`). The layout is the contract:

| bytes | field |
|---|---|
| 0 | `n` (u32): the highest ID that a read can name |
| 8 | `seen` (u64): the GPU address of a bitmap |
| 16 + 8 * id | the resource ID of the texture or sampler of the descriptor `id` |

A read clamps its index to `n`. A texture read first calls `dm_pool_mark`: it sets bit `id & 31` of word `id >> 5` of the bitmap that `seen` names (with an atomic OR, and only when the bit is clear). There is no null check, so `seen` of both tables is always the address of a bitmap: 131072 bytes for the texture ids and 512 bytes for the sampler ids. Generated code never marks a sampler read; the second bitmap only makes the address valid. The host never sets a bit.

`n` of the texture table is the highest ID of the texture pools that the game made. It is not the highest ID the host filled: a read above it would clamp to a lower ID and mark that one, and the host would never learn of the ID the shader wanted. The elements that the host has not filled hold 0, the null texture. `n` of the sampler table is the highest registered sampler ID. Neither shrinks.

**The wanted set.** Once a frame, before the handles are resolved, the encoder reads the words of the texture bitmap up to the highest ID of the pools (`Bindless::wanted`, `bindless/marks.rs`). Two generations of bits are kept: `current`, the bits set since the last sweep, and `previous`, the bits that the last sweep took. An ID is wanted while it is in either. Every 60 frames the sweep copies `current` to `previous` and clears the bits it copied with an atomic AND (`DataBuffer::fetch_and_u32`). A bit that a shader sets at the same time is not lost: the AND clears only the bits that were read, and a bit that was cleared is set again by the next read, because the shader tests it first. An ID that no shader reads leaves after two sweeps.

**The fill.** When a draw or dispatch of the frame has a program with a `tex_pool` binding, the encoder resolves every wanted ID after the bound handles: the slot of the descriptor, the record of its texture, `TextureStore::ensure` and `TextureStore::view`, the same steps as for a bound handle. It writes the resource ID of the view into the element and keeps the view in `Handles::keep`. The Metal texture is uploaded like any texture of the frame. The sampler table is not touched by a mark. An ID that has no slot or whose texture is dead gets the element 0 and counts `pool.unresolved`. ID 0 is the unbound slot and is skipped.

A shader that reads an ID for the first time sets its bit in that frame and reads the old element (the null texture, or what the ID held before). The host sees the bit in the next frame, so the ID shows its texture one frame later. With the encoder thread the snapshot of a frame carries the slots of the IDs that were wanted when the front end captured it, so a new ID can take one frame more. The pool path makes the view of the descriptor and does not fit it to the type the shader declares (a cube read as 2D); Metal reads a texture of another type as one fixed value.

**The samplers.** A sampler read is not marked, so the host fills every registered sampler. `Descriptors::sampler_epoch` changes when `register_sampler` sets a slot. When it is newer than the epoch the table holds, the encoder makes each sampler with `Bindless::sampler` (one Metal sampler for each distinct header) and writes its element. A texture registration does not change the counter.

**The lifetime.** No element may hold the resource ID of a view or texture that is gone. The store drops and replaces Metal textures when it evicts, supersedes or remakes them (`end_frame`, `ensure`, `invalidate`), and a view lives as long as the generation of its Metal texture (`trim_views`). The table remembers, for each element that is not 0, the `TexId` and the store generation behind it. `Gpu::pool_release` writes 0 into every element whose texture has no entry in the store or has another generation, and forgets it. It runs at the start of the pool step and again after `trim_views`, so the command buffers of the frame never read a dropped texture. A wanted ID is filled again by the next frame. The rule covers the elements that a bound handle published too.

The counters are `bind.tex-pool` and `bind.samp-pool` (a binding written into an argument buffer), `pool.wanted` (the wanted IDs of a frame with a pool binding), `pool.filled` (elements written because of a mark), `pool.unresolved` and `pool.cleared`. `tex_htab` and `samp_htab` (a table of guest handles) are still refused.

## Renamed storage ranges under the overlap (`rename`)

With the overlap on (`NXKIT_OVERLAP=1`, `overlap` in `game.json`) the CPU writes the data of frame N+1 while frame N runs on the GPU. The uniform buffers and the read-only storage buffers of a frame read a copy that the walk takes. A storage buffer that a shader writes cannot be copied that way, and two small ranges of a game are written by both sides: a word that a compute shader adds into (a compare-and-swap loop) and that the CPU reads and resets to 0 after its wait, and a block of coefficients that the CPU initialises in full every frame and that compute shaders accumulate into. With the overlap alone the CPU's writes for the next frame met the sums of the frame before. The host gives each frame a copy of such ranges, found by watching the game, so that no word that the GPU wrote lives on into the next frame. Nothing below runs, and nothing is allocated, with the overlap off.

**Learning** (`rename::Learn`). A range that is bound as a storage buffer that its shader may write (`rw` of the binding; `Renamer::note_rw` looks at it when the frame is encoded) and is at most 64 KiB is a candidate, one per start address. For each frame that binds it, the host keeps three copies of its bytes: `pre` at the flush of the frame (on the CPU thread, in `flush_frame`), `post` after the GPU finished that frame, and `pre_next` at the flush of the next frame. The words that the GPU wrote are those where `post` differs from `pre`. The frame is clean when `pre_next` differs from `post` in each of them (the CPU wrote them again), dirty when one of them stayed (state that the GPU keeps), and quiet when the GPU wrote nothing. A range becomes `Rename` after 8 clean frames, and `Never` after one dirty frame. A quiet frame neither counts nor resets. After 32 quiet frames in a row the range rests for 300 frames and is learned again. Only one flush in 16 (one in 120 after 300 flushes in which no candidate with a new address appeared and no range changed its class, until one does) takes `pre` (and the flush after it takes `pre_next`), so only one wait in 16 is a full one. The game allocates some of its storage ranges from rings, so addresses come and go and learning never ends for them; a candidate that no frame binds for 60 frames (a renamed one: 3000, a `Never` one: 600) is forgotten.

`post` is read in a CPU wait, when the GPU has finished the newest flushed frame and no later flush has happened (`Renamer::after_wait`); a frame for which that is not so is not judged. In the frame that takes `pre` the wait is a full wait (`Renamer::honest`): with the one-frame overlap the wait for the newest frame returns while that frame still runs, and `post` would be read too early. The ranges that are judged in that frame are bound to guest memory as before. A renamed range is checked again every 300 frames at the next such frame (the frame is bound to guest memory with a full wait; a renamed range is renamed in every other frame); a dirty frame turns it into `Never`. The line `DIAG overlap rename <addr> <size> Rename|Never after <n> frames` is printed once for each decision.

**Capture** (`Renamer::at_flush`, from `flush_frame` on the CPU thread, before the frame goes to the encoder). Each `Rename` range that the frame binds has its guest bytes copied into the `Capture` of the frame (`FrameDetail::rename`): the `start` bytes. The range is left out of the frame (bound to guest memory as before, counted in `overlap.rename-excluded`) when a copy or fill, a vertex, index or indirect buffer or a uniform buffer overlaps it (`rename::plan`), when a storage binding reaches out of it (a binding inside it is fine), or when it overlaps another renamed range.

**Binding** (`Renamer::begin_slot`, from `encode_frame` after the batch ring waited for the slot). The `start` bytes go into a shared buffer that belongs to the batch slot of the frame, one per slot like the pool tables. It is made when needed, grows, and is kept alive by the batch. A range that lies in a pool that raw global writes touched (`Gmem::learned`) is left out here, because a shader can store through a raw pointer to any place of it. Every storage binding of the frame that lies inside a renamed range, read-only or not, in every stage, reads the copy at the same offset: `RenameMap::resolve` in `bindless::args::sbuf_word`, the one place where a storage buffer's guest address becomes a GPU address. The signature of an ICB includes the copy address of each storage binding, so a recording that binds a renamed range is encoded again when its slot changes.

**Write-back** (`Renamer::after_wait`). Only in a CPU wait, and only for the frames up to the fence that the wait returned for: after `finish_batches` in the two waits of the overlap and in `drain` of a wait or of `wait_idle`, and in a wait that finds the fence done already. The frames that a wait covers are written back in the order of their fences. For each word that the GPU changed (the copy after the GPU differs from `start`) the host swaps it into guest memory with an atomic compare-and-swap (`GuestSpace::compare_exchange_u32`, `rename::decide_word`) that needs the word to still hold `start`. A word that the CPU wrote since the flush stays (`overlap.writeback-skipped`). A word that still holds what the write-back of the frame before wrote takes the new end value if a CPU wait lies between the flushes of the two frames, and otherwise the old value plus what this frame added (read as `f32`): on the console the GPU of the second frame adds onto the sum of the first when the CPU has not read and reset it in between, and a game that waits for only every second frame (so one wait covers two frames) reads both sums. This is how the CPU reads the sum of the frame it waited for and resets it. It is never done at bare completion of a batch: the CPU's reset would be overwritten and the next frame would count the sum twice. The `drain` that the encoder thread makes for a copy-back is not a CPU wait and writes nothing back. A frame whose slot is taken again before a wait asked for it is written back first, in `begin_slot`.

**Snapshots** (class `Snapshot`). A storage buffer that no shader of any frame writes (no `rw` binding in any stage; it is also not a copy or fill destination, and not in a pool that raw global stores touched) and that is at most 1 MiB is watched (class `Watch`). In the frame that is judged (one in 16, a full wait) the host takes its bytes at the flush (`pre`), at the start of the wait for the frame (`atwait`, `Renamer::before_wait`) and when the GPU is done (`post`). Words that differ between `pre` and `atwait` and stay until `post` are CPU writes while the frame ran. One judged frame with such words makes the range a `Snapshot`. A range that the CPU writes only in some frames (while the player moves) is not given up for a quiet judged frame: it is judged at every judged flush, and only after 30 quiet ones it rests for 120 flushes. Per-frame instance data that the CPU rewrites for the next frame is the case. A snapshot gets the same capture at the flush, the same slot buffer and the same redirection of every storage binding as a renamed range, and no write-back. It is left out of a frame when it exceeds 1 MiB or the snapshots of the frame exceed 8 MiB together (`overlap.snapshot-over-budget`, `overlap.snapshot-bytes` counts the bytes copied). If a shader is found to write it (a binding with `rw`, found at the encode of the frame, which then binds the guest range), a copy or fill writes it, or a pool with raw global stores holds it, it becomes `Never` at once. The bytes that the judged ranges of one flush read are limited to 16 MiB.

**Classes by binding** (`rename::Key`). The game allocates some of its storage ranges from rings, so a range has a new address every few frames while its binding stays the same. A class is therefore also kept for the binding: the program key, the stage and slot, and the size rounded up to 256 bytes (the masks are relative to the start of the range). When a range gets a class (`Rename`, `Snapshot`, `Merge`, `Conflict`), the keys of the bindings that bound it learn it (a range that is `Never` for its own reasons, such as a pool with raw stores, teaches nothing; a snapshot binding that a shader is found to write becomes `Never` for its key). At the flush every binding of the frame is looked at (`State::bindings`, the same few hundred buffers dropped by a small table): a range that is not known and whose key has a class gets that class at once (`State::adopt`), with the masks of the key, and is not judged for 600 flushes; a new address of a known key needs no learning frame. A key is taught `Never` only after 30 quiet judged frames; a new address of such a key is judged at the next judged flush and, if the CPU writes it, becomes `Merge` at once and teaches the key. If several keys bind one range it gets what all of them need: a snapshot binding asks for nothing, a `Never` key does not override, a merge and a renamed range cannot be combined and the range becomes a `Conflict` (the frame waits in full, `overlap.key-disagree`). The flush sends the encoder only the bindings that it could not place (`FrameDetail::rename_check`): new addresses of unknown keys, and bindings of a range that was only read whose key the encoder has not found read-only yet (`State::rw_known`). `note_rw` then costs a few lookups in a frame. Judging a range at its address remains how a key is learned. A range of a settled class that passed the check against other uses (copies, uniform, vertex, index and indirect buffers) in 8 flushes in a row is not checked again except once in 60 flushes.

**Merge ranges** (class `Merge`) and **conflicts** (class `Conflict`). A written range that the CPU also writes while a frame runs is judged on a copy (see below) on the judged frames for two sets of words, kept as bit masks over the judged frames: `C`, the words that the CPU wrote in flight, and `G`, the words that the GPU wrote. One probed frame with a CPU write in flight makes the range a `Merge` range (also when the GPU wrote nothing in that frame: the copy still keeps the frame's data from the CPU's next writes). A merge range stays one: its masks only grow, whatever later frames show. Its words fall in three sets: `R = C and G` (both write: the CPU rewrites the word every frame and the GPU then writes it, and the CPU may read the result, like a sum), `P = G and not C` (only the GPU writes: state that persists from frame to frame) and `C and not G` (only the CPU writes). A frame that binds a merge range reads and writes a copy in its batch slot (`rename::merge`, `overlap.merge-ranges`): at the flush the CPU thread captures only the words of `C`, packed, with the masks; on the GPU, a pass before the first pass of the frame (`nxkit_merge_in`, in the first command buffer) copies the guest range into the copy and lays the captured words of `C` over it; every storage binding of the range reads the copy; a pass after the last pass (`nxkit_merge_out`, in the last command buffer) writes the words of `P` from the copy into guest memory, so the state in `P` carries from frame to frame on the GPU and no word of the CPU is overwritten. The words of `R` are written back by the host in the wait that covers the frame, with the compare-and-swap and the accumulate rule of a renamed range (the guest word must still hold the captured value), so the CPU sees the GPU's value one frame late, as for a sum. A range with `R` empty is the earlier merge range, one with `P` empty the earlier renamed range; the renamed class (`Rename`) that is learned from the bytes at two flushes remains as the cheap case of a word that the CPU re-initialises between two frames, with the whole range captured. Both passes have a barrier from all earlier work and one for all later work. Words in neither mask are copied in and not written back. A merge range whose masks grow in more than 3 later judged frames is not understood: it becomes a `Conflict` (`overlap.merge-conflict`, counted once), and the wait for a frame that binds it is a full one. What cannot be seen: an `R` word that the CPU writes in flight after the GPU wrote it in the same frame looks like one written before; its GPU value is dropped. Ranges must be multiples of 4 bytes. If a range lies in a pool with raw global stores it becomes `Never` at once. The knob `conflictwait` makes every frame that binds a written range with CPU writes in flight (`Never`, `Merge` or `Conflict`) wait in full, to check whether such a range causes a defect.

**Judging a written range on a copy** (`Kind::Probe`). In a frame that is judged, a written range that is learned or checked (`Never`, `Merge`) is bound to a copy as well, so the GPU does not write guest memory while the CPU may: `nxkit_merge_in` fills the copy and a second copy of the start state from guest memory (empty `C` mask), and at the end of the frame `nxkit_probe_out` writes back every word where the copy differs from the start copy, as the GPU would have. After the GPU is done the host reads both copies: `G` is the words where they differ. `C` is the words where guest memory at the start of the wait differs from `pre` (only the CPU could write guest memory since the flush), except words that hold what the GPU wrote (a frame can end before the wait begins). A probe that gets no copy (another use of the range in the frame, a raw global pool) judges nothing and forces no full wait. The masks are the union over probed frames. Ranges must be multiples of 4 bytes. If a range lies in a pool with raw global stores it becomes `Never` at once. The knob `conflictwait` makes every frame that binds a written range with CPU writes in flight (`Never`, `Merge` or `Conflict`) wait in full, to check whether such a range causes a defect.

What the host cannot know: a game that writes its re-initialisation of a renamed range between its wait and the next flush gets the write-back of the frame before it in the middle (the swap needs the word to still hold `start`, and a word that the CPU zeroed holds the same value as `start`); the frame that follows then starts from the sum. A game that reads the range in a place other than after its wait, or a shader that reaches the range through raw global memory in a pool that was not yet seen to be written, is not seen either. The counters are `overlap.renamed`, `overlap.writeback-words`, `overlap.writeback-skipped` and `overlap.rename-excluded` (`docs/diagnostics.md`).

## Private GPU memory, step 1: tracking (`NXKIT_PRIVATE=track`)

With the overlap, the CPU writes the data of frame N + 1 while frame N runs, and the learned copies of `rename` do not
cover every range that both sides touch. A GPU-private copy of the guest pages that the GPU writes would remove the race
at its root: the GPU would work on the copy, the CPU would write guest memory, and the host would move bytes between the
two in queue order. Step 1 builds no copy. It measures what the dirty tracking that such a copy needs would cost and how
often the CPU touches a page while a frame that writes it is on the GPU. `NXKIT_PRIVATE=track` turns it on; unset, the
code is not reached: no signal handler is installed, no page is protected, and a guest memory access costs one extra
load in `nxkit-abi`.

**Pages.** The host page is 16 KiB, so the tracker (`nxkit-metal/src/pages.rs`) holds one atomic word per 16 KiB of guest
memory. A page is free (not tracked), clean (read only), dirty (read and write) or tagged with a frame (no access).

**At the flush of each frame** (`private::at_flush`, on the CPU thread) the host computes the pages the frame may write:
the storage buffer bindings of draws and dispatches whose shader may write them (the encoder reflects each program once
and tells the host; a program it has not reflected yet counts as writing, and so does a binding of a task or mesh
stage), the destinations of buffer copies and fills, and the pools that shaders wrote through raw pointers
(`Gmem::learned`, the whole pool). A single range above 256 MiB or a frame above 1 GiB is left out and counted. Then:

1. The pages that the CPU dirtied since the last flush go back to clean and read only (the state first, the protection
   second, so a store in between is either in guest memory already or faults again).
2. The pages of the frame's ranges are tagged with the frame and lose all access. They become tracked if they were not.
   Once tracked, a page stays tracked.
3. When the frame's batch is complete on the GPU (`SubmittedBatch::on_complete`, on the Metal completion thread), the
   pages still tagged with that frame become clean and read only.

**Faults.** The handler (`SIGBUS` and `SIGSEGV`, chained before the crash report) tells a read from a write by the write
bit of the exception syndrome, and acts on atomics only, plus `mprotect`:

- write to a clean page: the page is dirty, logged for the next flush, opened read and write;
- write to a page tagged with a frame that is not complete: counted as a **write in flight** with its program counter,
  then dirty and opened;
- read of such a page: counted as a **read in flight** with its program counter and opened read only (its tag stays, so a
  later write is seen);
- anything else (a race with a flush or a completion) is opened and counted as other.

Guest memory is the truth in this step: the handler never copies and never waits. That is what makes it safe to protect
a page against all access: the CPU reads the same bytes it would have read, a little later by the cost of the fault.

**Threads and system calls.** A thread that the host marked internal (the encoder thread, the code that writes counter
reports) is counted apart from the game's. A fault cannot hold a system call back: `read(2)` into a page without access
returns `EFAULT`. `nxkit_abi::guest::with_slice` and `with_slice_mut` (the paths that hand guest bytes to a system call)
and the permission changes of `nxkit-hos` (`protect`, `release`, `remap`, `drop_view`) call `before_host_access` first,
which opens the tracked pages in the range (`with_slice_mut` marks them dirty; a permission change or a release makes
them free). A system call that gets a guest pointer by another way is not covered.

**What the counters say, and their limits.** `DIAG private` (see `docs/diagnostics.md`) prints every 300 frames the
tracked pages and MiB, the pages a frame may write, the faults per frame by kind and by origin (guest code, host code,
internal threads), the time in the handler, the pages dirtied per frame, the `mprotect` calls, and the top 10 program
counters for writes in flight, reads in flight and writes to clean pages.

- "In flight" means: between the flush of the frame and the completion of its batch. With the encoder thread, that
  includes the time before the frame is encoded. With the honest wait, the game waits for the fence before it touches
  the pages and the counts are near zero; with the overlap they are the accesses a private copy would have to order.
- Only the first access to a protected page is seen. A page opened by a read in flight does not show the second read; a
  page opened by a write does not show the stores after it. The count is of pages touched, not of accesses.
- The program counter is the faulting instruction, which may be a `memset` or `memcpy` of the host or the game; the
  caller is not listed.
- A page becomes tracked when a frame binds it as a possible write, and stays tracked. A program that the encoder had
  not reflected when its first frame was flushed makes its read-only bindings tracked until the end of the run, so the
  tracked MiB is an upper bound.
- A frame whose batch is not reported complete (an encode that failed) is released when 24 newer frames wait.
- The frame number is kept in 28 bits (29 in step 1 before the mirror took a bit for "fresh").

## Private GPU memory, step 2: the mirror (`NXKIT_PRIVATE=1`)

Step 2 gives the GPU its own copy of the guest pages that it works on, so that the CPU's writes for the next frame cannot
race with the frame on the GPU. It changes no fence: the waits are the honest ones, the overlap and the renamer are off
(`overlap::on()` is false, the `overlap` knob and `NXKIT_OVERLAP` are ignored), and the picture and the frame rate must be
those of a run without the variable. What it proves is the mirror, the redirection of every GPU access, the copy of the
CPU's writes into the mirror in queue order, and the copy of the GPU's results back on a CPU access. Step 3 can then answer
a fence at the flush. Unset, nothing here runs. A value other than `track` and `1` leaves it off.

**The mirror** (`nxkit-metal/src/mirror.rs`). A host reservation as large as guest memory (an anonymous mapping without
access, no physical memory); byte `o` of guest memory has its mirror byte at `reservation + o`. The reservation is cut into
segments of 2 MiB. A segment becomes readable and writable when it holds a member page and stays so. Each maximal run of
adjacent usable segments has one no-copy `MTLBuffer` (`GuestMemory::from_raw_uncommitted`), so the GPU address of a mirror
byte is the real base of its run plus the offset in the run. When a run grows or two runs join, a new buffer is made over the
larger range of the same host memory (the bytes do not move). The old buffer joins the retired list and stays in the
residency set while a frame in flight, a cached ICB or the overlay of a frame holds it; `Mirror::ensure` drops it on the CPU
thread, never in a completion handler, and commits the residency set when it drops one. The encoder commits the residency set
before it submits a frame that uses a new buffer, as for the pools. The mirror of OoT should be 100 to 130 MiB of runs.

**Members.** A page is a member from the flush that first needs it. The pages that a frame may write (step 1: storage
bindings a shader may write or that are not yet known read only, buffer copy and fill destinations, texture to buffer
destinations, pools that shaders wrote through raw pointers) are members. So is every page of a binding that touches a member
(`private::closure`, two rounds): uniform and storage ranges, vertex streams (the extent an arrays draw reaches, 1 MiB for an
indexed draw), indices, indirect arguments, the count word of a multi-draw, the source of a buffer copy (1 MiB for the buffer of
a buffer to texture copy), the indirect group counts of a dispatch. A mirror binding must be contiguous and current, so a range
that touches a member becomes whole; the closure adds at most 256 MiB per flush. A range above 256 MiB and the part of a frame
above 1 GiB are not made members (counted). A page stays a member until the game releases or re-protects its memory
(`before_host_access` frees it, after copying the page from the mirror).

**At the flush** (`private::at_flush`, then `Pages::flush_mirrored`, on the CPU thread):

1. The segments that hold the members are made usable.
2. A page that becomes a member goes read only and its guest bytes are copied into the mirror (the protection first, so a
   store after it faults and is staged below or at the next flush). A page that raw stores wrote and that a frame in flight
   may still write in guest memory makes the flush wait for the frames in flight once (`enter waits`).
3. The pages that the CPU wrote since the last flush (write faults on read only pages) go back to read only, then their bytes
   are copied into a staging buffer (`Sink`, a pool of shared buffers that a buffer returns to when its frame is complete).
4. The pages of the written ranges are tagged with the frame and lose all access. A page that was staged is tagged fresh
   (guest memory holds the bytes the frame starts from). A page that the CPU wrote between 3 and 4 is staged again.
5. The pool snapshot of the frame gets an overlay (`overlay.rs`): the member bitmap, the mirror runs, and the staged pages.

**On the GPU.** The first command buffer of the frame starts with a compute pass (`nxkit_private_in`, `private::encode_copy_in`)
that copies each staged page from the staging buffer into the mirror, with a barrier from all earlier work (the frame before still reads and writes the
mirror) and one for all later work. It comes before the texture uploads, which may read member pages. The bytes for frame N + 1
land after frame N.

**Every site that turns a guest address into a GPU address** asks the overlay first, because `PoolSnapshot::resolve_start` and
`resolve` do. A range whose pages are all members resolves to the mirror: the `Loc` has the run, the offset in the run's buffer,
the real address and the room to the end of the run. A range that starts on a member page and reaches past the members, or one
that starts outside and reaches into them, is counted (`partial resolves`) and gives guest memory; the closure should make it
impossible. The sites:

- `bindless/args.rs` `locate`, used by `ubo_words` and `sbuf_word`: uniform buffers and storage buffers (the walk's snapshot of
  a range is used where there is one; the walker takes none for a range that a frame wrote, `walk/ext/uniform.rs`).
- `geometry.rs` `stream_loc`, `index_loc`, `word_loc`, `indirect_args`, `index_room`, and through them `indirect.rs`,
  `passes/indirect_dispatch.rs`, the multi-draw count and the indirect draws.
- `passes/buffers.rs` `real` (buffer copy and fill source and destination) and `passes/copy.rs` through `upload::locate` (the
  buffer side of texture and buffer copies).
- `upload/mod.rs` `locate` for every texture upload source and the scaled uploads: the buffer of the mirror run and the offset.
- `draws.rs` `pull::layout` (vertex pulling: the stream bases).
- `icbs.rs`: `icb_buffer` gets the buffer of a run (`PoolRefs::take` keeps it alive with the ICB), and the signature of an ICB hashes the
  mirror addresses of its uniform, storage, vertex and index bindings, so an ICB made over a run's buffer is built again when the run
  grew.
- `gmem.rs` `resolve_all`: the table of raw global memory splits every pool at its member pages: a member piece has the delta of its
  mirror run, the rest the delta of guest memory. The table is built again when the members or the runs change (the third part
  of its stamp is the overlay generation). The c0 tables of compute and mesh stages hold guest addresses that shaders look up
  in this table, so they reach the mirror too.
- CPU reads that feed the frame (`Shared::read_guest`): the 8-bit indices that are widened, the pulled indices of `NXKIT_DIAG=pull`,
  the `ubo` and `ubodump` diagnostics read member pages from the staged bytes or the mirror, and the rest from guest memory.

**What a CPU access does** (`nxkit-metal/src/pages.rs`, `serve_mirrored`). A page tagged with a frame holds stale guest
bytes. The signal handler (on atomics, `mprotect`, `memcpy` and the wake word) serves a fault on it:

- the frame is complete: the thread claims the page (a `sync` state), opens it, copies the page from the mirror (about 1 µs),
  and the page is clean (read) or dirty (write); the instruction runs again. Other threads that fault meanwhile sleep on the wake
  word and run their access again;
- the frame is in flight (the batch is not reported complete): a game thread sleeps on the wake word until the completion
  callback of the batch reports it (`SubmittedBatch::on_complete`, before the waiters of the batch wake), then copies. This is
  option (a) of the plan: the wait is for the whole batch, whichever pass writes the page. A write in flight is served the same
  way (wait, copy the page from the mirror, let the write land on the GPU's bytes), so the write is not lost and the GPU's
  bytes are not overwritten with stale ones. A wait that lasts 4 s copies anyway and is counted;
- a thread that the host marked internal (the encoder thread, the thread that writes counter reports, the game thread inside
  `flush_sync`) never waits. It gets the page open for reading: copied from the mirror unless the page is fresh (then guest memory
  is right). The completion of the frame takes the access away again. The encoder reads member pages through the overlay
  instead (`Shared::read_guest`, `Overlay::read`: staged bytes, else the mirror), so it does not fault; the internal open is the
  safety net, and counted (`internal opens`).

A page that the CPU writes while it is clean or synced goes dirty and is staged at the next flush. System calls that read or
write guest pages (`with_slice`, `with_slice_mut`, permission changes) go through `before_host_access`, which brings the pages
up to date first (for a system call in the host, which may wait for a writer).

**The knob `privpoison`** overwrites the guest bytes of the pages a frame writes with a quiet NaN (`0x7fc00000`) when they are
handed to the GPU. The CPU never sees it, because its access copies the page from the mirror first; a GPU path that still reads
guest memory shows it as NaN in the picture where something was missed. The overwrite opens the page for a moment: a CPU store in
that instant is lost, so use it for finding sites only.

**Counters** (`NXKIT_DIAG=private`, every 300 frames, `DIAG private mirror`): the mirror's MiB and runs and the buffers that wait
to be dropped; the pages staged per frame (and the ones the CPU wrote again after they were staged), the pages the copy-in pass
moved and lost, the pages copied from the mirror into guest memory (and how many by a fault of guest code), the pages the closure
added, the waits for a writer (count, milliseconds per frame, timeouts), the accesses of internal threads, the waits at the
flush, the partial resolves and whether the poison is on. The fault lines of step 1 follow, with the faults that copied a page
counted as other.

**Bisecting knobs.** `privnoubo`, `privnosbuf`, `privnostream`, `privnoupload`, `privnogmem` and `privnocopy` (read every frame)
bind one class of resolves to guest memory instead of the mirror (the overlay holds a class mask; `PoolSnapshot::resolve_c` and
`resolve_start_c` honour it). Guest memory is stale for the pages a frame wrote, so while any of them is on the tracker copies
every page of a completed frame back into guest memory at the batch completion (`Pages::set_eager_sync`), which makes guest
memory current and the knob isolate "mirror against guest" for that class. A page that the GPU did not write already equals the
mirror in the honest-wait mode, so a defect that stays with the knob on is not in that class. `privlatestage` stages the pages
the CPU wrote between the flush and the commit of the first command buffer a second time (`Pages::flush_late`) and copies them
in at the end of that buffer, to test whether CPU writes after the flush are the defect. The write counters of the report split
the first write of each member page by where the newest frame is (done or before the next flush, between flush and commit,
after commit and before done; "before the flush" and "after done" cannot be told apart for a game that waits) and list the top
program counters of the middle class.

**What step 2 does not cover.**

- The texture copy-back (`copyback.rs`) and the native replacements (`native`, the ANE offload) still use guest memory; a member
  page that one of them reads or writes is stale or lost there. The counter reports and the guest range of a copied-back texture
  keep going through the host's writes after the batch, which are plain CPU writes (dirty pages) for a member page.
- Textures are Metal textures; only their upload sources redirect.
- A CPU access that reaches the page while another thread copies it can read half a page. The copy runs for about a
  microsecond with the page open; threads that fault in that time sleep and retry, but a thread whose access began before the page
  opened is not held back.
- A page that was staged for a frame that is never encoded (an encode that failed) is not copied into the mirror.
- The frame number and the table keep the limits of step 1 (28 bits, 24 frames of runs).

## Private GPU memory, step 3: fences at the flush (`NXKIT_PRIVATE=1`)

With the mirror in place the CPU's writes for frame N + 1 cannot reach frame N on the GPU, so a wait for the fence of frame N
does not have to wait for the GPU. Step 3 answers it at the flush. The overlap and the renamer stay off.

**The wait** (`GpuBackend::wait_fence`, `wait_early`). A wait for fence N returns when the flush of N has run
(`captured`, set after `private::at_flush`): the CPU's writes are in the staging buffer, the pages the frame may write have no
access, and the walk has taken what it takes. It does not wait for the encoder thread or the GPU. Still bounded: the wait for
fence N also waits until the frame `BATCHES_IN_FLIGHT` (3) before it is done, which keeps the batch ring, the staging buffers
and `MAX_PENDING` presents from running away. A wait that finds the fence done answers `AlreadyDone` as before. Every answer
also writes the counter and timestamp reports of the frames the GPU has finished (`harvest`, no wait), so the reports are as
fresh as the GPU allows, not later than before; the report of the frame that was just flushed is not there yet (the game
reads the report of the frame before, as with the overlap).

Waits that stay full (`DIAG private fences`, `full waits`):

- `privhonest` (knob): the behaviour of step 2, for comparing.
- copy-back: a frame that copied textures back to CPU memory (`copied_recent`, `copied_fence`) and the `COPYBACK_SPAN` (3)
  frames after it; the copy-back is not guarded yet (step 5).
- `idle GPU`: `nvnQueueFinish` and the other `wait_idle` callers, and the drain at a flush that makes a page a member while a
  frame in flight may still write it in guest memory.
- `flush not finished`: a fence that is not captured yet.

**Copies of what the CPU may still write.** With early fences the CPU also writes the data of frame N + 1 into pages that are
not members, while frame N reads them from guest memory. The walk (`walk/mod.rs`) therefore takes the copies it takes with the
overlap (every bound uniform buffer, and the storage buffers that draws only read) when `NXKIT_PRIVATE=1` answers early, as of
the submit of the commands; a range whose pages are all members is not copied (the mirror holds the bytes of the flush), unless an
inline update changed it. `nosnap` and `nosnapsbuf` still turn them off.

**What belongs to a frame: the boundary.** The rule is: *the flush is the boundary for all data the frame reads through the
mirror*, except for submits that the flush ran before the CPU signalled what they wait for.

- The pages the CPU wrote before the flush are staged at the flush and copied into the mirror at the start of the frame. A
  write after the flush is the data of the next frame and goes into the next flush's staging buffer. With the CPU ahead of the
  encoder this is the only boundary that does not hand a frame the data of the one after it: the commit comes some
  milliseconds after the flush (the encode), and the game writes the next frame's data in that time.
- Evidence (OoT, from the start to the intro movie and the first scene; `DIAG private between flush and commit`): the CPU
  first-writes 0.0 to 1.1 member pages per frame between the flush and the commit (0.3 to 1.0 per frame in the movie). The address of
  each first write is compared with the byte ranges (not the pages) that the frame binds. In nearly all windows none is inside the bytes of a
  range that the frame reads as storage, uniform, vertex, index, indirect or copy source (0.00 per frame); 0.03 to 0.35 per frame are in a page that one of
  its bindings shares (false sharing), 0.2 to 0.7 in pages of no binding of the frame, and 0.00 to 0.03 in the bytes that the
  frame may write (those are writes in flight: the CPU waits for the frame and the write lands on the GPU's bytes). At the commit the
  bytes of the bound ranges in the written pages were compared with what the frame sees (staged bytes, else the mirror): equal in
  all windows but two, where 0.01 and 0.03 storage ranges per frame changed (1123 and 6273 bytes in 300 frames): a ring range
  that the CPU rewrites for the next frame right after the present, which the flush rule leaves to the next frame, as intended.
  Raw pointer reads of a shader are not bindings, so the pages in no binding cannot be excluded this way; the argument for them
  is the order of events: the game writes after its present, which is the flush.
- The exception is the queue gate (`nxkit-rt/src/nvn/ctx/gate.rs`, `order.rs`): a submit that waits for an event that only the
  CPU signals is held, and a flush runs it without the signal. The game writes what the submit reads until the signal, which
  comes after the flush. Such a flush now reports the waits (`Backend::forced_waits`, `ForcedWait`), and the backend registers
  them with the frame (`forced_pending`, `private::late_expect`). The CPU's `nvnEventSignal` reports the signal
  (`Backend::cpu_event_signalled`); when the last signal of a frame has come, `private::late_signal` captures the pages the CPU
  wrote since the flush into a second staging buffer. The encode of the frame waits for that capture (at most 15 ms; after
  4 frames in a row whose signals did not come in time the capture stops waiting) and adds a second copy-in pass before the
  commit. Each submit so sees the data as of the time its wait was satisfied, which is the time it ran on the console. In OoT no
  flush ran such a submit (0 of the frames in the windows, `DIAG private forced submits` is not printed); the gate showed
  `forced at a flush 0` in the queue diagnostic. The path is tested with a unit test and has not run in a game.
- `privlatestage` stays as a knob. It stages every page the CPU wrote since the flush a second time and copies it in before the
  commit. With the CPU ahead of the encoder it hands frame N the data of frame N + 1, so it is a comparison for honest waits
  (`privhonest`), not a setting for early fences.
- Counter: `DIAG private between flush and commit` (pages in all, in bytes the frame writes, in bytes it reads by kind, in a page
  of one of its bindings but outside the bytes, in no binding; frames with forced submits) and the line `exact`: the bound ranges per
  frame whose bytes in a written page differ at the commit from what the frame sees. Both print only with
  `NXKIT_DIAG=private`; the first writes are logged with their address per frame (`Pages::take_between`).

**Reads and writes in flight** (as in step 2: the page is tagged with the frame that may write it, and the CPU waits for the
whole batch of that frame, copies the page from the mirror and goes on; a write in flight does the same and then lets the store
land). What step 3 adds is the count: `DIAG private CPU waits for a writer` has, for reads and for writes, the waits per
frame, their milliseconds per frame (all threads together), the longest one and how many began before the commit of the frame
(the wait then includes the encode); `DIAG private in flight` sorts the first accesses to pages in flight by address: inside
the bytes of a binding or copy the frame may write, inside a raw-store pool, or outside those bytes in a page they share.

Measured in the OoT intro movie (a scene with a GPU time of 2 to 3 vsyncs at 60 Hz): the CPU first-reads pages in flight about
5 times per frame, waiting 140 to 330 ms per frame in all threads (single waits up to 320 ms: a read of a page tagged by the newest
frame waits for that frame, and for the two before it that the GPU still works on). All of these first reads (1650 to 1900 per 300
frames, 0 inside) are at an address outside the bytes that the frame may write: the page is tagged because a binding starts or ends
in it (a pool puts data of the CPU next to a buffer of the GPU), and the game reads the neighbouring data. A write in flight
is rare (0.02 to 0.07 per frame; 1 to 3 ms per frame, single waits up to 320 ms). So the wait is for the whole frame by
page granularity, not by a data dependency, and splitting the commit after the last pass that writes a page the CPU reads
(option (b) of the design) would shorten it only as far as these writers are early in the frame; the pages need a byte-exact
answer (emulate the faulting load for an address outside the written bytes, which needs the decoder of step 4 for loads, or make the
buffers of the GPU start on page boundaries). That is the next step; the split commit is not built.

**Counters** (`NXKIT_DIAG=private`, every 300 frames): `DIAG private fences`: waits answered at the flush, those that also waited for
the frame 3 before (count, ms per frame, longest) and the full waits by reason; `DIAG private CPU waits for a writer`;
`DIAG private in flight`; `DIAG private between flush and commit` (two lines) and `DIAG private between sample` (the first
writes into bound bytes); `DIAG private forced submits` when a flush ran submits before their CPU signals.

## Private GPU memory, step 4: accesses outside the bytes a frame writes (`NXKIT_PRIVATE=1`)

With early fences (step 3) the CPU reads and writes pages that the frame in flight may write, and each such first access waited for
the whole batch (in Ocarina of Time about 5 reads a frame, 140 to 350 ms in all, single waits up to 320 ms). Measured at the byte,
none of those accesses was inside the bytes the frame may write: they were in the same 16 KiB page as a written range (false
sharing at the page). Step 4 handles them by byte.

**The stale blocks.** Each tracked page has a mask of 256 bits, one per block of 64 bytes (`PageInfo::stale`). The flush of a frame
adds the blocks of every range the frame may write, rounded outward to whole blocks: the storage bindings and copy destinations by
their bytes, a pool that shaders wrote through raw pointers by all its bytes (`range_masks`, after the page is tagged). The bits
are the bytes whose truth is in the mirror: guest memory is stale there. They stay set until a copy from the mirror makes guest
memory current, so the mask of a page is the union of the frames since its last copy. The copy from the mirror into guest memory
(`copy_from_mirror`) moves the stale blocks only, and clears the mask when the writer is complete. The other blocks hold the CPU's
bytes and stay. `privpoison` fills only the stale blocks.

**The alias.** `Pages::make_alias` (called when the mirror is made) maps guest memory a second time with read and write access:
for each VM region of the range it calls `mach_vm_remap` without a copy into one reservation (`VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE`
over an anonymous `PROT_NONE` mapping, so the second mapping sits at a fixed offset from the first), then raises the protection of the
copy to read and write. Both mappings show the same pages (one VM object), and each has its own protection, so a page that has no
access in guest memory can be read and written through the alias. A region that was never touched has no VM object yet, and the
call makes one; for a touched region the object id and offset must be the same after the call (a copy would leave the GPU on the
old pages, see `nxkit-hos` `pinned`), else the alias is dropped with a message and every access waits as in step 3. The alias
costs address space only. `NXKIT_PRIVATE=1` prints one line at the start saying whether it exists.

**The emulation** (`pages/a64.rs`, `try_emulate`). In the fault handler, for an access by a game thread (not an internal one) to a
page tagged with a frame in flight, the handler reads the instruction at the program counter, decodes it, computes its address from
the saved registers (the faulting address of the signal is only checked to lie inside the access), and when the access lies in
one page and touches no stale block it does the access through the alias: a load fills the destination registers of the saved
thread state, a store reads the source registers and writes the bytes, a pre or post index writes the base register back, and the
program counter moves by 4. The page stays without access and the thread does not wait. The forms:

| form | encodings |
|---|---|
| `ldr`, `str` | `LDR`/`STR` of `b`, `h`, `w`, `x` (unsigned offset, pre and post index, register offset with `UXTW`/`LSL`/`SXTW`/`SXTX` and optional shift), `LDUR`/`STUR`, `LDRB`, `LDRH`, `STRB`, `STRH` |
| `ldrs` | `LDRSB`, `LDRSH`, `LDRSW` into `w` or `x`, in the same addressing modes |
| `ldp`, `stp`, `ldpsw` | `LDP`/`STP` of `w` or `x`, `LDPSW`, `LDNP`/`STNP`, signed offset, pre and post index |
| `simd ldr`, `simd str`, `simd ldp`, `simd stp` | the same for `b`, `h`, `s`, `d`, `q` registers |
| `ldar`, `stlr` | `LDAR`, `LDARB`, `LDARH`, `STLR`, `STLRB`, `STLRH`, `LDAPR` (as an acquire load) |
| `atomic` | `LDADD`, `LDCLR`, `LDEOR`, `LDSET`, `LDSMAX`, `LDSMIN`, `LDUMAX`, `LDUMIN`, `SWP` (also the store forms `STADD` and the others), all sizes, with the acquire and release variants: the same operation as a Rust atomic on the alias, the old value to the result register |
| `cas` | `CAS`, `CASB`, `CASH` and `CASP` (pairs of `w` or `x`; the `x` pair through `CASPAL`), the old value to the compare register(s) |
| `ldxr`, `stxr` | `LDXR`, `LDAXR`, `LDXP`, `LDAXP` and `STXR`, `STLXR`, `STXP`, `STLXP`, all sizes |
| `ld1`, `st1` | `LD1`/`ST1` of one to four registers (8 or 16 bytes each), also with an immediate or register post index |

**Exclusives.** An emulated load-exclusive is a load that remembers (address, bytes, value) in a slot of the thread; the
matching store-exclusive is a compare-exchange of the remembered value, status 0 on success and 1 on failure, and clears the slot.
A store-exclusive without a load-exclusive of the same address and size in the slot fails. `CLREX` does not fault, so it does not clear the slot; a stale slot is harmless, because every store-exclusive clears it and checks address, size and value. The
weakening is ABA: if another thread changes the location and changes it back between the two, the store succeeds where the
hardware monitor would have failed it; the value is the same, so a counter or a lock word behaves, but a lock-free structure that
needs the change itself will not. If the page opens between the load and the store (the frame completed), the store runs on the
hardware, finds no monitor and fails, and the loop of the guest goes round once more.
An atomic or exclusive on an address that is not aligned to its size (the hardware would fault) is not emulated and takes the
ordinary path. `LD2` to `LD4`, `ST2` to `ST4`, the single-lane and replicating forms of `LD1` are not emulated.

An aligned access of 1 to 8 bytes is one atomic access (relaxed; acquire for a load-acquire, release for a store-release), a
16-byte access is two 8-byte accesses, a misaligned access copies bytes. The access is not atomic as a whole against a GPU write,
which the stale mask makes irrelevant: the GPU never writes the bytes touched.

Everything else takes the path of step 3 (wait for the writer, copy the stale blocks, open the page) and is counted by class:
`exclusive` and `atomic` (forms not listed above), `ld1/st1` (the other structure loads and stores), `dc zva`,
`prefetch`, `other` (literal loads, unprivileged accesses, tagged and authenticated loads, anything else), `touches written bytes`
(the access reaches a stale block) and `page border or address mismatch` (the access crosses the page, or the signal's address is not
inside it).

**Stores and the next frame.** A store done by the handler lands in guest memory only. The mirror does not see it, so the frame on
the GPU keeps reading what it read, and the next flush hands it over: the first store to a page since the flush sets a flag on the
page and logs it; the flush clears the flag, reads the page through the alias and stages it with the mask of its stale blocks
(`Stager::stage_masked`). The copy-in pass leaves the blocks of the mask alone (`nxkit_private_in` takes a 256-bit skip mask per
page), so the frame's own results in those blocks survive, and the CPU's bytes in the others reach the mirror at the start of the
next frame. The frame's view of guest memory (`Overlay::read`) takes the skipped blocks from the mirror and the others from
the staging buffer. The late capture of a flush that ran submits before the CPU signals they wait for (`Pages::flush_late`) stages
these pages too. A page that is dirty (a store that took the ordinary path) is staged whole with the mask it has, which is empty
after a copy from the mirror.

**Limits.** The emulation is for game threads and host code alike (a `memcpy` of the host faults and is emulated the same way),
but only with an alias and only for a page whose frame is still in flight; after the completion the ordinary copy is one page
copy and needs no emulation. The stale mask is only as exact as the ranges of the flush: a shader that writes beyond the bound size of a
storage buffer, or into a raw pool before the encoder learned it, writes bytes that are not in the mask, and the copy from the
mirror no longer moves them (before step 4 the copy moved the whole page). A load of a block outside the mask sees guest memory,
which holds the CPU's own bytes. `privnoemu` (knob) turns the emulation off. A store of the CPU into a block that is
not stale and that the GPU reads in the same frame is seen by the next frame only, as every CPU write after the flush.

**Counters** (`NXKIT_DIAG=private`, `DIAG private emulated accesses per frame`): loads and stores done by the handler, by form,
the milliseconds they took in the handler, the pages the flush staged for them, and the accesses that were not emulated and waited
for the writer, by class; `DIAG private top program counters, not emulated` lists the program counters of those. The faults of
step 3 (`read in flight`, `write in flight`) and the waits for a writer now count only what was not emulated; the first store to a
page since the flush still counts in the line of writes to member pages by phase.

## Private GPU memory, step 5: whole bindings, and less work on the game thread (`NXKIT_PRIVATE=1`)

Step 5 closes a gap of the mirror and takes the cost of the model off the game thread.

**Whole bindings.** A range that is partly on member pages resolved to guest memory (`partial`): it could read bytes that the GPU
wrote in the mirror, or the GPU could write guest bytes that the next frame does not see. The flush now makes every
binding range that touches a member whole before the frame is encoded: the closure over the read ranges (uniform,
storage, stream, index, indirect and copy source) runs rounds until a round adds nothing (up to 8; it was 2), looks at each
range once and drops the ranges that are whole, and cuts a range that reaches past the end of guest memory to it instead of
leaving it out. Ranges that no part of `FrameDetail` announces (texture upload sources, copy boxes) are learned: every
resolve that finds a range split between members and non-members (`overlay::note_partial`, called from `resolve`/`resolve_c`
by class and from the uniform and storage sites through `check_span`) remembers `(address, length)`, and the next flush adds
the remembered ranges (at most 2048) to the closure. A range is therefore split at most once, in the frame that first
uses it; a second split resolve of the same range means a limit of the closure kept it (`CLOSURE_MAX_BYTES`, `RANGE_MAX_BYTES`,
the rounds), and is counted apart. Before this the line printed the count of a whole window (300 frames), not a per-frame figure:
601 in a window is about 2 per frame.

**Cost of the model.** Measured with a synthetic frame of the live shape (71 bindings of 72 pages, 77 written pages read and
written by the CPU, 154 pages beside them that the CPU writes every frame; test
`the_adaptive_settings_keep_the_bytes_exact_and_the_numbers_are_printed`): the flush spent 3.1 of 3.7 ms in the tags of the
written pages, and 2.9 of those in `range_masks`, which set the 256 block bits of every written page one by one. It sets
whole words now (0.3 ms for the same pages). The flush also tests a page's word before the compare-exchange (new members)
and the stale bits before the `fetch_or` (the tags), and `DIAG private cost per frame` splits the flush: plan of the written
ranges, read ranges, closure, growth of the mirror, the pages (new members, dirty log, staging, writable pages, tags, and the
`mprotect` time in all of it), the rest, and the fault handler.

**Copy back after completion.** A game thread that waits for a copy from the mirror (a fault on a page whose frame is done)
marks the page `eager` for the epoch (256 flushes). When a frame completes, a thread of its own copies the stale blocks of
the marked pages that are still tagged with that frame from the mirror into the page through the alias, then opens the run
for reading and sets the pages clean, with one `mprotect` per run of marked pages and the control mutex held per run (a flush
never sees a page half done; the handler's own claim, the `SYNC` state, covers a thread that faults at the same time). A
page that nobody faults on any more loses the mark after two epochs and faults once to get it back, so the copies a page
does not need are bounded. It needs the alias (no alias, no copy back). The knob
`privnoeager` turns it off.

**Writable pages.** A page that the CPU dirtied in 3 flushes in a row (`HOT_AFTER`) and that no frame writes in this flush
stays read and write: the flush keeps a copy of the bytes it staged for it, compares the live page with it at each flush,
stages the page when it differs, and after 8 flushes without a change (`HOT_QUIET`) protects it again and compares once more
(a store between the last compare and the protection is staged then). The CPU never faults on such a page; the cost is one
16 KiB compare per page per flush and one copy when it changed. A page that a frame writes in a flush leaves the set and
takes the old path (tag, stale blocks). A store during the flush lands in this frame or the next, as for every other page;
the shadow is what was staged, so no store is lost. The knob `privnohot` turns it off.

**Counters** (`NXKIT_DIAG=private`): `DIAG private partial resolves per frame` (class and split: starts in and leaves, starts out
and reaches in, past the run; the learned ranges; the repeats), `DIAG private cost per frame, game thread` (the milliseconds of
the flush by part, the fault handler and the sum, "the model costs"), `DIAG private adaptive per frame` (writable pages, staged
and unchanged, promotions and demotions; pages copied back after completion, `mprotect` calls and milliseconds on the copy-back
thread). Synthetic numbers, per frame, with 304 faults per frame without step 5: the copy back leaves 1 copied by a fault of
77, the writable pages take the faults to 83 (the 77 CPU writes to written pages remain: such a page is tagged every
frame and cannot stay writable); the flush costs 0.9 ms (it was 3.7), plus 0.5 ms for the 150 writable pages. A fault costs
2 us in this test and about 12 us live (3.9 ms for 313 faults), so the live saving is the faults times 10.

## Private GPU memory, step 6: pass signals for the results the CPU reads (`NXKIT_PRIVATE=1`)

Step 4 and 5 left the accesses that touch bytes a frame really writes. In Ocarina of Time these are the results that the game
reads back each frame (a visibility sum, spherical-harmonics coefficients): the CPU read waited 0.9 times per frame for 31 ms
(longest 89 ms) and a write 0.15 times for 9 ms, for the whole batch of the frame, and for the encode of the frame when the
access began before the commit. The GPU idled meanwhile and its clock dropped. A reader needs the bytes of one pass, not the
end of the frame.

**What is learned.** The handler records the page of every first access of a game thread that waits for a writer
(`learn_reader` in `pages.rs`, atomics only). `Pages::readers(frame)` (the encoder calls it, with its own lock, not the
flush's) drains that log into a set of CPU-read GPU results and drops a page whose last wait is more than 240 frames old
(`LEARN_FRAMES`, 4 s at 60 fps). Accesses the handler emulates never wait and never teach.

**Where the frame is cut.** `private::early_cuts`, called by `encode_frame_inner` before the batch is made, finds for each such
page the last pass of the frame that may write it. The ranges a pass may write come from the same rules as the flush
(`written_in`, storage bindings a shader may write, buffer copy and fill destinations), but taken from the commands of the pass
(`ordinals_with_end` gives the draws, dispatches and copies of each pass). The unit is the page: a later pass that writes
other bytes of the page moves the cut after itself, because the copy from the mirror moves every stale block of the page.
A page has no cut when a pool that shaders wrote through raw pointers covers it (any pass may write it), when the flush's
whole-frame ranges touch it and no pass was found for it, when a texture that the frame copies back or the CPU reads shares
its range (those are written by the last command buffer), or when its last pass is the last pass of the frame. At most
`EARLY_CUTS` (3) cuts are made: the earliest two and the last, the pages of the others join the next cut after them
(`plan_cuts`, a pure function with a test). A cut after pass `p` puts `p + 1` into the chunk ends of `jobs::split`, so the
chunk with that pass ends a command buffer; no pass moves. The ring holds `EARLY_CUTS` more allocators than before.

**How the signal is given.** `Batch::on_prefix_complete(count, f)` (`nxkit-metal` `batch.rs`) makes the batch commit in groups:
the command buffers up to the cut in one `commit`, the rest in another, each with its own feedback. Group `g` reports when every
group up to it is done (the feedback handlers may run in any order), and the callback of the cut, `Pages::release_early(frame,
runs)`, sets `ready = frame` on the pages of the cut and wakes the sleepers. Metal 4 has no event inside a command buffer, so
a commit is the finest unit; the price is one more commit per cut (the older comment on `submit` measured 0.2 to 1 ms of GPU
pause when every command buffer was committed one by one, here it is one to three extra commits). The knob `privnosplit` turns
the cuts off.

**Committing early.** `Batch::commit_ahead` returns an `Ahead` for the first group. The encode hands each command buffer of the
group to `Ahead::finish` as soon as its chunk is encoded (the first one, with the uploads and the copy-in, before the chunks), and the
thread that ends the last one commits the group while the other chunks still encode. A reader whose access began before the
commit (114 of the window) no longer waits for the whole encode. Not used when the frame has a late stage (`forced` flush, knob
`privlatestage`: the first command buffer still gets the pages the CPU writes until the CPU signal), and the knob
`privnoahead` turns it off. The group never contains the last command buffer, which carries the copy-back of textures.

**The wait.** A page is in flight for a thread while its frame is not complete **and** `ready < frame`
(`page_flying` in `pages.rs`). `wait_complete(page, frame, write)` sleeps on that, so a signal ends it; then the normal path runs:
claim the page, copy the stale blocks from the mirror, clear the stale mask, clean or dirty. The bytes are the pass's:
the mirror is written by the GPU before the commit's feedback. A later frame that flushes the page again tags it with its own
number, and `ready` of an older frame does not release it. `try_emulate` leaves a released page to the copy.

**Run-ahead.** With early fences the CPU used to run up to `BATCHES_IN_FLIGHT` (3) frames ahead, and a read of a GPU result waited
behind the whole queue. `NXKIT_PRIVATE_AHEAD=<n>` (default 1, at most the ring) makes a wait for fence N return once flush N is
captured and frame N - n is complete; 0 is the honest wait. `DIAG private fences` reports the time spent waiting for frame N - n.

**Counters** (`NXKIT_DIAG=private`): `DIAG private pass signals, per frame`: the waits that ended at a pass signal while the frame
still ran and those that ended at the completion of the whole frame (count and ms per frame; the sum equals `DIAG private CPU
waits for a writer`), the accesses that found their page released, the size of the learned set and how often it changed in the
window, the frames cut with the cuts per frame, their position (pass index plus one over the passes, average), the pages
released, the frames committed ahead, the readers per frame with and without a cut, and the planning time (encoder thread).

**Tests.** `plan_cuts` (cut positions, the cap of cuts, blocked and unwritten pages), `early_cuts` on a real analysed frame of
three passes after a real wait taught the pages, the exclusion of copy-back pages, and, on real Metal, a batch of three command
buffers where pass 0 writes a learned page and pass 1 spins for 0.5 s on the GPU: the reader returns 0.4 ms after the first
group was committed and 490 ms before the batch completes, while the third command buffer is not even encoded yet; in
`pages.rs` a reader goes on at `release_early` while a second page waits for the completion, and the learned set grows and
decays.

## Memory

`NXKIT_MEM_REPORT` (see `docs/diagnostics.md`) says where the memory goes. What the GPU layer does about it:

- The cache of native plans keeps 4 plans (`NXKIT_NATIVE_CACHE`), least recently used first out.
- The batch scratch buffers start at 8 MiB per slot and double up to 64 MiB when a frame needs more.
- Compressed, depth and stencil uploads share pooled staging buffers (see "Textures").
- Textures that no frame used for 600 frames (`NXKIT_TEX_EVICT`) lose their Metal copy when the guest copy is current (`textures/evict.rs`). The store notes the frame of each Metal texture's last use every time `ensure` hands it out, and `end_frame` scans every 60 frames. A texture that was ever an attachment, an atomic target or a CPU-read texture, that a frame wrote, or that is memoryless, widened or scaled stays. The dropped texture goes through `retired`, and the command buffers of frames in flight keep their own references through `Keep`, so it lives until the GPU is done. The next use makes it again with `fresh` set, and the normal upload path (`needs_upload`) fills it from guest memory. The orphans of the front end (finalized textures that a pool slot still names) lose their Metal copy the same way when they go idle.
- Textures over memory that another texture overwrote lose their Metal copy too (`textures/supersede.rs`, `NXKIT_TEX_SUPERSEDE=0` turns it off). On the console one guest range exists once, so a GPU write to a texture overwrites every other texture over the same memory. The game makes new texture objects over old memory with other sizes and formats, and each one has a Metal texture in the store. A Metal texture goes when another Metal texture over an overlapping range was written by a frame after the last use of the first one, and the first one was idle for `MIN_FRAMES` frames. Only a pass that wrote the texture counts as a write (`mark_written`), not the state "the GPU copy is newer". A texture with several images (`multi`: mip levels, layers, a cube, a depth) stays when the other range lies strictly inside it or strictly covers it, because the write may have touched only one image; a single image that was partly overwritten goes, so a smaller render target at the address of an older larger one supersedes it. A texture that the CPU reads back and a memoryless one stay. The scan runs with `evict_scan`, sorts one range per Metal texture by start address and compares each range with at most 256 followers, so a large cluster at one address is only partly seen in one scan. The ids keep their attachment, atomic and native state, so an id that is used again gets a new texture in the same shape.

- The guest range is one no-copy `MTLBuffer`. Its whole length counts in the Metal device total and in the Metal HUD, touched or not, and the driver wires all of it while it is resident (see `docs/nxkit-host.md`, "What the GPU buffer costs"). `NXKIT_MEM_REPORT` shows the touched and wired pages apart.
- The pipeline archive is read into memory per file. With shards (one per batch) the host opens a shard on its first pipeline and keeps 32 open (`NXKIT_SHARD_LRU`), and closes them all when the prewarm is over (`NXKIT_SHARD_TRIM=0` keeps them); the single archive of an older build is read whole (4.8 GB for one title, measured).
- `NXKIT_MEM_ALLOC=<MiB>` prints who makes a large heap allocation.

Shader libraries stay loaded on purpose. `Programs` keeps each `StageProgram` with its `ShaderLibrary` for the life of the shader directory, and the library cannot be dropped once the pipelines of a program exist: a program makes a new pipeline for every combination of render targets, blend state, vertex layout and topology, so a later draw can need the library again. The code does not know when the last such pipeline was made. `pipelines/render.rs` builds the pipeline description with `&vs.lib` before it looks the cache up, the background compile jobs (`RenderJob`, `MeshJob`) hold clones of the libraries, and the mesh path clones them as well. To release a library, the description would have to be built only on a miss, and `StageProgram.lib` would become a handle that loads the metallib again on demand. That changes the hot path of every draw for an unmeasured gain: the library counter (`libs` in the report) counts objects, not bytes, and Metal does not report what a library costs. Measure `libs` and the `files` regions in a long run before doing it.

## Native replacements (`native`)

A game can run a neural network as a chain of compute dispatches. `native` replaces such a chain by a `Plan` of own kernels (`nxkit-native-plan`, built by `nxkit-ndve` or `nxkit-ndsr`). The module knows no network: a `Replacement` gives the program keys that find the run, the weight buffers that prove the network, and the plan. Each frame `Native::prepare` reads and hashes the weights through the storage bindings, looks the verdict up in the plan cache (4 plans, least recently used out, `NXKIT_NATIVE_CACHE`) and resolves the game's textures, so nothing fails later. The pass loop then skips the run's dispatches and `encode` writes one compute pass with the plan's steps and a barrier between dependent steps. Any mismatch leaves the game's own dispatches: the result is the original picture and never a wrong one, and the health report counts the reason (`ndve.*`, `ndsr.*`).

- NDVE (`native_ndve`): the movie network, key and bindings of `nn::ndve`. NDSR (`native_ndsr`): the agl upscaler; each input and output size has its own plan.
- `NXKIT_NDVE=0` and `NXKIT_NDSR=0` turn a network off. `NXKIT_<NAME>_COMPARE=1` runs the game's chain and the plan on a sampled frame and prints the largest difference per channel. `NXKIT_DIAG=ndve`, `ndsr` print the capture list once per run. `NXKIT_DIAG=nativeprof` times each step with GPU timestamps (the steps then run one after the other, so the sum is above the plan's time). `NXKIT_SKIP_PROGRAMS` and `NXKIT_SKIP_FILE` leave out steps by kernel name.
- Status: measured kernel times are in the plans ([ndve-impl.md](plans/ndve-impl.md), [ndsr-native.md](plans/ndsr-native.md)). A plan holds tens of MB of scratch.

## Neural Engine offload (`native/ane`)

For NDVE the model can run on the Neural Engine instead (`NXKIT_NDVE_ANE=0` turns it off). Plan: [plans/ndve-ane.md](plans/ndve-ane.md). Files: `ane.rs` (session, load and refuse), `feed.rs` (the feeder thread), `conv.rs` (the CPU colour conversion), `slots.rs` (the result table), `offload.rs` (the dispatch side), `matching.rs`, `trace.rs` and `print.rs` (finding the picture), `ane_trace.rs` (`NXKIT_DIAG=ndvetrace`).

- A background thread writes the Core ML package (`nxkit-ndve::ane`), compiles and loads it with `nxkit-coreml`, and refuses it unless every operation is on the Neural Engine. The package is cached by the weights and the macOS build.
- The model runs at decode time. `nxkit-svc-video` reports each decoded `Frame` and each read of a picture into guest memory (`PictureSink`). The feeder converts the picture to the f16 input on the CPU and starts `predict_async`. The completion makes the slot `Ready`. A picture shown twice finds its slot again.
- The dispatch finds its picture by the address of a read, by the chain of the walk's copy records (each carries the plain GPU address of both sides; up to four hops), or by a luma fingerprint of 256 scattered bytes. The game copies the decoder's planes, so the first way alone fails.
- A ready result lets the frame run `ndve_ane_quant` and `ndve_ane_epilogue` in place of the plan, with the slot's outputs bound as textures. Without a result the plan runs. Counters: `ndve.ane` and `ndve.ane_fallback(warmup|late|unknown_picture|params|error|no_match)`.
- Not measured here: the share of pictures that are early enough in a game. Read it from the counters.

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
- Transform feedback and `DrawTexture` draws are placeholders without a binding.
- An indirect draw puts base instance 0 into the driver block, because the GPU reads the real one with the arguments.
- The arguments of an indirect draw must be final when its render pass starts. A draw of the same pass that writes them through a storage buffer is not seen.
- The multisample state reads only alpha to coverage.
- `UpdateUniform` writes into guest memory when the walker reads it. A frame that the GPU still runs can read the new data.

These are known gaps. Each shows up as a `Skip` count or is a limit of the first version.

- Partial clears (a scissor cuts the target) are not encoded.
- Triangle fans, quads, adjacency and patches, and byte indices, are not translated.
- Multisampled attachments are downgraded to one sample. The pass descriptions do not resolve.
- The binding kinds `cbuf`, `tex`, `samp`, `samp_tex`, `tex_pool`, `samp_pool`, `drvcb`, `ubo_ptr`, `support`, `border`, `blendz`, `sbuf`, `c1`, `img`, `gmem_map` and `gmem_log` are filled. A program with another kind is skipped (`Skip::Binding`); the health report names the kind (`binding.refused`). `img_ubo` (bindless images) is not filled. The shader variants `_bv`, `_bx` and `_bf` are not used. `_bp` is: a vertex shader with a `pull` field is loaded from `<key>_bp.metallib` (`programs.rs`), and each of its draws gets a `dm_vlayout` block at buffer 16 instead of its vertex streams and the default vertex (`pull/mod.rs`: per stream the address rounded down to 4 bytes, stride and divisor, per input location the NVN attribute word, 0 for an input without a bound stream). Its pipelines have no vertex layout, so the key and the thread memo leave the attributes and streams out. Off unless `NXKIT_PULL=1`; otherwise the `_b` library and the stage_in layout are used.
- `blendz` is one 32-bit word per draw in the fragment argument buffer (`translate::blend_zero_word`): 3 bits per colour target, the kind of its source RGB factor (0 none, 1 `ZERO`, 2 `SRC_ALPHA`, 3 `ONE_MINUS_SRC_ALPHA`). The pixel shader writes RGB 0 to a float colour whose factor is 0 for the pixel, because the Apple blender gives NaN for `0 x NaN` and the console gives 0 (see [translator.md](translator.md)). The word is 0 for blending off, an integer format, a colour the shader does not write, the equation MIN or MAX every other factor and a destination RGB factor that reads the source colour (`SRC_COLOR`, `ONE_MINUS_SRC_COLOR`; the rule would zero it); the binding then points to the zero buffer. The memo of the argument buffers includes the word. A program without the binding ignores it, and the knob `nobz` sets every word to 0.
- The binding kinds `cbuf`, `tex`, `samp`, `samp_tex`, `tex_pool`, `samp_pool` (the host tables of the pool tables section), `drvcb`, `ubo_ptr`, `support`, `border`, `sbuf`, `c1`, `img`, `gmem_map` and `gmem_log` are filled. A program with another kind is skipped (`Skip::Binding`); the health report names the kind (`binding.refused`). `img_ubo` (bindless images) is not filled. The shader variants `_bv`, `_bx` and `_bf` are not used. `_bp` is: a vertex shader with a `pull` field is loaded from `<key>_bp.metallib` (`programs.rs`), and each of its draws gets a `dm_vlayout` block at buffer 16 instead of its vertex streams and the default vertex (`pull/mod.rs`: per stream the address rounded down to 4 bytes, stride and divisor, per input location the NVN attribute word, 0 for an input without a bound stream). Its pipelines have no vertex layout, so the key and the thread memo leave the attributes and streams out. Off unless `NXKIT_PULL=1`; otherwise the `_b` library and the stage_in layout are used.
- The texture view has no channel swizzle (counted as `swizzle_ignored`). A cube view ignores the layer range.
- Texel fetches with integer coordinates and `textureSize` of a 2D texture see native units through the render scale of the support buffer. Other texture types still see the scaled size (refer to the render scale section).
- An unbound pool slot reads as zero. A pool ID that a shader reads for the first time shows its texture one frame later.
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
| Visibility counting adds per offset across passes with `Accumulate` | `crates/nxkit-metal/tests/visibility.rs` |
| An occlusion condition hides the right objects (samples on the scaled targets, depth of the pass) | a scene with occlusion culling, `NXKIT_DIAG=rendercond` false ratio, and `NXKIT_COUNTERS=0` to compare |

The code has not been compiled or run. The first build and the first frame of Breath of the Wild will show the remaining errors.

## Numbers that shaped the design

Super Mario Bros. Wonder frame 7200 has 601 draws in about 11,000 records and 565 recordings. 62 percent of the draws are in recordings called more than once, which is the case for ICBs. Binds are the largest group: 1,976 vertex buffers, 1,848 small uniform buffers and 1,365 textures, which is the case for bindless tables and guest-address binds. There are 67 render target changes and 217 program switches. Translating that frame on the CPU takes 4.5 ms of 4.7 ms, and it has 14,700 full barriers and 591 split submits. The target here is to encode the whole frame in parallel with few barriers and re-run the reused recordings from ICBs.
