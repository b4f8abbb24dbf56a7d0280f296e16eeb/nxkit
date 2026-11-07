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
- `tic` encodes and decodes a texture header (TIC) and a sampler header (TSC) of 32 bytes. The bit positions follow the Maxwell structure loosely. They were not checked on hardware. A game that writes headers itself needs a check against real data.
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
- A record that does not fit goes to the next block behind a jump. When no block is left, `write` returns `OutOfCommandMemory` and changes nothing. The host calls the memory callback of the game and writes the record again.
- The handle of a recording is the host address of its first record.

### Summaries and the cache

`decode` reads a recording from its handle, follows jumps and calls, and gives a `Summary`. The summary has the effect stream (`Cmd` values), the state that the recording takes from its caller (`Needs`), the state that it leaves behind, and every address and handle that it resolved.
A called recording gets its summary under the state that the caller has at the call. State that the callee sets stays set for the caller, as in nxnative.

`cache` keeps the summaries by handle. A summary is valid for the inherited state that it needs, and a handle can have several. A key holds the needed parts only: the attachments, the scissor, the draw state and the bound textures and images. A draw needs the attachments, because the Metal indirect command buffer of a later step depends on their formats.
- The game writes the command memory again: the summaries of that handle and of every recording that calls it are dropped. This happens at `end_recording` and in `guest_wrote`.
- A texture address or a handle resolves to something else, for example a texture pool slot that is registered again: the summary is dropped at the next lookup and decoded again.
- One handle keeps at most 16 summaries.

`CacheStats` counts hits, misses, invalidations, revalidations, evictions and unresolved handles.

### The frame

A queue submit takes the summary of a recording under the default state of a queue and appends its effects to the `Frame`. The stream stays in the cache, so a game submits the same recording in every frame without a new decode.
The frame is held until `end_frame`. The host calls it at a present or when the CPU waits for the GPU. `FlushStats` counts each reason. An early flush does not know the later frames, so a texture that is not proven dead stays stored.
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

### Merge

Two adjacent render passes become one when all of these are true.
- The attachment sets are equal.
- Only a barrier lies between them.
- The second pass does not start with a clear or a discard.
- The second pass does not sample an attachment of the first pass.

### Load action

1. `Clear` when the pass starts with a full clear.
2. `DontCare` when the pass starts with a discard.
3. `Load` when the pass blends, tests depth or stencil, or samples the attachment.
4. `DontCare` when the attachment is not stored.
5. `Load` otherwise, because a draw may cover only a part of the attachment.

### Store action

A backward walk knows for each texture whether a later command reads it.
- `Store` when something later in the frame loads it, samples it, copies from it, presents it or reads it back.
- `Store` when nothing in the frame proves that it is dead. A later frame may read it, for example a TAA history buffer. These bytes are counted in `bytes_stored_assumed`.
- `DontCare` when the game discards it at the end of the pass, or when the host marked the texture frame-local.

The analysis does not guess that a texture is dead. The host has to say it. This keeps a history buffer safe.

### Memoryless

An attachment is memoryless when it is not stored, not loaded, not sampled, and only one pass of the frame uses it.

### Counters

`PassStats` has the number of render passes before and after the merge, the bytes loaded, the bytes stored, the bytes stored only by assumption, the memoryless bytes, the bytes of a naive load and store of every attachment of every unmerged pass, and the bytes saved. The size of an attachment is its first mip level.

## NVN functions

`entries` has the 743 names of nxnative (`names.txt`), the 484 that nxnative implements, and the table `RECORDED` of the names that this crate handles. The table has the command memory functions and the CPU wait functions too.
`Coverage::of` takes the function names that one game imports and returns three lists: handled, not handled yet, and not implemented in nxnative.

## Limits

- The crate takes typed values. The code that reads the raw guest arguments of an NVN call is not written. It belongs to `nxkit-rt` and the host.
- A copy or compute pass is not moved out of a render pass, so some merges are missed.
- Memory aliasing between buffers and textures is not tracked. A buffer copy does not touch a texture of the same memory.
- A handle is resolved when the recording is decoded. A summary is decoded again when the same descriptor slot is registered again. A game that rewrites the descriptor memory without a register call is not noticed.
- A texture in a record is named by its GPU address. When two textures start at the same address, the newest one wins.
- A callee that sets state is assumed to leave it set for the caller, as nxnative does.
- The queue starts every submit with the default state. The state of a previous submit is not kept.
- A recording that overwrites the memory of another recording while it is still being written is found at its end, not before.
- Sparse textures are not supported.
- The analysis counts a texture as one unit. It does not track mip levels or layers separately.

## Attribution

The layout and the GOB order are ported from nxnative. The names lists are copied from it. Refer to `THIRD_PARTY.md`.
