# The NVN front end (`nxkit-nvn`)

`nxkit-nvn` is the first part of the GPU of the nxkit host. It is a pure Rust crate. It has no Metal code, no FFI and no dependency.
It is not wired into `nxkit-rt` or the host yet. No game has run through it. Refer to [plans/nxkit-host.md](plans/nxkit-host.md) for the plan.

## Guest memory and GPU addresses

The host maps guest memory as one Metal buffer without copies. A Metal 4.1 shader can read and write guest memory through a raw 64-bit address.
Metal chooses the GPU address of the buffer, for example `0x15_0400_8000`.
The GPU address that NVN gives the game is the Metal address. The rule is `backing_gpu_base + (host_address - backing_host_base)`.
There is no translation table.

The trait `GpuSpace` hides the base. `LinearSpace` is the implementation for one mapping. The Metal backend supplies the real one.
An address outside the mapping is an error. An address that does not fit in 40 bits is an error.

## Textures

- `format` knows the size of a block and the aspects of an NVN format. It is ported from nxnative.
- `layout` computes the storage size, alignment, block height and level offsets. It is ported from nxnative, which took the algorithm from deko3d. The tests compare the result with the C code of nxnative. Sparse textures are not supported and return an error.
- `swizzle` has a CPU reference for block-linear to linear and back. It is byte-by-byte and slow on purpose. It is the fallback and the reference for tests. Both GOB orders exist, Tegra X1 and Switch 2.
- `tic` encodes and decodes a texture header (TIC) and a sampler header (TSC) of 32 bytes. The words follow the structure of the Maxwell headers, but the bit positions are nxkit's own encoding. No bit position is checked against hardware, because no deko3d or Mesa header is on the development machine. The game never reads the headers, so the encoding has to be lossless and nothing more. Refer to [TIC and TSC](#tic-and-tsc).
- `descriptors` holds the texture pools and sampler pools. A handle is `texture_id | sampler_id << 20`. All pools share one id space.
- `resource` is the table of textures. It finds a texture by its GPU address. Each texture has a residency flag: `InSync`, `GuestNewer` or `GpuNewer`. A CPU write to texture memory is found with `guest_wrote`. A texture can be marked frame-local when the host knows that its contents are dead at the end of each frame.

## Recording in command memory

The decision is R17 in [decisions.md](decisions.md#part-3-runtime-decisions).

### Records

`cmdbuf` writes one record for each NVN command into the memory that the game gave with `nvnCommandBufferAddCommandMemory`. `record` defines the bytes.
- A record is a multiple of 8 bytes and starts with an 8-byte header: operation, a small value and the size. All values are little endian.
- A draw, a dispatch, a clear and a state change take 8 bytes. A texture bind, a call and a jump take 16 bytes. A copy takes 24 bytes. `SetTargets` takes up to 88 bytes. A test checks these sizes.
- A record holds guest GPU addresses. A texture is named by the GPU address of its first byte. `GuestMem` reads and writes the memory, because this crate has no `unsafe` code. `VecMem` is the implementation for tests.
- `nvnCommandBufferGetCommandMemoryUsed` counts from the last added block, as in nxnative. `GetCommandMemoryFree` is what is left in the current block.
- A record that does not fit goes to the next block behind a jump. When no block is left, `cmdbuf` returns `OutOfCommandMemory` and changes nothing. `FrontEnd::write` then calls the memory callback of the game through the host trait `MemoryCallback`, adds the block that it gives and writes the record again, at most 4 times. Without an enabled callback, or when it gives no usable block, the caller gets `OutOfCommandMemory`.
- The handle of a recording is the host address of its first record.
- A command that carries parameters is an extension record (`ext`, R53). Its operation is 64 or more and its payload is up to 28 words, so a record is at most 120 bytes (`MAX_RECORD_BYTES`). `ExtCode` names the 36 codes. `Setting` names the single-value state commands and `StateKind` and `DrawKind` the state binds and draw forms. A command with more data than one record holds, as push constants, inline uniform data or vertex states, is several records.
- `write_ext` and `write_encoded` write a record at a cursor without the `CommandBuffer`. `CommandBuffer::cursor` gives the write position and the limit of the current block, and `commit` takes a position that the writer reached. `FrontEnd::recorded` drops the summaries of a range that a recording wrote.
- The decoder reads the draws, dispatches, clears, copies and syncs of the extension records and the state patch of a state bind. It skips the rest.

### Summaries and the cache

`decode` reads a recording from its handle, follows jumps and calls, and gives a `Summary`. The summary has the effect stream (`Cmd` values), the state that the recording takes from its caller (`Needs`), the state that it leaves behind, and every address and handle that it resolved.
A called recording gets its summary under the state that the caller has at the call. State that the callee sets stays set for the caller, as in nxnative.

`cache` keeps the summaries by handle. A summary is valid for the inherited state that it needs, and a handle can have several. A key holds the needed parts only: the attachments, the scissor, the draw state and the bound textures and images. A draw needs the attachments, because the Metal indirect command buffer of a later step depends on their formats.
- The game writes the command memory again: the summaries of that handle and of every recording that calls it are dropped. This happens at `end_recording` and in `guest_wrote`.
- A texture address or a handle resolves to something else, for example a texture pool slot that is registered again: the summary is dropped at the next lookup and decoded again.
- One handle keeps at most 16 summaries.

**Checking a hit.** A summary lists the addresses and handles that it resolved. A hit has to prove that they still name the same textures. `ResourceTable::epoch` and `Descriptors::epoch` count the changes that can alter an answer (a texture made or finalized, a pool slot that gets another texture or loses its texture). A summary remembers the pair of counts at which its references last held. When the pair is the same, the hit skips the check. A deferred bind reads guest memory, which changes without notice, so those references (`Summary::deferred`) are read on every hit. A summary with no references of its own and one called summary takes the references of that one without a new dedupe.

**Appending a hit.** `CmdStream::seal` notes which commands hold a span when the summary is made. `Frame::append` and `CmdStream::splice` then copy the commands in one block and patch only those, and not even those when the frame lists were empty. A stream that nobody sealed (a hand-built one) is appended command by command, with the same result. The frame still holds its own copy of the commands, because the backend reads `Frame::cmds`. Sharing segments of the summaries without a copy would change that type.

**Profile and check.** `NXKIT_DIAG=decodeprof` prints the mean per submit of the lookups, hits, misses, call depth, records and the time of each step ([diagnostics](diagnostics.md)). `NXKIT_VERIFY_CACHE=1` decodes each submitted recording again with an empty cache and fails the submit when the commands, the texture lists, the needs or the state delta differ from the cached summary.

`CacheStats` counts hits, misses, invalidations, revalidations, evictions and unresolved handles.

### The frame

A queue submit takes the summary of a recording under the state that the queue has (see [Queue state](#queue-state)) and appends its effects to the `Frame`. The stream stays in the cache, so a game submits the same recording in every frame without a new decode.
The frame is held until `end_frame`. The host calls it at a present or when the CPU waits for the GPU. `FlushStats` counts each reason. `Backend::flush_frame` gets the frame, the plan and a `FlushCx` with the resource table and the descriptor pools. The backend applies what the GPU wrote to the table. An early flush does not know the later frames, so a texture that is not proven dead stays stored.
All vectors keep their memory from frame to frame.

### Indirect command buffers

`RecCache::icb_candidates` lists the summaries that hold only draws, binds, scissors, state changes and calls of such recordings, and that were used at least 3 times. Nothing is encoded. The limits of Metal indirect command buffers are not measured.

## Render pass inference

NVN has no render passes. Metal needs them. `Analyser` cuts the recorded frame into passes and picks the actions.

### Cuts

- A pass ends when the attachment set changes.
- A pass ends at a barrier. The next render pass starts with the barrier bits as its reason.
- Compute, copy, downsample, fence, present and CPU readback each end the render pass. Neighbours of the same kind form one pass.
- A full clear of an attachment that the pass already used starts a new pass.
- A clear under a scissor does not end the pass. The old contents stay in use, so the load action is `Load`.

The scan keeps a stamp for each texture id that says which render pass already holds it in its read list, so a draw does not search the reads of its pass for each texture. The stamp counts up with each render pass.
### Empty passes

A render pass with no draw and no clear (a discard alone) does nothing. The scan drops it when it closes and puts the state back as it was before the pass: the way the next pass begins, the barrier bits and the kind of work before. A discard says that the contents are undefined, so leaving them alone is allowed. `PassStats::empty_dropped` counts them. The knob `nopassmerge` keeps them.

### Merge

Two adjacent render passes become one when all of these are true.
- The attachment sets are equal.
- Only a barrier lies between them, or the target set changed and left the attachments equal (also after a dropped empty pass). The second pass begins with `Barrier` or `Targets`.
- The second pass does not start with a clear or a discard.
- The second pass does not sample an attachment of the first pass.

Why it is safe: the commands keep their order, all draws carry their full state, and nothing else runs between the two passes. A barrier between them only matters for what the second pass reads, and the plan knows its sampled textures. `merged_after_targets` counts the merges that began with `Targets`. The knob `nopassmerge` goes back to merging after a barrier only.

A compute or copy pass between two render passes still splits them. Moving it before the first pass is not done: the plan knows the textures of a dispatch, not its buffers, so it cannot show that the draws before do not read what the dispatch writes. `NXKIT_DIAG=passmerge` counts these splits and how many touch none of the textures of the two passes.

### Load action

1. `Clear` when the pass starts with a full clear.
2. `DontCare` when the pass starts with a discard.
3. `Load` when the pass blends, tests depth or stencil, or samples the attachment.
4. `DontCare` when the attachment is not stored.
5. `Load` otherwise, because a draw may cover only a part of the attachment.

Rule 5 is the one a host can improve if it knows the draw geometry: `AttachmentPlan::load_is_default` is true for such an attachment. `nxkit-gpu` does it under the opt-in knob `coverload`, for a colour attachment whose first draw is a full-screen triangle or quad with no vertex input, a viewport and scissor that hold the target, all channels written, and no blend, test or alpha to coverage. It is off by default because the fragment shader may still discard and the vertex shader may place the triangle small.

### Store action

A backward walk knows for each texture whether a later command reads it.
- `Store` when something later in the frame loads it, samples it, copies from it, presents it or reads it back.
- `Store` when nothing in the frame proves that it is dead. A later frame may read it, for example a TAA history buffer. These bytes are counted in `bytes_stored_assumed`.
- `DontCare` when the game discards it at the end of the pass, or when the host marked the texture frame-local.

The analysis does not guess that a texture is dead. The host has to say it. This keeps a history buffer safe.

One thing is learned from the frames. The backward walk also runs a second liveness that never uses what was learned (the evidence). When it ends at the start of the frame with `No` (the first use of the texture was a full clear or a discard, a single level and layer), the texture adds one to a streak. When it ends with `Proven` (the frame reads it before it overwrites it) the streak is zero again. A texture with a streak of 120 or more starts the walk as `No` instead of `Assumed`, so its store at the end of the frame is `DontCare`, and so is the next load by rule 4. It is a scratch target. The texture of a learned store is never memoryless. `stores_learned_dead` and `bytes_learned_dead` count them, the knob `noloadstore` turns the learning off. The risk is a game that starts to read such a texture without a clear after 120 frames: the first frame of the new pattern reads the lost contents.

### Memoryless

An attachment is memoryless when it is not stored, not loaded, not sampled, and only one pass of the frame uses it.

### Counters

`PassStats` has the number of render passes before and after the merge, the dropped empty passes, the merges after a target change, the learned dead stores, the bytes loaded, the bytes stored, the bytes stored only by assumption, the memoryless bytes, the bytes of a naive load and store of every attachment of every unmerged pass, and the bytes saved. The size of an attachment is its first mip level.

## NVN functions

`entries` has the 743 names of nxnative (`names.txt`), the 484 that nxnative implements, and the table `RECORDED` of the names that this crate handles. The table has the command memory functions and the CPU wait functions too. `FFI_GROUPS` has one row per group of `nxkit-rt` with the names that the group exports.
`Coverage::of` takes the function names that one game imports and returns three lists: handled, not handled yet, and not implemented in nxnative.

## The command buffer group

The group is `nxkit-rt`, module `nvn::cmdbuf`. It answers the 145 names of the row "cmdbuf" in `FFI_GROUPS`. `nvnCommandBufferEvaluateDlss` is not in the row and keeps the stop stub (R24). The decisions are R53 and R54.

### The hot path

A recording function takes no lock, looks up no registry entry and allocates nothing in the common case.
- The `NVNcommandBuffer` keeps a magic word at 0, the recording flag at 4, the write position at 8, the limit of the block at 0x18, the texture and sampler pool bases at 0x30 and 0x34 and the copy strides at 0x38 and 0x40. The bytes 0x10 to 0x17 belong to the game.
- A call reads the first 0x20 bytes, checks the flag and `pos + size <= limit`, writes the record into command memory and stores the new position.
- The slow path folds the position into the `CommandBuffer` with `commit`, writes with `write_encoded`, and stores the new cursor back. It runs when a block is full and in `AddCommandMemory`, `BeginRecording`, `EndRecording` and the memory getters. It holds one shard lock of the registry and never the lock of the front end.
- The memory callback runs on the slow path with no lock held. It may call `nvnCommandBufferAddCommandMemory`. A record that does not fit is retried after at most 2 callbacks. Without a callback, or after the second, the game stops (R15).
- `EndRecording` takes the lock of the front end once, for `FrontEnd::recorded`.

### Handles and slots

A texture handle holds the texture id in bits 0 to 19 and the sampler id in bits 20 to 31. A nonzero id gets the pool base of the command buffer added. The bases are set by `nvnCommandBufferSetTexturePool` and `nvnCommandBufferSetSamplerPool`. A slot is `bank * 32 + index`. The banks are vertex 0, fragment 1, compute 2 and the others 3. The stages 6 and 7 map to 3 and 4. A uniform or storage buffer index of 16 or more stops the game.

### State binds

The group reads the guest state structs itself and writes the draw state patch into the record. The layouts that the patch relies on are in `cmdbuf/seam.rs`. They must agree with the writers of the state functions. These are unverified until a title runs.

### Seam

`cmdbuf::Seam` has `texture_pool_base`, `sampler_pool_base` and `texture_address`. `set_seam` installs it once. Without a seam every call that needs it stops the game (R15). The default `texture_address` takes the lock of the front end once per call. A texture group that stores the GPU address in the guest `NVNtexture` can replace it with a read that takes no lock.

### Title needs

`set_command_settings` takes `CommandSettings`. The host sets it from the needs of the title before the game runs. `ignore_null_stream_binds` is the need `vertex_stream_null_bind`. When it is set, a bind of one null range at a stream of 16 or more is ignored. Otherwise a stream of 16 or more stops the game.

### Timestamps

A report timestamp is read from `Settings.clock_ns` when the command is recorded. This is unverified against the real GPU clock.

## Queue state

A queue keeps the state that its last submit left. The next submit on the same queue starts with that state, as on the GPU. `FrontEnd::submit_on(queue, handle)` takes the number of the queue. `nxkit-rt` uses the guest address of the `NVNqueue`. A queue that was not seen before starts with the default state. `drop_queue` forgets the state at `nvnQueueFinalize`.

The cache key of a summary holds the parts of the inherited state that the recording needs. The same recording on two queues with different state has two summaries. A recording that does not decode leaves the state of the queue as it was. A call from one recording to another uses the state of the caller at the call, as before.

## TIC and TSC

The words of the headers follow the structure of the Maxwell headers. The bit positions are nxkit's own encoding. No source on the development machine has the hardware bit positions: nxnative keeps the texture pool as a table in the host, and no deko3d or Mesa header (`cl9097tex.h`, `clb197tex.h`) was found. So no bit position is checked against hardware, and the doc comment of `tic.rs` says so.

What was checked on 2026-11-07 is that each field is wide enough for the largest value that nxnative can hold, from `nvn.h`, `nvn_tex.h`, `nvn_obj.h` and `g_devinfo`. The table is in the doc comment of `tic.rs`. The test `the_fields_do_not_overlap_and_hold_the_largest_values` fills every field with its largest value and reads each back. The stride limit of 16 MiB is nxkit's and is unverified.

## FFI

The C functions are in `nxkit-rt`, module `nvn` (R22 to R24). A game gets them by name from `nvnBootstrapLoader` or `nvnDeviceGetProcAddress`. The host calls `nxkit_rt::nvn::install` once with a `Backend` and the `Settings`, after the guest range is set.

- Every function is `extern "C"`. An argument of fewer than 32 bits is a `u32`, a pointer and a `size_t` are `u64`, an enum and an `int` are `i32`, and a boolean result is a `u32`.
- An NVN object is plain data in guest memory with a magic word. The first bytes hold the host's fields. The host writes only the bytes of its own layout, because the reserved sizes differ between SDK versions (R62). These sizes are in `abi::used`. The header sizes are in `abi::size` and only tests use them.
- A buffer keeps its pool at offset 8, its offset at 0x18 and its size at 0x20. The host reads them at every call, because guest ring allocators write them directly.
- Host data of an object lives in a sharded registry keyed by the guest address (`abi::Registry`). The programs and the textures use it.
- The front end, the resource table and the descriptor pools are behind one `Mutex` in `ctx::Gpu`. A wait on the GPU does not hold it.
- A function that fails ends the game with `[RED] NVN <name>: <reason>` (R15). A guest address that is not in the guest range is such a failure.
- A name that a group does not export is handled as R24 says. A test per group compares the names of the group with its row in `FFI_GROUPS`.
- `nvnProgramSetShaders` computes the key of every stage from its control section and code (`program::stages_of`) and tells the backend, so the backend can find the shader in the `_b.json` of the shader step by that key.
- A sync that a queue fenced signals when the GPU finished the work submitted until then. A sync that a window acquire fenced is signalled at once, because `Backend::acquire` blocks until the image is free.
- `nvnDeviceGetInteger` has the table of nxnative. The values of the API version and of the GPU code come from `Settings`.

### Textures, samplers and state objects

Refer to R51 and R52. The group `texture` is `texture.rs` and the modules below it, `sampler` is `sampler.rs`, and `state` is `state.rs` with `state/read.rs`.

- An `NVNtexture` holds the id, the GPU address, the host address of the storage and the description in its first 0x5C bytes. A getter reads the object and takes no lock. The layout is in the doc comment of `texture.rs`.
- A texture builder is 0x80 bytes in the headers and the host writes the first 0x50. A texture view is 16 bytes in the headers and the host writes the first 14. Their layouts are in `texture/builder.rs` and `texture/view.rs`. The view sets only what the game set. The rest comes from the texture when it is registered.
- A texture pool and a sampler pool are 0x20 bytes in the headers. The host writes the first 0x1C bytes: the memory pool at 0, the offset at 8, the count at 0x10, a magic at 0x14 and the first descriptor id at 0x18. The code is in `texture/pool.rs`. An id is at most 4095 for a sampler, so a sampler pool must lie below that.
- `nvnTexturePoolRegisterTexture` and `nvnSamplerPoolRegisterSampler` write the 32-byte header into the pool memory of the game. The front end only builds the header.
- `nvnTextureWriteTexels` and `nvnTextureReadTexels` work in place in the storage of the texture (R52). A linear texture has one level and one layer. The GOB order is always the one of the Tegra X1.
- The state objects have the sizes of the NVN headers. The readers for the command buffer are `state::blend`, `state::color`, `state::channel_mask`, `state::depth_stencil`, `state::multisample`, `state::polygon`, `state::vertex_attrib` and `state::vertex_stream`. The command buffer also uses `texture::pool_base`, `sampler::pool_base` and `texture::id_of`.

## Graphics take-overs of the SDK

Refer to R59. The module `gfx` of `nxkit-rt` has the SDK functions around NVN that nxnative takes over with `HLE_FORCE`. They are guest exports of the `gpu` service, not entries of the NVN proc table. Each source file of nxnative has one module. A check on 2026-11-08 of the files `dm/gfx/hle_nv.c`, `dm/gfx/hle_vi.c` and `dm/gfx/nvn/hle_nvn.c` gave 47 names.

- `gfx::nv` has 8 names: the allocator and devtools allocator callbacks, `nv::InitializeGraphics` and `nv::FinalizeGraphics`, `nv::InitCheck`, `nn::gs::sdk::CheckGraphicsSystemInitialized` and the two memory info calls. The module records the callbacks and the pool of the game. `nv::InitCheck` ends the game with `[RED]` when `nv::InitializeGraphics` was not called.
- `gfx::vi` has 31 `nn::vi` names. The display, the layer and the native window are static objects of the host. The native window handle goes to `nvnWindowBuilderSetNativeWindow`, which stores it and never reads it, so any non-null pointer works. `OpenDisplay` accepts only the name `Default`.
- `nn::vi::SetLayerCrop` records the crop. The present uses the crop of the window when it has one. Otherwise it uses the crop of the layer when it fits the window texture. A crop that does not fit is logged once with `[RED]` and the whole texture is shown.
- `nvnQueuePresentTexture` counts a frame of the layer. `nn::vi::GetLatestFrameNumber` reports the count. `nn::vi::ListFrameHistory` lists the last 8 frames, newest first, with 5 `u64` for each: the status, the frame number, the queue tick and the present tick twice.
- The HDR, VRR and extended swap interval calls report SDR output and no VRR.
- `gfx::native_image` has the 8 names of `nvnNativeImage*` and `nvnTextureInitializeFromNativeImage`. A texture on a native image is a linear texture over a memory pool of the picture memory of `nxkit-svc-video`. The pool and the scratch builders are in the 0x200 bytes of the `NVNnativeImage` of the game. The layout is in the doc comment of `native_image.rs`.
- `nn::vi::GetDisplayVsyncEvent` is in `nxkit-svc-pacing` and `nn::vi::GetIndirectImageMemoryRequirement` is in `nxkit-svc-system`. `gfx` does not have them.

## Limits

- The crate takes typed values. The raw guest arguments of an NVN call are read in `nxkit-rt`.
- One lock holds the front end. The command buffer functions do not take it on the hot path (R54). `EndRecording`, `AddCommandMemory`, the texture address lookup of the default seam and the queue and resource functions take it. Several threads that call these at the same time wait for each other, and the number of waits is not measured.
- The extension records can make `GetCommandMemoryUsed` larger than on the console (R53). This is unverified.
- A function that nxnative does not implement, and a sparse `nvnMemoryPoolMapVirtual` request, end the game with the name of the function. The `nvnDlss*` functions are in this group.
- `nvnDeviceGetInteger` has no table for the Switch 2 device. The entry 0x5F is not checked against a source.
- The host does not use the queue memory of a queue builder. It stores the sizes and the pointer.
- `Present.crop` holds the crop of the window, or the crop of the `vi` layer when the window has none. The blit of `nxkit-gpu` does not read it, so a crop does not change the picture. This is unverified on a running game.
- The layer size of `nn::vi::CreateLayer(w, h)` is recorded and no presenter reads it. A present shows the picture in the shape of the window texture.
- The frame history of the layer counts a frame as shown from the first `nn::vi::ListFrameHistory` call that sees it, because `Backend` reports no present time. Its ticks run at 19.2 MHz. An SDK of the Switch 2 counts at another rate. The host cannot tell the SDK apart.
- `nv::GetGraphicsMemoryInfo` writes 0x20 bytes when the NVN API major is 53 or less and 0x40 bytes otherwise. A game of SDK 10 or older that does not set `nvn_api` in `game.json` gets 0x40 bytes. This is unverified.
- `nn::vi::QueryDisplayBufferMaxResolution` answers 1920x1080 in both operation modes. The host has no fact of the size of the display.
- The dirty range tracking of nxnative for pools is not ported. `nvnMemoryPoolFlushMappedRange` tells the front end that the guest wrote the range, and the invalidate call does nothing.
- A copy or compute pass is not moved out of a render pass, so some merges are missed. The plan lists the textures of a dispatch and not its buffers, so a move cannot be proven safe. `NXKIT_DIAG=passmerge` counts the cases.
- The learned dead store is a guess from 120 frames. The coverage load (`coverload`) is a guess from the draw. Both are marked as such above.
- Memory aliasing between buffers and textures is not tracked. A buffer copy does not touch a texture of the same memory.
- A handle is resolved when the recording is decoded. A summary is decoded again when the same descriptor slot is registered again. A game that rewrites the descriptor memory without a register call is not noticed.
- A texture in a record is named by its GPU address. When two textures start at the same address, the newest one wins.
- A callee that sets state is assumed to leave it set for the caller, as nxnative does.
- A recording that overwrites the memory of another recording while it is still being written is found at its end, not before.
- Sparse textures are not supported. A sparse texture stops the game at `nvnTextureInitialize`.
- A stencil view or a stencil texture is registered as its depth view, because `TextureView` has no depth-stencil mode. The first one is logged.
- `nvnTextureReadTexels` of a texture that the GPU wrote last stops the game, because `Backend` has no readback.
- `nvnMultisampleStateSetSampleLocations` ignores the positions and logs the first call. The host does not use custom sample locations.
- A pool range that is initialised again with another size or address keeps its old ids reserved. The ids of a pool are never given back.
- The analysis counts a texture as one unit. It does not track mip levels or layers separately.
- The memory callback of a command buffer runs with no lock held. It may call the command buffer functions.

## Attribution

The layout and the GOB order are ported from nxnative. The names lists are copied from it. Refer to `THIRD_PARTY.md`.
