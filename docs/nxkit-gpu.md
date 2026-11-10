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
- **Vertex pulling.** A vertex shader with a `pull` field in its `_b.json` draws through its `_bp` library. The encoder fills a `dm_vlayout` block (for 16 streams the GPU address, stride and divisor, and 16 attribute words) and binds it at buffer 16; the pipelines have no vertex layout, so one archived base per pair serves every layout. `NXKIT_PULL=0` keeps `stage_in`. `NXKIT_DIAG=pipe` counts pulled and `stage_in` draws. Built and tested; not yet measured in a game.

## Bindless textures and buffers

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
- The binding kinds `cbuf`, `tex`, `samp`, `samp_tex`, `tex_pool`, `samp_pool`, `drvcb`, `ubo_ptr`, `support`, `border`, `blendz`, `sbuf`, `c1`, `img`, `gmem_map` and `gmem_log` are filled. A program with another kind is skipped (`Skip::Binding`); the health report names the kind (`binding.refused`). `img_ubo` (bindless images) is not filled. The shader variants `_bv`, `_bx` and `_bf` are not used. `_bp` is: a vertex shader with a `pull` field is loaded from `<key>_bp.metallib` (`programs.rs`), and each of its draws gets a `dm_vlayout` block at buffer 16 instead of its vertex streams and the default vertex (`pull/mod.rs`: per stream the address rounded down to 4 bytes, stride and divisor, per input location the NVN attribute word, 0 for an input without a bound stream). Its pipelines have no vertex layout, so the key and the thread memo leave the attributes and streams out. `NXKIT_PULL=0` keeps the `_b` library and the stage_in layout.
- `blendz` is one 32-bit word per draw in the fragment argument buffer (`translate::blend_zero_word`): 3 bits per colour target, the kind of its source RGB factor (0 none, 1 `ZERO`, 2 `SRC_ALPHA`, 3 `ONE_MINUS_SRC_ALPHA`). The pixel shader writes RGB 0 to a float colour whose factor is 0 for the pixel, because the Apple blender gives NaN for `0 x NaN` and the console gives 0 (see [translator.md](translator.md)). The word is 0 for blending off, an integer format, a colour the shader does not write, the equation MIN or MAX every other factor and a destination RGB factor that reads the source colour (`SRC_COLOR`, `ONE_MINUS_SRC_COLOR`; the rule would zero it); the binding then points to the zero buffer. The memo of the argument buffers includes the word. A program without the binding ignores it, and the knob `nobz` sets every word to 0.
- The binding kinds `cbuf`, `tex`, `samp`, `samp_tex`, `tex_pool`, `samp_pool` (the host tables of the pool tables section), `drvcb`, `ubo_ptr`, `support`, `border`, `sbuf`, `c1`, `img`, `gmem_map` and `gmem_log` are filled. A program with another kind is skipped (`Skip::Binding`); the health report names the kind (`binding.refused`). `img_ubo` (bindless images) is not filled. The shader variants `_bv`, `_bx` and `_bf` are not used. `_bp` is: a vertex shader with a `pull` field is loaded from `<key>_bp.metallib` (`programs.rs`), and each of its draws gets a `dm_vlayout` block at buffer 16 instead of its vertex streams and the default vertex (`pull/mod.rs`: per stream the address rounded down to 4 bytes, stride and divisor, per input location the NVN attribute word, 0 for an input without a bound stream). Its pipelines have no vertex layout, so the key and the thread memo leave the attributes and streams out. `NXKIT_PULL=0` keeps the `_b` library and the stage_in layout.
- The texture view has no channel swizzle (counted as `swizzle_ignored`). A cube view ignores the layer range.
- Texel fetches with integer coordinates and `textureSize` of a 2D texture see native units through the render scale of the support buffer. Other texture types still see the scaled size (refer to the render scale section).
- The pool tables are written while earlier frames may still run. A frame in flight can read an entry that a later frame rewrote.
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
