# Overlapping the CPU and the GPU for games that wait on their newest fence

Status: 2026-11-08. Step 2 is built behind `NXKIT_OVERLAP=1` and holds 60 fps in Breath of the Wild with a stable picture.
The measurements changed the design: see "What the measurements showed".

## The problem

Breath of the Wild waits in `nvnSyncWait` every frame for the fence of the frame it just submitted. The frame time is
then the CPU work of the game and the host plus the GPU time, one after the other. In Zora's Domain at 1080p that is
about 11 ms of CPU and 10 ms of GPU, so 60 fps does not hold. The same pattern is in earlier notes:
"BotW waits every frame for its newest fence, so CPU and GPU run one after the other".

Measured with the `nowait` knob (`nvnSyncWait` returns at once, nothing is protected): the frame step goes to hold 1
and holds it, 60 fps, at a CPU cost of 0.67 vsyncs and a GPU time of 0.99 vsyncs. A UI element flickers, because the
game writes data for the next frame while the GPU still draws the current one.

The cause is game specific; the fix belongs in the host and is general. A game that waits on an older fence already
overlaps and is not affected.

## Why the GPU sees the CPU's writes

The GPU reads guest memory without a copy: `GuestMemory` is one no-copy `MTLBuffer` over the whole guest memory.
Uniform, vertex, index and storage buffers are bound by their guest GPU address. Only uniform ranges that a recording
updates inline (`UpdateUniformBuffer`) get a snapshot in the frame's scratch memory. Textures in CPU-visible pools reach
the GPU through uploads, which are copies already.

So when `nvnSyncWait` returns before the GPU is done, every CPU write to a buffer that the frame on the GPU reads is a
race.

## What the host knows

- The read set of a frame: the walker's `FrameDetail` holds, per draw and dispatch, the bound uniform buffers
  (address, size), the vertex streams, the index buffer and the storage buffers.
- The CPU writes: not from `nvnMemoryPoolFlushMappedRange`. Breath of the Wild flushes about 73 ranges a frame, but all
  of them are command memory (its recordings), which only the walker reads. It writes its uniform and vertex data to
  coherent memory without a flush. What changed is only visible in the bytes themselves.

## The design

1. **Uniform buffers: always copied.** All bound uniform ranges of a frame are about 750 KiB in Breath of the Wild, so the
   walk copies every one of them (`UboDetail::snapshot`) and the encoder uploads each copy once into the frame's scratch
   memory before the chunks start. No detection is needed. (Built.)
2. **Vertex buffers: copied when their bytes change.** Bound vertex data is about 19 MiB a frame, but only about 52 KiB
   of it changes from frame to frame (UI, particles). A hash per binding, compared with the last frame that had it,
   marks a range dynamic; dynamic ranges get the same shadow copy. Static data (meshes, textures) stays zero-copy.
   (Not built yet; the picture was stable without it.)
3. **One frame of overlap.** With the switch on, `nvnSyncWait` for fence N waits for fence N - 1 only. The batch ring
   already bounds the frames in flight.
4. **Full waits where the CPU reads GPU results.** A frame that copies GPU-written textures back to CPU-visible memory,
   has a `CpuRead`, or writes a storage buffer in CPU-visible memory keeps the full wait for its fence. Counter reports
   (timestamps) are written when the frame ends, one frame later than now; the game's GPU time reads then lag one frame.
5. **A safety net that learns.** A check mode compares, after the frame, the bytes of the zero-copy bindings with their
   hash at encode time. A difference is a write that the shadow copies missed. It is counted in the health report
   (`overlap.unprotected`), the range becomes dynamic, and the next frames wait fully until a frame passes without a
   miss. Index buffers did not change at all in Breath of the Wild; storage buffers change, but likely because the GPU
   writes them, so they must not be copied.
6. **A switch.** `NXKIT_OVERLAP=1` (built); a `game.json` key `overlap` later. Off by default until a game is verified,
   so that every step can be compared with the `nowait` knob and the plain wait.

## What the measurements showed

In Zora's Domain at 1080p, per frame:

| binding | bindings | bytes | changed since the last frame |
|---|---|---|---|
| uniform | ~1560 | 754 KiB | ~210, 105 KiB |
| storage | ~190 | 272 KiB | ~115, 175 KiB |
| vertex | ~1220 | 19 MiB | ~105, 52 KiB |
| index | ~800 | 2.5 MiB | 0 |

- Plain wait: hold 2, 30 fps, CPU cost ~0.7 vsyncs, GPU ~0.6 vsyncs.
- `nowait` (no protection): hold 1, 60 fps, a UI element flickers.
- `NXKIT_OVERLAP=1` (uniform copies, wait for N - 1): hold 1, 60 fps, stable picture, CPU cost and GPU time ~0.8 vsyncs.
  Single frames are still late now and then (see the hitches below).

The first version uploaded the snapshots from the chunks through a shared, locked map; the encode went from about 5 to
10 ms. They are now uploaded once before the chunks.

## Measurements before the build (done)

The diagnostic `NXKIT_DIAG=overlap` runs without changing the behaviour and prints:

- the flush calls and bytes per frame, and the dynamic pages;
- the bytes of the frame's buffer bindings that overlap dynamic pages, and the flushes that race with the frame on the
  GPU (both near zero for Breath of the Wild, because its flushes are command memory);
- per kind of binding, the distinct bindings and bytes, and how many of them changed since the last frame (the table
  above). It hashes every bound range each frame, so it is slow.

## Risks

- A write to a zero-copy binding that is not caught races with the GPU. The check mode of step 5 finds them.
- A game that reads back GPU results without a `CpuRead` that the decoder sees (a storage buffer that a compute
  shader writes and the CPU reads) would read stale data. Step 4 covers the known cases.
- More scratch memory per frame.

## Order of work

1. The `overlap` diagnostic, and a run in Zora's Domain with and without `nowait`. (Done.)
2. Shadow copies of all uniform buffers, the `NXKIT_OVERLAP` switch, the one-frame wait. (Done.)
3. Dynamic vertex ranges by hash.
4. The safety net (check mode and health reason).
5. The dropped frames at 60 fps: pipeline builds, the frames with twice the passes, the cost of the encode.
6. Turn it on for Breath of the Wild in its `game.json`, then try a second game (Ocarina of Time on Switch 2) to see what
   of it is general.
