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

A texture that the game only uploads is never an attachment and keeps its size. Viewports, scissors and copy regions between scaled targets scale with the factor. A copy between a scaled and an unscaled texture is not encoded and counts as `Skip::Copy`. A texture that becomes scaled is created again and its old contents are lost.

## Textures

`textures.rs` makes one Metal texture for each `TexId` and keeps it while its description stays the same. A texture in `GuestNewer` state, or a new texture in `InSync` state, is deswizzled from guest memory by the `Deswizzler` in the first command buffer before the frame uses it. A scaled texture is not uploaded (counted as `scaled_upload_lost`). Render targets stay in Metal (R4). `Gpu::encode_frame` copies a texture back to guest memory with the `Swizzler` only when a `CpuRead` command names it and the GPU wrote it, in this frame or before. A scaled, compressed or widened texture is not copied back.

## Diagnostics

The encoder has opt-in diagnostics (`NXKIT_DIAG`), knobs for experiments (`NXKIT_KNOB`) and a render health report that counts every draw, upload, view and copy that was dropped (`NXKIT_DIAG=health`). [diagnostics.md](diagnostics.md) lists them all.

## Measurement (R16)

`EncodeStats` counts draws, skipped commands by reason, ICB executions and builds, pipeline switches and builds, barriers emitted and avoided, render and compute passes, command buffers, textures created, uploads, copy-backs and the CPU time of the encode in nanoseconds. The `Metrics` of the batch ring hold the GPU time of every command buffer.

## Limits

The walker leaves some of the detail out:

- `FrameDetail.copies` and `runs` stay empty. A copy uses the whole first level and no draw runs as an ICB.
- Images and storage buffers are not in the detail. Indirect, mesh and `DrawTexture` draws are placeholders without a binding.
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
