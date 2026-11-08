# Diagnostics of the GPU path

The GPU path (`nxkit-rt`, `nxkit-nvn`, `nxkit-gpu`) has opt-in diagnostics for debugging a game. Environment variables turn them on. They are off by default. When they are off, each one costs one cached check. This page lists every category, knob and variable, and explains how to read the render health report.

The code is in `nxkit_nvn::diag` (the shared `NXKIT_DIAG` parser and the health report), `nxkit-gpu/src/diag.rs` (knobs, filters, sampled frames) and `nxkit_rt::diag!` (the `cmd` category).

## Switches

| variable | effect |
|---|---|
| `NXKIT_DIAG` | Comma separated list of categories (see below). `all` turns on every category. |
| `NXKIT_DIAG_TEX` | Limits the texture and pass categories to one format, one size, or both, separated by `/`. The format is the Metal format name as `Debug` prints it. The size is `WxH` or `WxHxL` with the layer count. Examples: `R16Unorm/256x256`, `1600x900`, `Rg11B10Float`. Without it, every texture passes. |
| `NXKIT_DIAG_DIR` | The directory of the diagnostic files: the `nobar` file and the `img` pictures. Default `/tmp/fmap`. |
| `NXKIT_KNOB` | Comma separated list of knobs (see below). Knobs change what the encoder does. |
| `NXKIT_KNOB_FILE` | A file with the same list as `NXKIT_KNOB`. The encoder reads it again every frame, so you can change a knob while the game runs. |
| `NXKIT_PAINT_FS` | Comma separated fragment program keys that the `paint` and `paintdir` knobs paint. When it is empty, they paint every draw into one colour target. |
| `NXKIT_WATCH_TEX` | Comma separated texture ids. The encoder prints the passes that render into them and the copies that touch them (at most 40 each). |
| `NXKIT_WATCH_ADDR` | A GPU address in hex. The decoder prints the texture that each record word at this address resolves to (at most 30). There is no default address. |
| `<NXKIT_DIAG_DIR>/nobar` | A file with `lo hi`. The passes in `[lo, hi)` get a full barrier from all earlier work. Use it to bisect a missing barrier. It is read every frame. |

The behaviour switches below are not diagnostics, but they help to find the cause of a defect:

| variable | effect |
|---|---|
| `NXKIT_ENC_THREAD=1` | Encodes on a thread of its own (the `enc_thread` key of `game.json` does the same). |
| `NXKIT_ENCODE_THREADS` | The number of chunk threads of the encoder. 0, the default, picks the number from the cores. |
| `NXKIT_SCALE` | The render scale of render targets. The default is 1.0. |
| `NXKIT_WALK_CACHE=1` | Turns on the template cache of the walker. It is off by default, because in Breath of the Wild it costs more than it saves. |
| `NXKIT_SCREEN` | The screen the window opens on, by its index (0 is the built-in one) or a part of its name, for example `XB273U`. The refresh rate that the host uses is the one of the screen the window is on now, also after you drag it to another screen (`docs/pacing.md`). |
| `NXKIT_SET_REFRESH=120` | Switches the display of the window to this refresh rate at the start, at the size it has, and puts the old mode back at the exit (`set_refresh` of `game.json` does the same). Use an even multiple of the frame rate: 120 for a game at 60 fps. The switch is for this process only, so the system restores the mode also after a crash. |
| `NXKIT_TIMED_PRESENT=1` or `0` | Forces timed presents (`presentAfterMinimumDuration:`) on or off. By default they are on for a screen that adapts its refresh rate (ProMotion) and off for the others. |
| `NXKIT_GPU_LABELS=1` | Names the encoder of each pass (pass index, first attachment, draws) for the Metal debugger. Instruments does not show the labels of Metal 4 encoders. |
| `NXKIT_PASS_LIST=1` | With `NXKIT_DIAG=passes`: also prints every pass of the frame with its attachments and their load and store actions. |
| `NXKIT_OVERLAP=1` | The CPU runs one frame ahead of the GPU: `nvnSyncWait` for the newest frame waits for the one before it, and every bound uniform buffer reads a copy (`docs/plans/fence-overlap.md`). |
| `NXKIT_PIPELINE_ASYNC=0` | Builds a missing render pipeline in the encode, as before. By default it compiles on a background thread and the draw is left out until it is ready (`skip.pending`). |
| `NXKIT_AUDIO_CLICKS=1` | Prints every jump at the border of two rendered audio blocks that is much larger than any step inside them (what a click sounds like), with the wall clock in ms. |
| `NXKIT_AUDIO_DEVICE_FRAMES` | The IO buffer of the audio output device in frames. Without it the device keeps its own (the log names its transport and buffer at the start). |
| `NXKIT_PACE_DIAG=1` | Prints, every 120 presents, the mean frame interval, pacing wait, CPU cost and GPU time in vsyncs, and the state of the frame step. In the state line, `hz` is the unit of the frame step (60 for a game at 60 fps, 120 for a target of 120). It is not the refresh rate of the screen and it does not change with it. The screen is in the `nxkit-host: screen ...` line, see below. |
| `NXKIT_VERIFY_CACHE=1` | Walks every recording again without the template cache and compares the result. This is slow. |
| `NXKIT_COPYBACK=0` | Turns off the copy-back of GPU-written textures in CPU-visible pools. |
| `NXKIT_COPYBACK_MAX_MB` | The most storage that one frame copies back. The default is 128. |

## The screen line and the judder line

The host prints one `nxkit-host: screen` line at the start, and another one each time something in it changes: the window moves to another screen, the display mode changes, `NXKIT_SET_REFRESH` switches the mode, or the measured tick rate differs from the plan.

```
nxkit-host: screen "XB273U NV" refresh 120.00 Hz (CoreGraphics mode), link 120 Hz, 2 ticks per 60 fps frame
nxkit-host: screen "XB273U NV" refresh 144.00 Hz (CoreGraphics mode), link 60 Hz, 1 tick per 60 fps frame (uneven); 144 Hz is not an even multiple of 60 fps, so frames alternate between two lengths (judder): set NXKIT_SET_REFRESH=120
nxkit-host: screen "Built-in Liquid Retina XDR Display" refresh 120.00 Hz (adaptive, highest rate), link 60 Hz, 1 tick per 60 fps frame, timed presents
```

It names the screen, the refresh rate with its source (`CoreGraphics mode`, `NSScreen`, or `adaptive, highest rate`), the rate of the display link, the ticks that one game frame stays, `(uneven)` when no whole number fits, `timed presents` when they are on, and any mode switch. When the link ticks at a rate more than 4% off the plan, the host prints that too and uses the measured rate.

`NXKIT_DIAG=judder` prints, every 120 shown frames, a histogram of the refreshes that each shown frame stayed, from the times of `Drawable::on_presented`:

```
DIAG judder refresh 120.00 Hz: 1x 0 2x 118 3x 2 4x 0 5x+ 0 dropped 0 mean 2.02 refreshes
```

A steady 60 fps game on 120 Hz has all frames in `2x`. Two neighbouring bins (`2x` and `3x` on 144 Hz, 60 fps) are judder. A frame in `4x` or more is a late frame. `dropped` counts the presents that the display did not show.

## Sampled frames and limits

Every print that can repeat once per draw, pass or frame has a limit. There are three kinds of limit:

- *first N*: the category prints at most N lines for the whole run.
- *every Nth*: the category prints one line every N frames, presents or calls.
- *sampled*: the per-draw and per-pass categories print only on sampled frames. A sampled frame is one out of 300 encoded frames, from frame 150 on. On a sampled frame, `draw` and `ubo` print at most 2000 lines each. `NXKIT_DIAG_TEX` limits them to passes that have an attachment that passes the filter.

A "frame" in this section is one encoded batch (`Gpu::encode_frame`). A game frame can flush more than once.

## Categories (`NXKIT_DIAG`)

The cost column shows the cost when the category is on: low, medium (extra work per draw or texture), or high (reads guest memory or the GPU every frame).

| category | crate | prints | limit | cost |
|---|---|---|---|---|
| `health` | nvn, gpu | the render health report (see below) | every 300 frames, and when a reason counts for the first time | low |
| `stats` | nvn, gpu | the encode counters, the walker counters, why the render passes start, and the decode time and the counters of the decode cache | every 30 frames, the decode every 300 submits | low |
| `cmd` | rt | each command buffer call: add memory, begin, end, call, memory queries, submits, clipped memory and overflow blocks | first 2000 per call site | medium |
| `decode` | nvn | a record that the decoder could not read, with the recording and its spans | first 50 | low |
| `tex` | gpu | why a texture was not resolved or created, and the unresolved handles of each draw | first 300, and 400 distinct draws | low |
| `buf` | gpu | a vertex, index or uniform buffer outside guest memory | first 300 | low |
| `target` | gpu | render targets that cannot form a pass (not renderable, size mismatch, the same slot twice, no target), and alias conflicts | first 300, alias lines on sampled frames | low |
| `draw` | gpu | the state of each draw (programs, args, depth, raster, masks, blend, inputs, streams, uniform buffers, textures) and the attachments | sampled | medium |
| `ubo` | gpu | the uniform buffers of each draw, from guest memory now and from the snapshot of the walk, and the first floats of two vertex streams | sampled | high |
| `pixel` | gpu | nine pixels of the presented texture, and draw and texture details | every 60th present, and draws on sampled frames | medium |
| `pixelall` | gpu | a counters line per frame and the lit pixels of each present, to match frames while bisecting with `nobar` | every 10th frame or present | medium |
| `present` | gpu | the counters of the present path (ticks, asked, found, blitted) and the presented texture | every 120th tick or present | low |
| `memory` | gpu | the Metal memory of the process and the number of resident allocations | every 120 frames | low |
| `depth` | gpu | distinct depth and stencil states, and the depth attachments of the passes | 40 distinct, 20 of every 500 passes | low |
| `clear` | gpu | depth and stencil clears | 20 of every 1000 | low |
| `copy` | gpu | copies that the encoder refused | first 400 | low |
| `tcopy` | gpu | copies that touch a texture that passes `NXKIT_DIAG_TEX`. It needs the filter. | first 300 | low |
| `comp` | gpu | each dispatch with its program, groups and result | sampled | low |
| `barr` | gpu | for each pass, the bound textures that the barrier planner does not know | sampled | medium |
| `lum` | gpu | nine pixels of each stored colour pass that passes the filter, read after the frame | sampled | high (waits for the GPU) |
| `img` | gpu | with `lum`: targets of 640x360 or more are sampled on a 96x54 grid and written as PPM pictures into `<NXKIT_DIAG_DIR>/img` | sampled | high |
| `upload` | gpu | failed uploads, refused pitch-linear uploads, and every upload of a texture that passes `NXKIT_DIAG_TEX` | first 300 / first 200 | low |
| `stale` | gpu | guest bytes of a texture that changed between frames, with the residency of the front end. A change while the texture is not `GuestNewer` is a CPU write that the front end did not see. | first 200 | high (hashes every texture that passes the filter every frame; set `NXKIT_DIAG_TEX`) |
| `life` | gpu | Metal textures made and dropped, for textures that pass the filter | first 300 | low |
| `contain` | gpu | a new texture whose storage lies inside another one, or holds another one | first 300 | medium |
| `split` | gpu | a new texture at the address of another one with a different description | first 300 | low |
| `copyback` | gpu | the first copy-back of each texture, refused copy-backs, pins at the native size and a summary | first 300 textures, 100 refusals, summary every 300 frames | low |
| `fit` | gpu | views made for the texture type that a shader declares | first 300 | low |
| `hnd` | gpu | each distinct handle with its backing texture and view | 2000 distinct | medium |
| `bc` | gpu | the first bytes of each compressed texture in guest memory | 200 distinct | low |
| `prog` | gpu | programs that did not load, unknown binding kinds, draws without a vertex stage | once per program, first 40 draws | low |
| `pipe` | gpu | render, compute and depth-stencil states that Metal did not build | first 300 | low |
| `overlap` | gpu | the flushes of the CPU, the dynamic pages, and per kind of buffer binding the bytes bound and the bytes changed since the last frame (`docs/plans/fence-overlap.md`) | every 120 frames | high (hashes every bound range) |
| `judder` | nvn | the histogram of the refreshes each shown frame stayed, and the mean (see above) | every 120 shown frames | low |
| `hitch` | gpu | each frame whose encode took 7 ms or more or that built pipelines or textures, with the wall clock in ms, to put next to the late frames of `NXKIT_PACE_DIAG` | each such frame | low |
| `kpass` | gpu | the render passes that could run as compute dispatches (`docs/plans/kernel-passes.md`, step 0), the runs of them, and why the others cannot | every 300 frames | low |
| `walkmiss` | gpu | with `NXKIT_WALK_CACHE=1`: the field of the walker state that made a call miss the templates of its handle | every 20000 misses | medium |
| `passes` | gpu | the render passes of a frame by their number of draws and by the size of their first attachment | every 300 frames | low |
| `indirect` | gpu | draw records without arguments (transform feedback, `DrawTexture` or unknown draw kinds) | at powers of two per kind | low |
| `mesh` | gpu | the mesh and indirect draws of the frame (`EncodeStats::kinds`): encoded and skipped by reason for each class (mesh, mesh-indirect, indirect, multi-indirect-count), the Metal draws they became, the compute pre-passes and their records, and what was clamped. The first frame with such draws prints the line without the category. `stats` turns it on too. | every 30 frames | low |

## Knobs (`NXKIT_KNOB`)

Knobs are experiments. A frame with a knob does not render correctly. Use knobs to find out which part of the state causes a defect.

| knob | effect |
|---|---|
| `nodepth` | Turns off the depth and stencil tests. |
| `nocull` | Turns off culling. |
| `nooffset` | Sets the depth bias to zero. |
| `noeq` | Changes the depth test `equal` to `always`. |
| `flat` | A draw into four colour targets that its fragment shader all writes gets constant G-buffer values. |
| `paint` | Paints magenta every draw into one colour target, or only the programs of `NXKIT_PAINT_FS`. |
| `paintdir` | Same as `paint`, but the colour is the absolute value of the first varying. |
| `cubearr` | Makes a cube view as a cube array. |
| `allbar` | Gives every pass a full barrier from all earlier work. This is the same as a `nobar` range that covers every pass. |
| `nowait` | `nvnSyncWait` returns at once. The game may write memory that the frame on the GPU reads; it measures what the wait costs. |
| `overlap` | The same as `NXKIT_OVERLAP=1`, switched while the game runs. |
| `nosnap` | With the overlap on, the uniform buffers are not copied. The picture is wrong; it measures what the copies cost. |
| `vsbar` | A render pass that samples what an earlier pass wrote holds its vertex stage too, also when no vertex shader of the pass samples (the behaviour before). |
| `nosnapsbuf` | With the overlap on, storage buffers that draws only read use the buffer itself instead of the walk's copy (the behaviour before; the dialog text of Breath of the Wild then flies in). |
| `nobarrier` | Gives no pass a barrier. The picture is wrong; it measures what the barriers cost the GPU. |

## The render health report

Defects that drop work are often silent. A texture upload is refused, a pool slot points to a dropped texture, a draw has no detail, or Metal refuses a view. The picture then has a hole, and the log shows no error. The health report counts every place that drops, refuses or degrades work. It counts always, at the cost of one atomic add. `NXKIT_DIAG=health` prints the counts.

The report prints every 300 encoded frames. It also prints in the frame where a reason counts for the first time. When nothing was dropped, the report is one line:

```
health: clean (frame 1800)
```

Otherwise it has one line for each reason with a count that is not zero:

```
health: frame 1800: work was dropped
health:   reason                   since last      total
health:   skip.detail                       7          7 NEW
health:   skip.cull                       112        112 NEW
health:   skip.texture                     41         41 NEW  e.g. handle 0x2fd: NoDescriptor
health:   walk.draw-no-args                 7          7 NEW
health:   tex.view-failed                   1          1 NEW  e.g. attachment view TargetView(level 0, layer 3) of TextureDesc { format: Bc4Unorm, kind: D3, width: 64, height: 64, depth: 16 }: a view cannot change...
health:   upload.linear-refused             0          3
```

- `since last` is the count since the previous report, and `total` is the count since the start. A reason with `0` since the last report happened earlier, for example during loading. A reason that grows in every report is a defect that is still active.
- `NEW` marks a reason that counts for the first time in this report.
- `e.g.` shows up to three distinct examples (format, size, handle, program key, the error of Metal). Only the first examples are kept, and each example is cut after 160 bytes.
- The head line says `clean apart from culled draws` when only `skip.cull` counts. A draw with both faces culled draws nothing on the hardware either.

The `skip.*` lines count the draws, copies and dispatches that the encoder left out, by `nxkit_gpu::Skip`. The other lines count the causes that come before a skip. One cause can give many skips, and some causes give no skip but a degraded result.

| reason | where | meaning |
|---|---|---|
| `skip.detail` | encoder | A command has no detail from the walker. Look at `walk.draw-no-args` and `walk.draw-no-vertex`. |
| `skip.program` | encoder | The shader library or its reflection is missing. Look at `program.failed`. |
| `skip.binding` | encoder | The reflection has a binding kind that the encoder does not fill. |
| `skip.primitive` / `skip.index` | encoder | A primitive type or an index type that has no Metal equivalent. |
| `skip.format` | encoder | A vertex, texture or attachment format that has no Metal equivalent, or a colour target with a compressed or depth format. |
| `skip.target` | encoder | The render targets cannot form a pass (`target` category). |
| `skip.cull` | encoder | Both faces are culled. This is not a loss. |
| `skip.texture` | encoder | A texture could not be resolved, created or viewed. |
| `skip.pipeline` | encoder | A pipeline did not compile. Look at `pipeline.failed`. |
| `skip.buffer` | encoder | A buffer outside guest memory, or a full scratch buffer. |
| `skip.copy` | encoder | A copy between textures of different formats or scales, or a copy without textures. |
| `skip.transfer` | encoder | An upload or a copy-back failed. Look at `upload.*` and `copyback.*`. |
| `skip.encode` | encoder | Metal refused an encoded command. |
| `skip.pending` | encoder | The pipeline still compiles on a background thread. The draw shows a frame or two later. It is not a loss. |
| `walk.draw-no-args` | walker | A draw record without draw arguments (transform feedback, `DrawTexture` or an unknown draw kind). The draw gets an empty detail. |
| `walk.draw-no-vertex` | walker | The bound program has no vertex stage, or a mesh draw finds no program with a mesh stage. |
| `walk.stage-ignored` | walker | A geometry or tessellation stage. The encoder runs the vertex, fragment, task and mesh stages only. It counts once per program key. |
| `decode.unresolved` | decoder | A texture handle or address in a recording names no texture. It counts once per decoded recording. A recording that the cache answers does not count again. |
| `decode.bad-record` | decoder | A record could not be read. The rest of the recording is lost. |
| `tex.handle-unresolved` | encoder | A texture handle has no descriptor or no texture record. This is the case of a pool slot that points to a dropped texture. |
| `tex.format-unsupported` | encoder | A texture format or target that has no Metal equivalent. |
| `tex.create-failed` | encoder | Metal did not create a texture. |
| `tex.view-failed` | encoder | Metal did not create a view. For example, a view between a compressed and an uncompressed format, or a type that the texture cannot be seen as. |
| `upload.failed` | encoder | The deswizzle or the copy of a texture upload failed. The examples include the error of the kernel, for example "the texture is not usable by the kernel" for a Depth16 upload. |
| `upload.linear-refused` | encoder | A pitch-linear texture with layers, levels, 3D or no stride. Only one level and one layer are copied. |
| `upload.scaled-lost` | encoder | Guest data that does not reach a scaled texture. |
| `copyback.refused` | encoder | A GPU-written texture in CPU-visible memory that could not be copied back. The CPU reads stale bytes. |
| `copyback.over-budget` | encoder | A copy-back that was left out because the frame went over `NXKIT_COPYBACK_MAX_MB`. |
| `program.failed` | encoder | A program that did not load. It counts once per key. |
| `pipeline.failed` | encoder | A render, compute or depth-stencil state that Metal did not build. It counts every attempt. |

The walker counts only when it reads a record. A recording that plays from the template cache does not count again. Without `NXKIT_WALK_CACHE=1` every frame counts. With `NXKIT_VERIFY_CACHE=1`, a recording is walked twice and counts twice.

To add a reason, add it to `reasons!` in `nxkit-nvn/src/diag.rs`, call `nxkit_nvn::diag::note` where the work is dropped, and add a line to the table above.
