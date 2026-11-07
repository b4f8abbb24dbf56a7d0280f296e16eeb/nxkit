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

## Frame recording

A recorder keeps the state of one command buffer and writes `Cmd` values into a `CmdStream`. A command is a small `Copy` value. Texture lists share one `Vec` and a command points into it.
A queue submit appends the stream to the `Frame` and moves the spans. The stream stays valid, so a game can submit a command list in every frame.
All vectors keep their memory from frame to frame. A warm frame does not allocate. A test checks the capacity.

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

`entries` has the 743 names of nxnative (`names.txt`), the 484 that nxnative implements, and the table `RECORDED` of the names that this crate handles.
`Coverage::of` takes the function names that one game imports and returns three lists: handled, not handled yet, and not implemented in nxnative.

## Limits

- The crate takes typed values. The code that reads the raw guest arguments of an NVN call is not written. It belongs to `nxkit-rt` and the host.
- A copy or compute pass is not moved out of a render pass, so some merges are missed.
- Memory aliasing between buffers and textures is not tracked. A buffer copy does not touch a texture of the same memory.
- A handle is resolved when it is bound, not when the draw runs. A game that rewrites a descriptor after the bind is not handled.
- Sparse textures are not supported.
- The analysis counts a texture as one unit. It does not track mip levels or layers separately.

## Attribution

The layout and the GOB order are ported from nxnative. The names lists are copied from it. Refer to `THIRD_PARTY.md`.
