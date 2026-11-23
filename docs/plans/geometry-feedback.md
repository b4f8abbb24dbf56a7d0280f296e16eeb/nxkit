# Geometry shaders with transform feedback: run the captures that Xenoblade Chronicles 3 draws from

Status: plan (2026-11-23). Not started. The findings come from Xenoblade Chronicles 3 (`010074F013262000`, the save
in "Sieches Feld") on the M2 Max.

## The problem

Xenoblade Chronicles 3 shows long stretched polygons over its characters, the statues and parts of the scene. The
bytes these draws read are not the console's: a transform feedback capture writes them on the console, and the host
does not run it.

The sequence of one capture, as the walker sees it:

1. `BindTransformFeedbackBuffer(0, addr, size)`, sizes 0x3c78 to 0x120e8.
2. `BeginTransformFeedback(control)`. The control block is 0x40 bytes just before the buffers. Its two halves (`+0x00`
   and `+0x20`) take turns from frame to frame.
3. `SetRasterizerDiscard(true)`, then `DrawTransformFeedback(points, counter)`: the vertex count comes from the counter
   at the base of the other half, so from the capture of the frame before.
4. `EndTransformFeedback`, `SetRasterizerDiscard(false)`.
5. Later draws read the captured buffer as storage buffer 0 of their vertex stage (`vp_s0`, about 245 draws a frame:
   `ce974a4f8a3e0671`, `8a42b69f1519309b`, `b13b10c7f672cfb3`, `6babc936d58fcd8f` and others). A game binds a 16-byte
   buffer of shared data to slot 0 when it is done; that binding captures nothing.

The bound program has a geometry shader (program slot 2). The four geometry shaders with feedback in the build
(`293ecebc0136519b`, `59060483d9ec9c62`, `8a37ca1cf06f439e`, `c01e3ade8e15a842`) take points and emit at most 1 or 16
points each (`geometry.output` 1, `vertices` 1 or 16, `stride` 21 or 49 words). Their `_bgv` libraries have `main0_tf`
with `tf_strides` 172 or 52 bytes. So the count of captured vertices depends on the data, and the next frame reads it
back from the counter.

Leaving the readers out is no workaround: without them the scene goes dark (about 74,000 draws per 300 frames).

What exists:

- The translator: `main0_gs` in the vertex library (captures the vertex outputs, `dm_geometry_capture` at buffer 13),
  the kernels `_bg0` to `_bg4` (and the lean `_bl*`), `_bgv` that draws the kernel output, and `main0_tf` of `_bgv`
  (`dm_tf_capture` at buffer 14, the feedback buffers at 8 + b, the order buffer for the records). See
  `docs/translator.md`, "Geometry, task and mesh shaders", and `crates/nxb-xlate/src/msl/variant/tf.rs`.
- The walker: `walk::feedback` keeps the ranges that a capture wrote. Draws that read one as a vertex stream are left
  out (`skip.feedback`). `DrawTransformFeedback` has no draw arguments (`walk.draw-no-args`).
- The encoder: rasterizer discard runs `main0_discard` with rasterization off. A geometry shader that does not only
  pick the layer is ignored (`walk.stage-ignored`).

## The plan

1. **Walker: the capture state.** Keep the bound buffers, begin/end/pause/resume and the control address per queue
   (in the `State` with a cache group, or as now in `walk::feedback` with templates off for such recordings). A draw
   inside a capture gets a `CaptureDetail`: the buffers, the control block, the records of the capture before it.
   `DrawTransformFeedback` gets `DrawArgs::Feedback { mode, counter }`.
2. **Encoder: vertex-only captures.** For a draw without a geometry shader: the `main0_tf` entry of `_b`, rasterization
   off, the buffers at 8 + b, `dm_tf_capture` at 14 (offsets, the record limit, first, count, base instance). Only
   arrays with list topologies: the record of a vertex is its place in the draw.
3. **Encoder: geometry jobs.** For a draw with a geometry shader: the vertex capture (`main0_gs`, rasterization off) into
   a scratch buffer, the dispatch of `_bg<t>` for the input topology, a compaction pass that turns the emitted
   vertices into the order buffer and the record count, and `_bgv` `main0_tf` over the order buffer. The ABI of the
   kernel buffers (`vertex_output`, `geometry_vb_output`, `geometry_ib_output`, `vb_info`, `trb_data`) comes from
   `crates/nxb-xlate/src/msl/geom.rs`.
4. **The counters.** At the end of a capture, a small kernel writes the bytes written of each buffer into the control
   block in guest memory, as the console does. `DrawTransformFeedback` becomes an indirect draw: a kernel turns the
   counter into `MTLDrawPrimitivesIndirectArguments` (bytes / stride of the stream).
5. **Barriers.** The barrier planner follows textures only. A pass that captures or that stores from the vertex stage
   must make the later passes that read the buffers wait (`Scope::ToLaterPasses`, after the vertex stage, or a
   consumer barrier in the readers).
6. **Readers.** With the capture run, `skip.feedback` applies only to captures that the host could not run (an indexed
   or strip capture, a missing entry), and also to storage buffer readers then.

## Tests

- Walker: a capture with pause and resume gives the records of each draw; `DrawTransformFeedback` reads the count of
  the capture that wrote its counter.
- Metal: a vertex capture of 3 points into a buffer gives the strides of `tf_strides`; a geometry job over a point list
  that emits 0 or 2 points per input gives the compacted order and the count.
- Xenoblade Chronicles 3 in "Sieches Feld": no spikes over 30 frames, `skip.feedback` 0.
