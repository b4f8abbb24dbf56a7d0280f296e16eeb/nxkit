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

The variables `NXKIT_ANE_DUMP`, `NXKIT_ANE_STUDY`, `NXKIT_COREML_PACKAGE`, `NXKIT_NDSR_PACK` and `NXKIT_NDVE_DAT` belong to ignored tests and are not read by the host.

The behaviour switches below are not diagnostics, but they help to find the cause of a defect:

| variable | effect |
|---|---|
| `NXKIT_ENC_THREAD=1` | Encodes on a thread of its own (the `enc_thread` key of `game.json` does the same). |
| `NXKIT_ENCODE_THREADS` | The number of chunk threads of the encoder. 0, the default, picks the number from the cores. |
| `NXKIT_SCALE` | The render scale of render targets. The default is 1.0. |
| `NXKIT_WALK_CACHE=1` | Turns on the template cache of the walker. It is off by default, because in Breath of the Wild it costs more than it saves. |
| `NXKIT_SCREEN` | The screen the window opens on, by its index (0 is the built-in one) or a part of its name, for example `XB273U`. The refresh rate that the host uses is the one of the screen the window is on now, also after you drag it to another screen (`docs/pacing.md`). |
| `NXKIT_SET_REFRESH=120` | Switches the display of the window to this refresh rate at the start, at the size it has, and puts the old mode back at the exit (`set_refresh` of `game.json` does the same). Use an even multiple of the frame rate: 120 for a game at 60 fps. The switch is for this process only, so the system restores the mode also after a crash. |
| `NXKIT_TIMED_PRESENT=1` or `0` | Forces timed presents (`presentAfterMinimumDuration:`) on or off. They are off by default: on a ProMotion screen they let macOS lower the panel rate, and frames waited several vsyncs for the screen. |
| `NXKIT_VRR=0` | Turns off variable refresh (VRR) and keeps the fixed grid. By default an adaptive screen with a variable frame step (a target above 60 with a state page) gets a range for the display link and shows each frame as soon as it is ready, see `docs/pacing.md`. The screen line says `VRR on`. |
| `NXKIT_VRR_MIN` | The lowest rate of the VRR range of the link in Hz, the floor the game needs. The default is 60. |
| `NXKIT_GPU_LABELS=1` | Names the encoder of each pass (pass index, first attachment, draws) for the Metal debugger. Instruments does not show the labels of Metal 4 encoders. |
| `NXKIT_PASS_LIST=1` | With `NXKIT_DIAG=passes`: also prints every pass of the frame with its attachments and their load and store actions. |
| `NXKIT_OVERLAP=1` | The CPU runs one frame ahead of the GPU: `nvnSyncWait` for the newest frame waits for the one before it, and every bound uniform buffer reads a copy (`docs/plans/fence-overlap.md`). |
| `NXKIT_NATIVE_CACHE=<n>` | How many plans of the native replacements (`nn::ndve`, `nn::ndsr`) the cache keeps (default 4, the least recently used goes first). A plan holds tens of MB of scratch. |
| `NXKIT_NDVE=0` | Turns the native replacement of the movie network (`nn::ndve`) off. The game's own chain of compute dispatches runs, as it does when the weights or the bindings do not match (`docs/plans/ndve-impl.md`). |
| `NXKIT_NDVE_COMPARE=1` | With `NXKIT_DIAG=ndve`: on one movie frame in 120 (the 10th, the 130th and so on), the game's chain and the native plan both run. The plan writes copies of its pictures. Four frames later the output of the plan and the output of the game are read back and the line `DIAG ndve compare` gives the largest difference in each channel and the number of differing pixels. The game's chain is noisy (a race between lanes), so judge by the largest difference. |
| `NXKIT_NDVE_ANE=0` | Keeps NDVE on the GPU. By default the movie network runs on the Neural Engine, made at decode time ahead of the game (`docs/plans/ndve-ane.md`); the Metal plan is the fallback for every picture without a result in time. |
| `NXKIT_NDVE_ANE_CACHE=<dir>` | The folder of the Neural Engine model: the `.mlpackage` and the compiled model, named by the weights and the macOS build. Default `~/Library/Caches/nxkit/ndve-ane`. |
| `NXKIT_NDSR=0` | Turns the native replacement of the in-game upscaler (`nn::ndsr`, packs `vr` and `vr_light`) off. The game's own dispatch runs, as it does when a network does not match (`docs/plans/ndsr-native.md`). |
| `NXKIT_NDSR_COMPARE=1` | With `NXKIT_DIAG=ndsr`: runs the game's dispatch and the native plan on a sampled frame and prints the largest difference per channel. |
| `NXKIT_SKIP_PROGRAMS=<key>,<key>` | Leaves out the dispatches whose compute program has one of these keys, and the steps of a native plan with such a kernel. Finds the pass that turns a picture black or costs GPU time. |
| `NXKIT_SKIP_FILE=<path>` | The same list in a file (comma or line separated), read again twice a second, so one run can measure one program after the other. |
| `NXKIT_AV1_FRAMES=<n>` | The frames in flight of the rav1d worker, 1 to 8 (default 2). With one frame the worker waits for each picture; with more a picture comes out on a later send. |
| `NXKIT_LANGUAGE=<tag>` | The console language of the game, for example `de` or `en-GB`. Without it the first preferred language of the Mac that the console has, else `en-US`. |
| `NXKIT_PAUSE_UNFOCUSED=1` | Reports the loss of the key focus to the game (it pauses, as on the console). By default the game keeps running when another window gets the focus. |
| `NXKIT_PIPELINE_ASYNC=0` | Builds a missing render pipeline in the encode, as before. By default it compiles on a background thread and the draw is left out until it is ready (`skip.pending`). |
| `NXKIT_PIPELINE_THREADS=<n>` | The threads that compile pipelines in the background (default half the cores, 2 to 8). They take the job that a draw of the latest frame asked for first. |
| `NXKIT_PIPELINE_QOS=utility` | Runs the compile threads (and with them the system's shader compiler service) at the utility QoS class, as before. The default is user-initiated: at utility the service ran on the efficiency cores while draws of the frame on screen waited. |
| `NXKIT_PIPELINE_BASES=1` | Compiles the unspecialized pipeline of a base that the archive lacks and specializes it per colour state (see "Archive coverage"). Off by default: in Ocarina of Time a base was as rare as its pipelines, because the vertex layout is part of it, and its compile took 0.1 to 10 s where the full compile took 1 to 6 ms. |
| `NXKIT_PULL=0` | Draws every vertex shader with its `_b` library and the stage_in vertex layout of the draw, as before vertex pulling. By default a vertex shader whose `_b.json` has a `pull` field is drawn through its `_bp` library, which reads the streams itself from a layout block at buffer 16, so its pipelines have no vertex layout and the archive's base of the pair serves them (see "Archive coverage"). With pulling off, those pairs miss the archive. |
| `NXKIT_PIPELINE_SPECIALIZE=0` | Never specializes, not even an archived base: the exact pipeline is looked up in the archive or compiled from the functions. For comparing pictures. |
| `NXKIT_PIPELINE_BASES=0` | Does not compile the unspecialized pipeline of a base that the archive lacks; every colour state of it compiles from the functions, as before (see "Archive coverage"). |
| `NXKIT_PIPELINE_CACHE=0`, `NXKIT_PIPELINE_RECORD=0`, `NXKIT_PIPELINE_PREWARM=0` | The runtime pipeline cache (docs/shader-step.md "The runtime pipeline cache"): `CACHE=0` turns all of it off (no archives written or read, no list, no start work); `RECORD=0` writes no `pipelines.seen` (the archives are still used); `PREWARM=0` makes no pipeline at the start. |
| `NXKIT_PIPELINE_CACHE_DIR=<dir>`, `NXKIT_PIPELINE_CACHE_MB=<n>`, `NXKIT_PIPELINE_CACHE_FLUSH=<s>`, `NXKIT_PIPELINE_PREWARM_WAIT=<s>` | The folder (default: the run folder if writable, else `~/Library/Caches/nxkit/<title id>/`), the size limit of the `rt-` archives (512 MiB, at most 64 files), the seconds between writes (30) and how long the start waits for the pipelines of the list (20; the rest goes on in the background). The prewarm uses `NXKIT_PIPELINE_THREADS`. |
| `NXKIT_AV1_SOFTWARE=1` | Decodes AV1 movies on the CPU with rav1d, also on a Mac with an AV1 decoder in hardware (M3 and newer). Use it to compare the two decoders. The log line `[codec] AV1: ...` names the decoder that opened. |
| `NXKIT_AUDIO_CLICKS=1` | Prints every jump at the border of two rendered audio blocks that is much larger than any step inside them (what a click sounds like), with the wall clock in ms. |
| `NXKIT_AUDIO_DEVICE_FRAMES` | The IO buffer of the audio output device in frames. Without it the device keeps its own (the log names its transport and buffer at the start). |
| `NXKIT_ARCHIVE_SHARDS=0`, `NXKIT_SHARD_LRU=<n>` | `0` ignores the pipeline shards (`pipelines/index.nxpi`) and loads `pipelines.mtl4archive`. `<n>` is the number of shards that stay open (default 32). See docs/shader-step.md "The pipeline archive as shards". |
| `NXKIT_MEM_REPORT=<s>` | Prints a `nxkit-mem` line every `<s>` seconds: `phys_footprint` split into resident, compressed and GPU driver memory (`graphics`); the guest reservation (mapped, resident, dirty, compressed, counted per page; the `by region` numbers next to them come from the region walk and overstate it); malloc, IOAccelerator, IOSurface, file-backed and other regions by VM tag; and the Metal device total split into the no-copy guest buffer (its whole length counts, touched or not, which is what the Metal HUD shows), textures, buffers and the rest, with the numbers of live libraries and pipelines, and the textures that the encoder evicted for being idle (`tex evicted`, with their MiB) and made again (`reuploaded`), and the pipeline archive files that are open (`archive shards N (M MiB, opened K)`: the single archive counts one with its whole size, with shards it is at most `NXKIT_SHARD_LRU` of them, 32 by default, and `opened` counts every open, so a closed shard that is asked again counts again). The archive reads its file into memory, so these MiB are heap. The region walk costs some ms per line (`walk` at the end). |
| `NXKIT_TEX_EVICT=<frames>` | The encoder drops the Metal copy of a texture that no frame used for this many frames (default 600, about 10 s; 0 turns it off; at least 8) when the guest copy is the current one, and uploads it again when the game uses it. A render target, a texture that a frame wrote, one that the CPU reads back, a memoryless, widened or scaled texture is never dropped. The report line counts `tex evicted` and `reuploaded`. If a game shows stale or wrong textures, try `NXKIT_TEX_EVICT=0`. |
| `NXKIT_MEM_ALLOC=<MiB>` | Prints a line `nxkit-mem-alloc: <alloc\|alloc_zeroed\|realloc> <n> MiB on thread <name>` and a backtrace for every heap allocation (or growth) of at least that many MiB, on stderr. It finds who owns a large part of the `malloc` heap of the memory report (try 32). The global allocator of the `nxkit-host` binary does it (`sys/allocspy.rs`); it costs one load and a compare per allocation when off. A release build needs symbols for readable frames. |
| `NXKIT_VERIFY_CACHE=1` | Walks every recording again without the template cache and compares the result. This is slow. The NVN front end does the same for its summaries: every submit decodes the recording again with an empty cache and compares the commands, the texture lists and the state delta with the cached summary. A difference prints a line and fails the submit. |
| `NXKIT_PACE_DIAG=1` | Prints, every 120 presents, the mean frame interval, pacing wait, CPU cost and GPU time in vsyncs, and the state of the frame step. It also prints `nxkit-pacing: diag present interval <mean> ms (sd, min, max) over 120 presents`: the time between two frames as the display showed them (`presentedTime`). On the fixed grid it is a multiple of the tick; with variable refresh a frame of 13 ms shows after about 13 ms. In the state line, `hz` is the unit of the frame step (60 for a game at 60 fps, 120 for a target of 120). It is not the refresh rate of the screen and it does not change with it. The screen is in the `nxkit-host: screen ...` line, see below. |
| `NXKIT_COPYBACK=0` | Turns off the copy-back of GPU-written textures in CPU-visible pools. |
| `NXKIT_COPYBACK_MAX_MB` | The most storage that one frame copies back. The default is 128. |

## The screen line and the judder line

The host prints one `nxkit-host: screen` line at the start, and another one each time something in it changes: the window moves to another screen, the display mode changes, `NXKIT_SET_REFRESH` switches the mode, or the measured tick rate differs from the plan.

```
nxkit-host: screen "XB273U NV" refresh 120.00 Hz (CoreGraphics mode), link 120 Hz, 2 ticks per 60 fps frame
nxkit-host: screen "XB273U NV" refresh 144.00 Hz (CoreGraphics mode), link 60 Hz, 1 tick per 60 fps frame (uneven); 144 Hz is not an even multiple of 60 fps, so frames alternate between two lengths (judder): set NXKIT_SET_REFRESH=120
nxkit-host: screen "Built-in Liquid Retina XDR Display" refresh 120.00 Hz (adaptive, highest rate), link 60 Hz, 1 tick per 60 fps frame, timed presents
```

It names the screen, the refresh rate with its source (`CoreGraphics mode`, `NSScreen`, or `adaptive, highest rate`), the rate of the display link, the ticks that one game frame stays, `(uneven)` when no whole number fits, `timed presents` when they are on, `VRR on` with the range of the link when variable refresh is on (the link rate is then the range), and any mode switch. When the link ticks at a rate more than 4% off the plan, the host prints that too and uses the measured rate.

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
| `stats` | nvn, gpu | the encode counters, the walker counters, why the render passes start, the decode time and the counters of the decode cache, and the pipeline archive coverage (`pipelines: archive hits N, misses M ...`, then one line for each cause of a miss, see below) | every 30 frames, the decode every 300 submits | low |
| `walkprof` | gpu | the walk of one submit: time in reading records, in the other records, in the detail of draws, in snapshots and in allocation (the clock reads make the walk slower), the records, draws and dispatches, the snapshots by kind (per-submit map, not copied, copied, shared with an earlier copy, fresh buffers, KiB), the heap blocks of the draws, and the banks of a draw that equal those of the draw before | first 200 submits, then every 20th | medium |
| `cmd` | rt | each command buffer call: add memory, begin, end, call, memory queries, submits, clipped memory and overflow blocks | first 2000 per call site | medium |
| `decodeprof` | nvn | where the time of a submit goes: lookups, hits, misses, call depth, records read, commands appended, and the time of the cache lookup (with the check of the references), of a miss (records, splices, end of decode) and of the append to the frame. Hits that skipped the check of their references are counted. The clocks read the time twice per step, so the numbers include that cost. | every 300 submits | low |
| `decode` | nvn | a record that the decoder could not read, with the recording and its spans | first 50 | low |
| `tex` | gpu | why a texture was not resolved or created, and the unresolved handles of each draw | first 300, and 400 distinct draws | low |
| `buf` | gpu | a vertex, index or uniform buffer outside guest memory | first 300 | low |
| `target` | gpu | render targets that cannot form a pass (not renderable, size mismatch, the same slot twice, no target), and alias conflicts | first 300, alias lines on sampled frames | low |
| `draw` | gpu | the state of each draw (programs, args, depth, raster, masks, blend, inputs, streams, uniform buffers, textures) and the attachments | sampled | medium |
| `ubo` | gpu | the uniform buffers of each draw, from guest memory now and from the snapshot of the walk, and the first floats of two vertex streams | sampled | high |
| `pixel` | gpu | nine pixels of the presented texture, and draw and texture details | every 60th present, and draws on sampled frames | medium |
| `pixelall` | gpu | a counters line per frame and the lit pixels of each present, to match frames while bisecting with `nobar` | every 10th frame or present | medium |
| `present` | gpu | the counters of the present path (ticks, asked, found, blitted) and the presented texture | every 120th tick or present | low |
| `memory` | gpu | the Metal memory of the process (with the guest buffer, textures and buffers apart), the live libraries and pipelines and the number of resident allocations | every 120 frames | low |
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
| `pipe` | gpu | render, compute and depth-stencil states that Metal did not build; every 60 frames the state of the background builds (see "Archive coverage") | first 300; one line per 60 frames | low |
| `gmem` | gpu | the size of the raw global memory table when it is built, and each pool that the write logs showed written (`docs/nxkit-gpu.md`) | each rebuild and each new pool | low |
| `overlap` | gpu | the flushes of the CPU, the dynamic pages, and per kind of buffer binding the bytes bound and the bytes changed since the last frame (`docs/plans/fence-overlap.md`) | every 120 frames | high (hashes every bound range) |
| `judder` | nvn | the histogram of the refreshes each shown frame stayed, and the mean (see above) | every 120 shown frames | low |
| `hitch` | gpu | each frame whose encode took 7 ms or more or that built pipelines or textures, with the wall clock in ms, to put next to the late frames of `NXKIT_PACE_DIAG` | each such frame | low |
| `kpass` | gpu | the render passes that could run as compute dispatches (`docs/plans/kernel-passes.md`, step 0), the runs of them, and why the others cannot | every 300 frames | low |
| `ndve` | gpu | for each distinct set of movie weights, once: the capture list of `docs/plans/ndve-native.md` section 10 (the run, and for each dispatch its key, groups, storage and uniform buffers with the offset from the blob start, push constants as words and floats, textures and images with format, size and sampler), whether the plan was built or why it was refused, and which of the game's intermediate pictures the frame reads later. Also each reason the game's chain ran instead (first 50), and the `NXKIT_NDVE_COMPARE=1` results | once per set of weights, and the compare every 120 movie frames | low (the compare reads two pictures) |
| `ndvetrace` | gpu | with the Neural Engine path on (the default), at the first NDVE dispatch and 60 dispatches later: the descriptors of the luma and chroma textures, their memory pool, whether the decoder's latest picture lies in the same pool or the same VM object (two views of one memory), 4 KiB compared, every command of this and the previous frame that writes the planes (copies, image and storage buffer binds, attachments, dirty tracking), the first 64 bytes of each plane and the fingerprints | two reports | low (copies the commands of two frames while it waits) |
| `nativeprof` | gpu | the mean GPU time of each step of a native plan (precise timestamps, one heap of a ring of 6) and the sum | every 120 frames | low (the steps run one after the other) |
| `ndsr` | gpu | for each distinct NDSR run, once: the capture list (the three SMAA dispatches and the network), whether the plan was built or why it was refused, and the `NXKIT_NDSR_COMPARE=1` results | once per run | low |
| `codecall` | video | every `nn::codec` call with its time (`codec` prints the opens and the decoder in use) | every call | low |
| `walkmiss` | gpu | with `NXKIT_WALK_CACHE=1`: the field of the walker state that made a call miss the templates of its handle | every 20000 misses | medium |
| `passmerge` | nvn, gpu | what the pass plan still leaves: render passes (with one draw, empty), neighbours with equal attachments that stayed apart and why (clear, sampled, other), pairs split by a compute or copy pass and how many of them touch none of the textures, loads that are `Load` only by the default rule, attachments no draw wrote, stores kept only by assumption, and (from the encoder) whether the first draw of such a load covers the whole target | every 30 frames (nvn line), sampled frames (cover lines) | low |
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
| `nopassmerge` | The pass plan keeps render passes with no draw and no clear, and merges only after a barrier (the behaviour before). Not wrong, only slower: it compares the frame time. |
| `noloadstore` | The pass plan does not learn dead stores of textures that start every frame with a clear. Not wrong in a game that behaves; it compares the frame time. |
| `coverload` | Opt in. A colour attachment whose first draw visibly covers the whole target (full-screen triangle, no vertex input, full viewport, no blend or test) is loaded as `DontCare`. A fragment shader that discards would show garbage. |
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
| `decode.unresolved` | decoder | A texture handle or address in a recording names no texture. It counts once per decoded recording. A recording that the cache answers does not count again. A handle of texture id 0 is an unbind (NVN reserves the first descriptors) and does not count. The examples name the handle or address. |
| `decode.bad-record` | decoder | A record could not be read. The rest of the recording is lost. |
| `tex.handle-unresolved` | encoder | A texture handle has no descriptor or no texture record. This is the case of a pool slot that points to a dropped texture. |
| `tex.format-unsupported` | encoder | A texture format or target that has no Metal equivalent. |
| `tex.create-failed` | encoder | Metal did not create a texture. |
| `tex.view-failed` | encoder | Metal did not create a view. For example, a view between a compressed and an uncompressed format, or a type that the texture cannot be seen as. |
| `upload.failed` | encoder | The deswizzle or the copy of a texture upload failed. The examples include the error of the kernel, for example "the texture is not usable by the kernel" for a Depth16 upload. |
| `upload.linear-refused` | encoder | A pitch-linear texture with layers, levels, 3D or no stride. Only one level and one layer are copied. |
| `upload.scaled-lost` | encoder | Guest data that does not reach a scaled texture: the texture has no scaling kernel (depth, stencil, compressed, 3D, shared-exponent formats) or the staging upload failed. Scaled colour textures take the staged upload and count as `upload.scaled`. |
| `upload.scaled` | encoder | Guest data that reached a scaled texture through a native-size staging texture and a GPU scale. This is not a loss and does not make the report unclean. A high count means the CPU writes a texture that the GPU renders at the scaled size, every frame. `EncodeStats.textures_kept_native` counts the textures the store keeps native for that reason. |
| `copyback.refused` | encoder | A GPU-written texture in CPU-visible memory that could not be copied back. The CPU reads stale bytes. |
| `copyback.over-budget` | encoder | A copy-back that was left out because the frame went over `NXKIT_COPYBACK_MAX_MB`. |
| `program.failed` | encoder | A program that did not load. It counts once per key. |
| `pipeline.archive-miss` | encoder | The archive of the build lacked a pipeline and Metal compiled it. The draw waited as `skip.pending`, so this is a hitch and not a loss. Without `NXKIT_DIAG=stats`, `health` or `pipe` it is only counted. With one on, the example says why (see "Archive coverage"). It counts the pipelines of a run without an archive too. |
| `pipeline.failed` | encoder | A render, compute or depth-stencil state that Metal did not build. It counts every attempt. |

The native replacement of a chain of dispatches (`ndve`, see `NXKIT_NDVE`) prints its own block after the report, with the same cadence, and when a counter shows up that the last block did not have. It is not in the table above, because the reasons live in `nxkit-nvn`, which knows no network:

| line | meaning |
|---|---|
| `ndve.native` | Frames in which the plan ran in place of the game's dispatches. |
| `ndve.fallback(reason)` | Frames in which the run was found and the game's dispatches ran. The reasons are `run` and `interleaved` (the commands around the run are not only dispatches and barriers), `binding` and `memory` (a weight buffer is not bound or lies outside guest memory), `weights` (the bytes are not the network's), `build`, `buffer`, `pipeline` and `tgmem` (the plan or its kernels could not be made), `push`, `resource` and `spec` (a texture, image or push block the plan names is not bound where the replacement expects it), `encode` (Metal refused the pass; the picture of that frame is lost). The example gives the sentence. |

The walker counts only when it reads a record. A recording that plays from the template cache does not count again. Without `NXKIT_WALK_CACHE=1` every frame counts. With `NXKIT_VERIFY_CACHE=1`, a recording is walked twice and counts twice.

To add a reason, add it to `reasons!` in `nxkit-nvn/src/diag.rs`, call `nxkit_nvn::diag::note` where the work is dropped, and add a line to the table above.

## Archive coverage

The pipeline archive of the build holds one pipeline for each shader pair, with the state that the build assumes: eight unspecialized colour attachments, no vertex layout, no primitive class, one sample, no alpha to coverage, no ICB support. At run time a pipeline is the pair and the state of the draw. An archive lookup finds only the state it holds, so a lookup with the formats and blend of a draw misses. The host therefore asks the archive for the unspecialized pipeline of the pair once (a *base*) and makes each draw's pipeline with `MTL4Compiler newRenderPipelineStateBySpecializationWithDescriptor:pipeline:`, which fills in the colour attachments and costs a fraction of a compile. Only colour attachment state can be specialized: a draw with a vertex layout, several samples, alpha to coverage or ICB support needs a base of its own, which the archive does not have. With `NXKIT_PIPELINE_BASES=1` such a base, and the base of a pair the archive lacks, is compiled once with its colour attachments unspecialized, and every colour state of it after that is a specialization too (a vertex function that writes the point size gets a base without a class, as a full compile does). A pipeline without a base is compiled from the functions; it compiles on a background thread and the draw waits (`skip.pending`).

The archive holds the bases the host asks for: the vertex shader's `_bp` library with the `entry` of its `pull` field when it has one (vertex pulling, `nxkit-gpu` `pull`), else its `_b` library with its `entry`; the pixel shader's `_b` library with its `entry`. A pulled draw has no vertex layout, so the base of its pair serves every layout. A vertex shader with stage inputs and no `pull` field is left out (`left[2]` of `plan::plan`). Archives built before pulling hold `_b` bases only for vertex shaders without inputs, and older ones `main0_z` bases: the pairs that pull now show as `archive named the pair but missed` or not at all, and the app needs a new archive. With `NXKIT_PULL=0` the host asks for `_b` with the vertex layout of the draw, which no archive holds.

`NXKIT_DIAG=pipe` prints every 60 frames how the background builds keep up:

```
DIAG pipe frame 3900: pending draws 48210 (total 214286), new jobs 85, failed 0 | queue 40, compiling 6 of 6 threads | cached 0 archive 0 specialized 70 (1.8/9.0 ms mean/max) compiled 15 (210.4/850.2 ms mean/max) | draws pulled 33012 stage_in 0 | bases from archive 31 (archive named the pair but missed 0) compiled 0 missing 2, specialized 410 refused 0
```

`pending draws` are the draws of the window that waited for their pipeline, `new jobs` the pipelines queued in it, `queue` and `compiling` the jobs now. Per kind of creation it gives the count and the mean and longest time of a job in the window: `archive` an exact archive hit, `specialized` a specialization of a base (the time includes compiling the base when it was the first of it), `compiled` a compile from the functions. `draws pulled` and `stage_in` count the vertex draws of the window prepared through the `_bp` library and with the stage_in layout (a vertex shader without inputs, one that cannot pull, or `NXKIT_PULL=0`); a draw that ICB records count once, when the ICB is built. With pulling and a current archive, `bases from archive` should cover nearly every pair. `bases` are totals: found in the archive, looked up in an archive that names the pair and missed (the archive holds other functions or another state than the host asks for), compiled, missing (not archived with compiling off, or refused), the pipelines specialized, and the refusals of Metal with the first one.

`NXKIT_DIAG=stats` prints every 30 frames:

```
nxkit-gpu: pipelines: archive hits 1840, misses 212 (89.7% covered), compiled in 3180 ms (15.0 ms each)
nxkit-gpu:   miss    150  pair archived, state differs: vertex-layout
nxkit-gpu:   miss     40  pair not archived
nxkit-gpu:   miss     22  pair archived, state differs: color-formats+blend
```

A hit is a pipeline that the archive gave, exactly or as a specialization of a base. A miss is one that Metal compiled from the functions. The causes come from asking the archive again for each miss (up to nine lookups, only with `stats`, `health` or `pipe` on):

| cause | meaning | what the build has to do |
|---|---|---|
| `pair not archived` | the archive has no pipeline with these two functions, even with the build's assumed state | put the pair in the plan. A pair whose vertex shader has stage inputs (not pull) is left out by the plan today (`left[2]` of `plan::plan`), because the vertex layout is not known at build time |
| `pair archived, state differs: <groups>` | the pair is archived, and setting these groups of the pipeline's state alone on the archived pair makes the lookup miss. The groups are `topology`, `samples`, `alpha-to-coverage`, `vertex-layout`, `color-formats`, `blend`, `write-mask`, `icb` | precompile the pair with that state, because the unspecialized descriptor does not cover it |
| `pair archived, only the whole state misses` | each group alone is found, the combination is not | precompile the combination |
| `no archive` | the app has no archive (`pso_mtl4` off, or the file is missing) | |

With shards, `pair not archived` also covers a pair that the index lacks and a shard that could not be opened (the host prints the error once).
| `mesh (not in the build's archive)` | a mesh pipeline; the build makes none | |

The example of `pipeline.archive-miss` in the health report carries the same text with the shader entries and the compile time of the pipeline.

### The runtime pipeline cache lines

At the start the host prints `nxkit-gpu: runtime pipeline cache <folder>: N archives (M MiB), K pipelines listed`, then with a list `nxkit-gpu: compiling shaders: making K pipelines on T threads`, a line `pipeline prewarm progress: done of K in S s (...)` every 2 s while it waits, and `pipeline prewarm done: K of K in S s (archive a, specialized b, compiled c, cached d, compute e, failed f, skipped g)`. Read it as: `archive` and `specialized` are lookups and specializations without a compile; `compiled` is what the archives lacked (it is zero in the second run with the same build, host, macOS and GPU, and if it is not, the archives were cut by the size limit or the capture did not take them); `failed` is a pipeline that Metal refused; `skipped` is one whose library is not in `shaders/` (a library of the host's own source, or a build that changed). `continues in the background` means the wait ended first. With `NXKIT_DIAG=pipe`, `pending draws` in the first frames is the number of draws that still found no pipeline.
