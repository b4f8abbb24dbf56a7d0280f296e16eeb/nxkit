# Diagnostics of the GPU path

The GPU path (`nxkit-rt`, `nxkit-nvn`, `nxkit-gpu`) has opt-in diagnostics for debugging a game. Environment variables turn them on. They are off by default. When they are off, each one costs one check, kept until the next change of a live setting. This page lists every category, knob and variable, and explains how to read the render health report.

The code is in `nxkit_nvn::diag` (the shared `NXKIT_DIAG` parser and the health report), `nxkit-gpu/src/diag.rs` (knobs, filters, sampled frames) and `nxkit_rt::diag!` (the `cmd` category).

## Switches

The keys `NXKIT_DIAG`, `NXKIT_DIAG_TEX`, `NXKIT_DIAG_FS`, `NXKIT_DIAG_WATCH`, `NXKIT_WATCH_TEX`, `NXKIT_WATCH_ADDR`,
`NXKIT_KNOB`, `NXKIT_PAINT_FS`, `NXKIT_SKIP_PROGRAMS`, `NXKIT_GPU_LABELS`, `NXKIT_PASS_LIST`, `NXKIT_BARPASS`, `NXKIT_TRACE_FS`,
`NXKIT_TRACE_HANDLES` and `NXKIT_PACE_DIAG` are
live (W35). You can change them while the game runs, through the control socket:

```bash
tools/nxkit-ctl keys
```

```bash
tools/nxkit-ctl set NXKIT_WATCH_TEX=123,456
```

```bash
tools/nxkit-ctl log 200
```

`keys` lists every live key with its value now. `set KEY=VALUE` and `unset KEY` change a key, and the next frame uses
the new value. A change gives every rate limit its budget again. `log` reads the last lines of the log of the game, and
`log +CURSOR` reads the lines after an earlier read. `patch` loads patch modules (see "Patch modules"). `help` lists every verb. Every other key is read at the start
only, and `set` refuses it. `NXKIT_CONTROL=0` turns the socket off.

| variable | effect |
|---|---|
| `NXKIT_DIAG` | Comma separated list of categories (see below). `all` turns on every category. |
| `NXKIT_DIAG_TEX` | Limits the texture and pass categories to one format, one size, or both, separated by `/`. The format is the Metal format name as `Debug` prints it. The size is `WxH` or `WxHxL` with the layer count. Examples: `R16Unorm/256x256`, `1600x900`, `Rg11B10Float`. Without it, every texture passes. |
| `NXKIT_DIAG_DIR` | The directory of the diagnostic files: the `nobar` file and the `img` pictures. Default `/tmp/fmap`. |
| `NXKIT_KNOB` | Comma separated list of knobs (see below). Knobs change what the encoder does. |
| `NXKIT_KNOB_FILE` | A file with the same list as `NXKIT_KNOB`. The encoder reads it again every frame, so you can change a knob while the game runs. |
| `NXKIT_PAINT_FS` | Comma separated fragment program keys that the `paint` and `paintdir` knobs paint. When it is empty, they paint every draw into one colour target. A listed program is also painted in a pass with more colour targets (a G-buffer pass). Then `paint` paints the first four slots and `paintdir` paints slot 0. The other slots keep their contents. |
| `NXKIT_DIAG_FS` | Comma separated fragment program keys. With keys, the `draw` line prints every draw of these programs on every frame (at most 200 lines per program per run). The line ends with the index of its render pass in the frame, the size of the attachments and the colour targets of its pass. For each slot it gives the format and the blend state (`off`, or `on` with the NVN source and destination RGB factors and the RGB op). The `nanscan` category counts only the passes that hold a draw of these programs. For these passes it also counts the targets before the pass and the textures that the programs sample. Without the variable, `draw` prints on sampled frames only and `nanscan` counts every pass. |
| `NXKIT_WATCH_TEX` | Comma separated texture ids. The encoder prints the passes that render into them and the copies that touch them (at most 40 each). |
| `NXKIT_DIAG_WATCH` | With `NXKIT_DIAG=tiny`: `0xADDR:LEN,0xADDR:LEN` (GPU addresses, LEN in bytes, at most 512). On each sampled frame, after the frame completed, the guest memory of each range is printed as `DIAG tiny watch 0x.. f32 [..] hex [..]`. |
| `NXKIT_BARPASS=<pass index>` | Traces the barrier plan of one pass. Every 60th frame, for that pass, it prints `DIAG barpass`: the pass kind, its commands, the barrier that the pass gets, every texture that the pass reads or writes with the stages that the planner still holds for it (`write`, `read`) and the last pass that touched it, and then every earlier pass of the frame that writes a texture that this pass reads (the writes of storage images count). Use it when a pass shows stale data. The pass index is the one of `NXKIT_DIAG=barr` and `passes`. |
| `NXKIT_TRACE_FS=<fragment key>[:slot,slot,..]` | Traces the draws of one fragment program (the shader key, as `NXKIT_DIAG_FS` names it). Every 300th draw prints `DIAG tracefs`: the stencil reference, the depth-stencil state and the scissor. For each listed fragment texture slot (every bound slot without a list) it prints the texture id, the NVN format, the size, the levels, the residency, the address, and how many of the first 4096 bytes of its guest storage are not zero. A 1x1 texture also prints its four floats. A game reuses a handle slot from draw to draw, so the texture of a slot is the one that the frame resolved last for the handle. The trace turns on the handle map that `comp`, `barr` and `nanscan` use. |
| `NXKIT_TRACE_HANDLES=<handle,..>` | Traces the resolve of bindless handles (decimal or `0x` hex). Each frame that binds a listed handle resolves it once. The first 4 resolves of each handle and then every 60th print `DIAG trace-handle`: the texture id, the description of its resource, the format of the TIC header and its view with the swizzle, the residency, the address and the flags of the texture. A game reuses a handle slot for different textures from draw to draw, so a line shows what the slot held when the frame resolved it. It does not show what each draw saw. |
| `NXKIT_WATCH_ADDR` | A GPU address in hex. The decoder prints the texture that each record word at this address resolves to (at most 30). There is no default address. |
| `NXKIT_CRASH_REPORT=0` | Turns off the crash report. By default a fatal `SIGSEGV`, `SIGBUS`, `SIGILL`, `SIGFPE`, `SIGTRAP` or `SIGABRT` prints a report to stderr. The report has the address, the thread, the registers, the frame pointer chain (guest frames as `main+0x...`, host frames with their symbol) and the guest module ranges. Then the process ends as before (status 139 for a `SIGSEGV`). |
| `<NXKIT_DIAG_DIR>/nobar` | A file with `lo hi`. The passes in `[lo, hi)` get a full barrier from all earlier work. Use it to bisect a missing barrier. It is read every frame. |

The variables `NXKIT_ANE_DUMP`, `NXKIT_ANE_STUDY`, `NXKIT_COREML_PACKAGE`, `NXKIT_NDSR_PACK` and `NXKIT_NDVE_DAT` belong to ignored tests and are not read by the host.

The behaviour switches below are not diagnostics, but they help to find the cause of a defect:

| variable | effect |
|---|---|
| `NXKIT_ENC_THREAD=1` | Encodes on a thread of its own (the `enc_thread` key of `game.json` does the same). |
| `NXKIT_ENCODE_THREADS` | The number of chunk threads of the encoder. 0, the default, picks the number from the cores. |
| `NXKIT_SCALE` | The render scale of render targets. The default is 1.0. A diagnostic only: a pass can mix scaled and native attachments (R70). |
| `NXKIT_WALK_CACHE=1` | Turns on the template cache of the walker. It is off by default, because in Breath of the Wild it costs more than it saves. |
| `NXKIT_SCREEN` | The screen the window opens on, by its index (0 is the built-in one), by `id:` and its CoreGraphics display id (`NSScreenNumber`, for example `id:2`, what a launcher passes for the screen it is on) or by a part of its name, for example `XB273U`. The refresh rate that the host uses is the one of the screen the window is on now, also after you drag it to another screen (`docs/pacing.md`). |
| `NXKIT_SET_REFRESH=120` | Switches the display of the window to this refresh rate at the start, at the size it has, and puts the old mode back at the exit (`set_refresh` of `game.json` does the same). Use an even multiple of the frame rate: 120 for a game at 60 fps. The switch is for this process only, so the system restores the mode also after a crash. |
| `NXKIT_TIMED_PRESENT=1` or `0` | Forces timed presents (`presentAfterMinimumDuration:`) on or off. They are off by default: on a ProMotion screen they let macOS lower the panel rate, and frames waited several vsyncs for the screen. |
| `NXKIT_VRR=0` | Turns off variable refresh (VRR) and keeps the fixed grid. By default an adaptive screen with a variable frame step (a target above 60 with a state page) gets a range for the display link. It then shows each frame as soon as it is ready (see `docs/pacing.md`). The screen line says `VRR on`. |
| `NXKIT_VRR_MIN` | The lowest rate of the VRR range of the link in Hz, the floor the game needs. The default is 60. |
| `NXKIT_GPU_LABELS=1` | Names the encoder of each pass (pass index, first attachment, draws) for the Metal debugger. Instruments does not show the labels of Metal 4 encoders. |
| `NXKIT_PASS_LIST=1` | With `NXKIT_DIAG=passes`: also prints every pass of the frame with its attachments and their load and store actions. |
| `NXKIT_OVERLAP=1` | The CPU runs one frame ahead of the GPU: `nvnSyncWait` for the newest frame waits for the one before it, and every bound uniform buffer reads a copy (`docs/plans/fence-overlap.md`). |
| `NXKIT_PRIVATE=track` | Tracks which guest pages the game's CPU writes. It also tracks which pages the CPU touches while a frame that may write them runs on the GPU (step 1 of the private GPU memory, `docs/nxkit-gpu.md`, "Private GPU memory, step 1: tracking"). Diagnostic only: the picture, the bindings and the waits do not change. The pages that a frame may write on the GPU become tracked. These are storage bindings a shader may write, destinations of buffer copies and fills, and pools that shaders wrote through raw pointers. Tracked pages are read only, so a CPU write faults once per frame and the page is opened. The pages the newest frame may write have no access until its batch completes. A CPU access then is a read or a write in flight, and the host counts it with its program counter. Prints `DIAG private` every 300 frames. Costs a page fault per page the game writes, and a run with it is slower. Unset: no handler work and no protection. |
| `NXKIT_PRIVATE=1` | The GPU works on a private mirror of the guest pages that frames write (steps 2 and 3 of the private GPU memory, `docs/nxkit-gpu.md`, "Private GPU memory, step 2: the mirror" and "step 3: fences at the flush"). A wait for a fence returns when the flush of that frame has run (its inputs are in the staging buffer, the pages it may write have no access). The frame 3 before it bounds the wait. `nvnQueueFinish`, the frames after a copy-back and the knob `privhonest` keep the full wait. The overlap and the renamer are off. The walk copies the bound uniform and read-only storage ranges that are not private, as with the overlap. The host copies the pages that a frame may write, and the pages of every binding that touches them, into a host reservation (2 MiB segments, one no-copy buffer per run) and binds them there. The flush stages the pages the CPU wrote, and a compute pass at the start of the frame copies them into the mirror. The CPU's first access to a page that a frame wrote waits for that frame's batch, then copies the page from the mirror. Prints `DIAG private mirror`, `fences` and the lines for the waits every 300 frames with `NXKIT_DIAG=private`. Costs a page fault per page the game writes and about 40 to 130 MiB of mirror in OoT. Other values than `1` and `track` leave it off. |
| `NXKIT_NATIVE_CACHE=<n>` | How many plans of the native replacements (`nn::ndve`, `nn::ndsr`) the cache keeps (default 4, the least recently used goes first). A plan holds tens of MB of scratch. |
| `NXKIT_NDVE=0` | Turns the native replacement of the movie network (`nn::ndve`) off. The game's own chain of compute dispatches runs, as it does when the weights or the bindings do not match (`docs/plans/ndve-impl.md`). |
| `NXKIT_NDVE_COMPARE=1` | With `NXKIT_DIAG=ndve`: on one movie frame in 120 (the 10th, the 130th and so on), the game's chain and the native plan both run. The plan writes copies of its pictures. Four frames later the host reads back the output of the plan and the output of the game. The line `DIAG ndve compare` gives the largest difference in each channel and the number of differing pixels. The game's chain is noisy (a race between lanes), so judge by the largest difference. |
| `NXKIT_NDVE_ANE=0` | Keeps NDVE on the GPU. By default the movie network runs on the Neural Engine, made at decode time ahead of the game (`docs/plans/ndve-ane.md`). The Metal plan is the fallback for every picture without a result in time. |
| `NXKIT_NDVE_ANE_CACHE=<dir>` | The folder of the Neural Engine model: the `.mlpackage` and the compiled model, named by the weights and the macOS build. Default `~/Library/Caches/nxkit/ndve-ane`. |
| `NXKIT_NDSR=0` | Turns the native replacement of the in-game upscaler (`nn::ndsr`, packs `vr` and `vr_light`) off. The game's own dispatch runs, as it does when a network does not match (`docs/plans/ndsr-native.md`). |
| `NXKIT_NDSR_COMPARE=1` | With `NXKIT_DIAG=ndsr`: runs the game's dispatch and the native plan on a sampled frame and prints the largest difference per channel. |
| `NXKIT_TEMPORAL_UPSCALE=0` | Turns the temporal upscale of `temporal_upscale` in `game.json` off (R72, `docs/nxkit-gpu.md`, "Temporal upscale"). The game's own temporal anti-aliasing and upscale draws run. The log line `nxkit-gpu: temporal upscale: off, NXKIT_TEMPORAL_UPSCALE=0` confirms it. |
| `NXKIT_SKIP_PROGRAMS=<key>,<key>` | Leaves out the dispatches whose compute program has one of these keys and the steps of a native plan with such a kernel. It also leaves out the draws whose vertex, task, mesh or fragment program has one (`skip.listed`). While a list is set, the encoder does not replay recorded runs of draws. Finds the pass that turns a picture black or costs GPU time. |
| `NXKIT_SKIP_FILE=<path>` | The same list (draws too) in a file (comma or line separated), read again twice a second, so one run can measure one program after the other. |
| `NXKIT_INPUT_FILE=<path>` | Scripted buttons for player 1, for a run that nobody attends. The host reads the file again twice a second (a missing or empty file means no scripted input). Entries are comma or line separated, and `#` starts a comment. `hold <action>` holds the action while the line is in the file. `pulse <action> <on_ms> <period_ms>` holds it for `on_ms` at the start of every `period_ms`. The periods start at the moment the host first read the content with the line, so the first press comes at once. An action is a button or stick direction of the key layout (`a`, `b`, `x`, `y`, `l`, `r`, `zl`, `zr`, `plus`, `minus`, `lstick`, `rstick`, `dpad_up`, `dpad_down`, `dpad_left`, `dpad_right`, `ls_up`, `ls_down`, `ls_left`, `ls_right`, `rs_up`, `rs_down`, `rs_left`, `rs_right`). The actions join those of the keyboard before the sticks are worked out, so opposite directions cancel. They work without a controller, with the keyboard off and without the key focus. A line that does not parse is skipped, with one message per distinct text. When the content changes, the host prints `nxkit-input: scripted input: <entries or none>`. Example: `pulse a 100 1500, hold ls_up` presses A for 100 ms every 1.5 s and holds the left stick up. A file with `wait`, `press`, `release`, `stick` or a `hold` of several buttons is a timeline, and so is a recording of the `record` verb (see "Input and replay"). |
| `NXKIT_AV1_FRAMES=<n>` | The frames in flight of the rav1d worker, 1 to 8 (default 2). With one frame the worker waits for each picture. With more, a picture comes out on a later send. |
| `NXKIT_LANGUAGE=<tag>` | The console language of the game, for example `de` or `en-GB`. Without it the first preferred language of the Mac that the console has, else `en-US`. |
| `NXKIT_OP_MODE=docked\|handheld` | The operation mode that the game sees, over the `op_mode` of the title module. A launch option (`docs/nxkit-host.md`, launcher.json). Another value is ignored. |
| `NXKIT_RUMBLE=0`, `NXKIT_RUMBLE_GAIN=<0..3>`, `NXKIT_RUMBLE_MIN=<0..0.9>` | `NXKIT_RUMBLE=0`: no rumble. `NXKIT_RUMBLE_GAIN`: the factor on the rumble of the game (1.0). `NXKIT_RUMBLE_MIN`: the rumble below which a controller does not move (0). A value outside the range is ignored. Launch options. |
| `NXKIT_HD_RUMBLE=0` | A Switch Pro Controller rumbles through Core Haptics like any other controller, not with HD Rumble (decisions R74). A launch option. The log says once per controller how it rumbles: `nxkit-input: rumble: <name>: haptic localities ...` for Core Haptics, `HD Rumble over Usb` or `Bluetooth`, or why HD Rumble is off. |
| `NXKIT_FPS_TARGET=30\|60\|120` | The frame rate over the `fps_target` of the title module. Only a rate up to `fps_max` (or the title's own) of a title whose `fps_target` is not 0 is taken. Another value is refused with a line in the log and the title's stays. A launch option. |
| `NXKIT_STICK_DEADZONE_L=<0..50>`, `NXKIT_STICK_DEADZONE_R=<0..50>` | The radial dead zone of the left and the right stick of each controller, in whole percent of the travel (0: none). Inside it the stick reads centred, outside it the rest of the travel is spread over the full range (`stick_deadzone.h` of the old engine). The keyboard and the mouse are not affected. Another value is 0. Launch options. |
| `NXKIT_PAD_MERGE=1` | Every controller is player 1: no other player shows up, the controllers are merged as in a one-player game and all of them rumble with player 1. A launch option. |
| `NXKIT_PAD_PRESS_TO_JOIN=1` | A new controller gets no player until a button of it is down, then the lowest free player, whatever player it had before. `NXKIT_PAD_MERGE=1` wins. A launch option. |
| `NXKIT_PAD_MAP=<button>=<button>,…`, `NXKIT_KEYMAP=<function>=<key code>,…` | What the buttons of a controller send, and the functions on other keys, in the format of `DM_PAD_MAP` and `DM_KEYMAP` of the old engine (`docs/nxkit-host.md`). A value with one malformed entry is ignored whole, with one line in the log. Set by the launcher, no launch option. |
| `NXKIT_SAVE=<folder>` | The save folder of user 1, as `DM_SAVE` of the old engine: the folder that is the committed save. Its siblings hold the work folder, the device save and the other users' saves. Without it the save is `~/Library/Application Support/nxkit/<title id>/save` (`docs/nxkit-host.md`, Saves). The log says `nxkit-host: save <folder>` at every start. Set by the launcher, no launch option. |
| `NXKIT_USER=<1..8>` | The user the game starts with, as the preselected user. A user that is not in the registry `<save>.users` is added at the start, with a save of its own, `<save>_user<n>` (`docs/nxkit-host.md`, Users). Another value is user 1. The log says `nxkit-svc-system: user <n> added` once. Set by the launcher, no launch option. |
| `NXKIT_NICK=<name>` | The nickname of user 1, as `DM_NICK` of the old engine (`Player`). The other users are `Player <n>`. Set by the launcher, no launch option. |
| `NXKIT_SAVE_SYNC=<folder>\|icloud\|0`, `NXKIT_SAVE_SYNC_CHOICE=<choice>` | The cloud saves folder and the player's choice when both saves changed, as `DM_SAVE_SYNC` and `DM_SAVE_SYNC_CHOICE` of the old engine (`docs/nxkit-host.md`, Cloud saves). The log lines start with `nxkit-save-sync:`: `on: <file>` at the first mount, then what the start did, `sent: ...` after a send, and `warning:` for what went wrong. A choice that is not one is ignored with one line. Set by the launcher, no launch option. |
| `NXKIT_METAL_HUD=1` | The Metal performance HUD over the game. A launch option. The host sets `MTL_HUD_ENABLED=1` in its own environment before it makes a Metal device and sets `developerHUDProperties` of the window's `CAMetalLayer` (`mode` `default`). The layer property alone shows no HUD (tested 2026-11-22 on macOS 27.2). |
| `NXKIT_PRESENT_UPSCALE=metalfx\|off` | How the present scales a picture that is smaller than the window: `metalfx` (or `1`) through MetalFX's spatial scaler, `off` (the default) by nearest pixel. A launch option (R70). The log has a line `nxkit-metal: present upscaling: ...` with the mode, each new scaler and each size that MetalFX refuses. |
| `NXKIT_SAVED_CHOICES=0` | The host ignores the saved choices of the launch options in the preferences of the app (`launcher.<env>`). |
| `NXKIT_FULLSCREEN=1` | The window starts in full screen, as `--fullscreen`. Any value that is not blank and not `0` turns it on. For a launcher that opens the app bundle, which starts the host with no arguments (`docs/nxkit-host.md`). |
| `NXKIT_OVERLAY=1` | The game opens over the launcher's full-screen Space instead of a Space of its own: a borderless window on every Space. It fades in with the game's first picture and fades out when the game ends. No loading window shows, and the launcher gets the progress (`org.nxkit.loading.<pid>`) and the first picture (`org.nxkit.shown.<pid>`). NX2Apple's console mode sets it. |
| `NXKIT_CURSOR_HIDE` | The seconds without movement after which the pointer over the game hides (3). `0` never hides it. |
| `NXKIT_CURSOR_HIDDEN=1` | The pointer starts hidden: the launcher's own pointer was hidden when it started the game. |
| `NXKIT_LAUNCHER_PID` | The process of the launcher for Back to the launcher. It wins over `NXKIT_LAUNCHER_BUNDLE_ID`, so the game goes back to the copy that started it when two run. |
| `NXKIT_HOME_MENU=0` | No Home menu on a short press of a controller's Home button (`docs/nxkit-host.md`, The Home button). |
| `NXKIT_HOME_QUIT=0` | No quit prompt when Home is held for 2 seconds. |
| `NXKIT_LAUNCHER_BUNDLE_ID` | The bundle identifier of the launcher that started the game. The Home menu gets Back to the launcher, which pauses the game in the background and activates that application. |
| `NXKIT_HOME_TEXT` | The texts of the Home menu and the quit prompt in the player's language, one per line: hold, title, note, quit, cancel, resume, back to the launcher, menu title. |
| `NXKIT_PAUSE_UNFOCUSED=1` | Reports the loss of the key focus to the game (it pauses, as on the console). By default the game keeps running when another window gets the focus. |
| `NXKIT_PIPELINE_ASYNC=0` | Builds a missing render pipeline in the encode, as before. By default it compiles on a background thread and the draw is left out until it is ready (`skip.pending`). |
| `NXKIT_PIPELINE_THREADS=<n>` | The threads that compile pipelines in the background (default half the cores, 2 to 8). They take the job that a draw of the latest frame asked for first. |
| `NXKIT_PIPELINE_QOS=utility` | Runs the compile threads (and with them the system's shader compiler service) at the utility QoS class, as before. The default is user-initiated: at utility the service ran on the efficiency cores while draws of the frame on screen waited. |
| `NXKIT_PIPELINE_BASES=1` | Compiles the unspecialized pipeline of a base that the archive lacks and specializes it per colour state (see "Archive coverage"). Off by default: in Ocarina of Time a base was as rare as its pipelines, because the vertex layout is part of it. Its compile took 0.1 to 10 s where the full compile took 1 to 6 ms. |
| `NXKIT_PULL=1` | Draws a vertex shader whose `_b.json` has a `pull` field through its `_bp` library (vertex pulling). This library reads the streams itself from a layout block at buffer 16. So its pipelines have no vertex layout, and the archive's base of the pair serves them (see "Archive coverage"). **Off by default until it is verified:** in Ocarina of Time's title the presents went from a steady 16.67 ms to a mean of 55 to 110 ms, with stalls of up to 1.9 s. The GPU was at its lowest clock, and no command buffer failed. Without it every vertex shader draws with its `_b` library and the stage_in vertex layout of the draw, and those pairs miss the archive (they compile from the functions). `NXKIT_DIAG=pull` checks the reads of the pulled draws. |
| `NXKIT_PULL=0` | The same as unset: every vertex shader draws with its `_b` library and the stage_in vertex layout of the draw. Only `1` turns vertex pulling on (`pull::enabled`). |
| `NXKIT_RENDERCOND=3d` | A false render condition (`SetRenderEnable`, `SetRenderEnableConditional`) leaves out draws and clears only. By default it also leaves out dispatches, copies and downsamples. See `crates/nxkit-nvn/src/rendercond.rs`. |
| `NXKIT_COUNTERS=0` | Does not emulate the GPU counters (samples passed, vertices, primitives): every `ReportCounter` of such a counter is unknown again, a render condition on it renders, and nothing is written to its report. By default the backend counts samples with Metal visibility results, and vertices and primitives from the draw arguments. It writes the reports when the GPU finished the frame (see docs/nxkit-gpu.md, "GPU counters and render conditions"). |
| `NXKIT_RENDERCOND_STALE=0`, `=addr` | `0`: a render condition reads an emulated counter only when the report it names landed. Otherwise it renders. By default it falls back to the last counter that landed at the address (a report of an earlier frame, one frame of latency). Then it falls back to the last one that landed at the same place in an earlier frame: the same queue and the same number among its reports in the frame. This is for a game that moves its query memory every frame. `addr` keeps the address fallback only. |
| `NXKIT_QUEUE_ORDER=0` | Holds no submit for a wait on another queue: every queue goes into the frame in the order of its submits, as before. By default the host holds a submit whose recording waits for an event (`nvnCommandBufferWaitEvent`) or a sync (`nvnCommandBufferWaitSync`, `nvnQueueWaitSync`) that is not signalled yet. It holds the later work of its queue too, until the work that signals it is submitted. A flush runs what is still held (see `crates/nxkit-rt/src/nvn/ctx/gate.rs`). With the variable set, `NXKIT_DIAG=queues` still counts the waits that would have held. |
| `NXKIT_RELEASE_FLUSH=0` | Turns off the flush after a release. By default a CPU event signal (`nvnEventSignal`) can release held submits with at least `NXKIT_RELEASE_FLUSH_MIN` (default 64) draws, dispatches and copies. Such a signal flushes the frame at once (`FlushReason::Release`), so the encode of the frame's main work starts before the present. The flush runs no held op. `NXKIT_DIAG=frame` and `gateseq` count the flushes by reason (`docs/nxkit-gpu.md`, frame steps). |
| `NXKIT_STREAM_COMMIT=0` | Commits a frame as one group again. By default the encode of a frame with at least 8 passes puts a small lead chunk (about one eighth of the weight) first. It commits the first command buffer (uploads) with the lead chunk as soon as both are encoded, while the other chunks encode (`Batch::stream_prefix`). Not used with `NXKIT_PRIVATE=1`. |
| `NXKIT_PRIVATE_READOPEN=0` | With `NXKIT_PRIVATE=1` and late results, the flush protects a page that a frame writes `PROT_NONE` again also when its guest bytes are a completed frame's. By default such a page stays readable and a read of it takes no trap (`docs/nxkit-gpu.md`, "Private GPU memory, step 8"). |
| `NXKIT_REC_LOCK=0` | `nvnCommandBufferEndRecording` leaves the drop of the cache summaries of its memory to the next lock of the front end (the spans go into a list) instead of taking the lock. Measured slower in Ocarina of Time, so off by default. |
| `NXKIT_PIPELINE_SPECIALIZE=0` | Never specializes, not even an archived base: the exact pipeline is looked up in the archive or compiled from the functions. For comparing pictures. |
| `NXKIT_PIPELINE_BASES=0` | Does not compile the unspecialized pipeline of a base that the archive lacks. Every colour state of it compiles from the functions, as before (see "Archive coverage"). |
| `NXKIT_PIPELINE_CACHE=0`, `NXKIT_PIPELINE_RECORD=0`, `NXKIT_PIPELINE_PREWARM=0` | The runtime pipeline cache (docs/shader-step.md "The runtime pipeline cache"): `CACHE=0` turns all of it off (no archives written or read, no list, no start work). `RECORD=0` writes no `pipelines.seen`, but the host still uses the archives. `PREWARM=0` makes no pipeline at the start. |
| `NXKIT_PIPELINE_CACHE_DIR=<dir>`, `NXKIT_PIPELINE_CACHE_MB=<n>`, `NXKIT_PIPELINE_CACHE_FLUSH=<s>`, `NXKIT_PIPELINE_PREWARM_WAIT=<s>` | The folder (default: the run folder if writable, else `~/Library/Caches/nxkit/<title id>/`). The size limit of the `rt-` archives (512 MiB, at most 64 files). The seconds between writes (30). How long the start waits for the pipelines of the list (20, and the rest goes on in the background). The prewarm uses `NXKIT_PIPELINE_THREADS`. `NXKIT_PIPELINE_CACHE_ROTATE=<n>` (1000) hands the capture to a new serializer after `n` pipelines. `NXKIT_PIPELINE_CACHE=0` captures nothing at all. |
| `NXKIT_AV1_SOFTWARE=1` | Decodes AV1 movies on the CPU with rav1d, also on a Mac with an AV1 decoder in hardware (M3 and newer). Use it to compare the two decoders. The log line `[codec] AV1: ...` names the decoder that opened. |
| `NXKIT_AUDIO_CLICKS=1` | Prints every jump at the border of two rendered audio blocks that is much larger than any step inside them (what a click sounds like), with the wall clock in ms. |
| `NXKIT_AUDIO_DEVICE_FRAMES` | The IO buffer of the audio output device in frames. Without it the device keeps its own (the log names its transport and buffer at the start). |
| `NXKIT_ARCHIVE_SHARDS=0`, `NXKIT_SHARD_LRU=<n>` | `0` ignores the pipeline shards (`pipelines/index.nxpi`) and loads `pipelines.mtl4archive`. `<n>` is the number of shards that stay open (default 32). See docs/shader-step.md "The pipeline archive as shards". |
| `NXKIT_MEM_REPORT=<s>` | Prints a `nxkit-mem` line every `<s>` seconds. The line has `phys_footprint` split into resident, compressed and GPU driver memory (`graphics`). It has the guest reservation (mapped, resident, dirty, compressed, counted per page). The `by region` numbers next to them come from the region walk and overstate it. It has malloc, IOAccelerator, IOSurface, file-backed and other regions by VM tag. It has the Metal device total, split into the no-copy guest buffer, textures, buffers and the rest. The whole length of the guest buffer counts, touched or not, which is what the Metal HUD shows. With the total come the numbers of live libraries and pipelines. Then come the textures that the encoder evicted for being idle (`tex evicted`, with their MiB) and made again (`reuploaded`). Last come the pipeline archive files that are open (`archive shards N (M MiB, opened K)`). The single archive counts one with its whole size. With shards, it is at most `NXKIT_SHARD_LRU` of them, 32 by default. `opened` counts every open, so a closed shard that is asked again counts again. The archive reads its file into memory, so these MiB are heap. The guest part has `wired` too. While the buffer is resident, the driver wires all of it. The region walk costs some ms per line (`walk` at the end). A second line, `nxkit-mem paging`, gives per second the task's faults, page-ins, decompressions and swap-ins. It also gives the system's free, wired and compressor sizes, its `memory_pressure` level and its decompressions, compressions, page-ins, swap-ins and swap-outs. A third, `nxkit-mem regions`, is the region use that the host also prints at the end of a run. A fourth, `nxkit-mem pools`, is the NVN memory pools that are live now. Its first fields are `n`, the pools `made` and `gone` since the start, `bytes` (the sum of the lengths) and `pages` (the union of the pools, each rounded out to 16 KiB pages, overlaps counted once). Then come `unaligned` (pools whose start or end is not on a 16 KiB page), `largest`, and `gpu` and `cpu-only` (bytes in pools without and with `GPU_NO_ACCESS`). Last come `heap`, `alias` and `other` (`pages` split by region) and `sizes` (counts up to 64 KiB, 1 MiB, 16 MiB, 256 MiB, and larger). All sizes are MiB. `pages` is how much of the guest range the GPU must reach if there were one no-copy buffer for each pool. The host prints it at the end of a run too. Then come the `nxkit-mem tex` lines: the inventory of the Metal textures of the texture store. `nxkit-gpu` takes it in the idle scan every 60 frames while the report is on. So the lines appear after the 60th frame and are up to a second old. The end of a run prints the last one. All sizes are MiB, counts come before the slash. `tex store` has the store's Metal bytes against `process`, which is all textures of the process. The difference is staging and native textures outside the store. `tex store` also has count/MiB of each class. Each Metal texture is in the first class that fits: `memoryless`, `attachment` (a render target at any time), `gpu-written` (a frame wrote it, also image atomics), `cpu-read`, `scaled`, `widened`, `sampled` (a plain uploaded texture that the idle rule may drop). Then come the block `shadows`, the `retired` textures (replaced or evicted, held until the GPU is done) and the number of texture `views` (they share storage). `tex guest` gives for `sampled` and `widened` the guest memory size against the Metal bytes and the quotient `x` (the expansion). `tex formats` lists the 8 formats with the most bytes and sums the rest as `other`. `tex idle` splits the `sampled` textures by frames since their last use: `<=60` is in use, `<=600` is not yet dropped by the default rule, `older` is not dropped although it is idle. `tex scale` has the scaled textures and the bytes they would have at scale 1 (bytes / scale squared). `tex dup` counts the entries (texture objects of the game) that have the same guest start and description as another live entry (`same-range`). It also counts those whose guest range overlaps another live entry's range (`overlap`). Each count has the number of Metal textures behind it and their MiB. The line shows whether the game makes many texture objects over the same memory. `tex largest` lists the 8 largest Metal textures as `format WxHxdepth-or-layers mMips sScale class MiB idle frames`. |
| `NXKIT_TEX_EVICT=<frames>` | The encoder drops the Metal copy of a texture that no frame used for this many frames, when the guest copy is the current one. The default is 600, about 10 s. 0 turns it off, and other values are at least 8. The encoder uploads the texture again when the game uses it. A render target, a texture that a frame wrote, one that the CPU reads back, a memoryless, widened or scaled texture is never dropped. The report line counts `tex evicted` and `reuploaded`. If a game shows stale or wrong textures, try `NXKIT_TEX_EVICT=0`. |
| `NXKIT_TEX_INPLACE=0` | Pitch-linear textures get a copy instead of lying over their guest bytes (`docs/nxkit-gpu.md`, "Pitch-linear textures over guest memory"). The encoder uploads the copy in every frame that uses it, unless the GPU wrote it last. By default such a texture is a Metal texture over the no-copy buffer of its pool, so a CPU write into the pool reaches it without an upload. |
| `NXKIT_TEX_SUPERSEDE=0` | Turns off the supersede rule of the texture store. By default the encoder drops the Metal copy of a texture when two conditions are true. First, a frame wrote another texture over the same guest memory after the last use of the first one. Second, the first one was idle for at least 8 frames. A texture with several images (mip levels, layers, a cube, a depth) stays when the other texture lies inside its range or covers it, because the write may have touched only one image. A texture that the CPU reads back and a memoryless texture stay. The report line `tex attach` counts `superseded` and `remade`. When an id was remade, the line `tex remade` follows: the total, and the 8 shapes (format and size of the new texture) with the most remakes, each as `count/gpu-newer`. `gpu-newer` counts the remakes whose newest content was only on the GPU, so the new texture got no upload and the content is lost. The counts are cumulative. `NXKIT_DIAG=life` prints a `DIAG life superseded` line for each one. If a game shows stale render targets, try `NXKIT_TEX_SUPERSEDE=0` or the knob `nosupersede`. |
| `NXKIT_SHARD_TRIM=0` | Keeps the open pipeline shards after the prewarm. By default the last prewarm thread closes them (`nxkit-gpu: prewarm over, N archive shards closed`). A later draw opens its shard again. |
| `NXKIT_GUEST_KEEP=0` | Stops pinning the GPU buffer's range in the kernel: releases replace the pages again and `MapMemory` shares them, which leaves the GPU on stale pages (docs/nxkit-host.md, "What the GPU buffer costs"). For comparisons only. |
| `NXKIT_GUEST_WIRE=1` | Wires the GPU buffer's range with `mlock` at start and prints the MiB and the time it took. |
| `NXKIT_MEM_RESERVE=<size>`, `NXKIT_MEM_ALIAS=<size>` | Replace `memory.reserve` and `memory.alias` of `game.json` for one run (bytes, `0x` hex, `M` or `G`). The heap region takes what is left. Use the `region use` line of an earlier run to pick them. |
| `NXKIT_UE_HEAP=<size>` | The arena of the Unreal engine heap at the end of the alias region (bytes, `0x` hex, `M` or `G`), in place of 40 % of it. Only for a main whose fixups name Unreal's FMallocBinned3. `nxkit-ue` reads it (`UeSettings`). The engine region gets what the alias region has room for besides it. |
| `NXKIT_STALL_DUMP=<seconds>` | When no present of an NVN game went on screen for that long, prints `nxkit-host: stall: no present for <ms> ms` and one line per guest thread (`Kernel::thread_report`). The line has its name, priority, state and whether it waits in a lock. For a thread in a svc, it also has the guest pc, lr and frame chain. Once per stall. |
| `NXKIT_MEM_ALLOC=<MiB>` | Prints a line `nxkit-mem-alloc: <alloc\|alloc_zeroed\|realloc> <n> MiB on thread <name>` and a backtrace for every heap allocation (or growth) of at least that many MiB, on stderr. It finds who owns a large part of the `malloc` heap of the memory report (try 32). The global allocator of the `nxkit-host` binary does it (`sys/allocspy.rs`). When it is off, it costs one load and a compare per allocation. A release build needs symbols for readable frames. |
| `NXKIT_VERIFY_CACHE=1` | Walks every recording again without the template cache and compares the result. This is slow. The NVN front end does the same for its summaries. Every submit decodes the recording again with an empty cache. It compares the commands, the texture lists and the state delta with the cached summary. A difference prints a line and fails the submit. |
| `NXKIT_PACE_DIAG=1` | Prints, every 120 presents, the mean frame interval, pacing wait, CPU cost and GPU time in vsyncs, and the state of the frame step. It also prints `nxkit-pacing: diag present interval <mean> ms (sd, min, max) over 120 presents`: the time between two frames as the display showed them (`presentedTime`). On the fixed grid it is a multiple of the tick. With variable refresh a frame of 13 ms shows after about 13 ms. In the state line, `hz` is the unit of the frame step (60 for a game at 60 fps, 120 for a target of 120). It is not the refresh rate of the screen and it does not change with it. The screen is in the `nxkit-host: screen ...` line, see below. |
| `NXKIT_COPYBACK=0` | Turns off the copy-back of GPU-written textures in CPU-visible pools. |
| `NXKIT_COPYBACK_MAX_MB` | The most storage that one frame copies back. The default is 128. |

## The screen line and the judder line

The host prints one `nxkit-host: screen` line at the start, and another one each time something in it changes. This happens when the window moves to another screen, when the display mode changes, when `NXKIT_SET_REFRESH` switches the mode, or when the measured tick rate differs from the plan.

```
nxkit-host: screen "XB273U NV" refresh 120.00 Hz (CoreGraphics mode), link 120 Hz, 2 ticks per 60 fps frame
nxkit-host: screen "XB273U NV" refresh 144.00 Hz (CoreGraphics mode), link 60 Hz, 1 tick per 60 fps frame (uneven); 144 Hz is not an even multiple of 60 fps, so frames alternate between two lengths (judder): set NXKIT_SET_REFRESH=120
nxkit-host: screen "Built-in Liquid Retina XDR Display" refresh 120.00 Hz (adaptive, highest rate), link 60 Hz, 1 tick per 60 fps frame, timed presents
```

It names the screen, the refresh rate with its source (`CoreGraphics mode`, `NSScreen`, or `adaptive, highest rate`), the rate of the display link and the ticks that one game frame stays. It shows `(uneven)` when no whole number fits and `timed presents` when they are on. It shows `VRR on` with the range of the link when variable refresh is on (the link rate is then the range). It also shows any mode switch. When the link ticks at a rate more than 4% off the plan, the host prints that too and uses the measured rate.

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

The column "takes effect" says when a change of `NXKIT_DIAG` works. `live` means that the next frame uses it (W35). `start` means that the host decides at the start of the game, so you must start the game with the category. The categories `frame` and `gateseq` are the only ones that are start only. Both need the hooks for the batches of the GPU, which the host builds once. Every other category reads `NXKIT_DIAG` where it prints, or keeps the answer in a `Memo` that the next change of a live setting resets.

The cost column shows the cost when the category is on: low, medium (extra work per draw or texture), or high (reads guest memory or the GPU every frame).

| category | crate | takes effect | prints | limit | cost |
|---|---|---|---|---|---|
| `health` | nvn, gpu | live | the render health report (see below) | every 300 frames, and when a reason counts for the first time | low |
| `stats` | nvn, gpu | live | the encode counters, the walker counters, why the render passes start, the decode time and the counters of the decode cache, and the pipeline archive coverage (`pipelines: archive hits N, misses M ...`, then one line for each cause of a miss, see below) | every 30 frames, the decode every 300 submits | low |
| `walkprof` | gpu | live | the walk of one submit. It gives the time in reading records, in the other records, in the detail of draws, in snapshots and in allocation (the clock reads make the walk slower). It gives the records, draws and dispatches, and the snapshots by kind (per-submit map, not copied, copied, shared with an earlier copy, fresh buffers, KiB). It also gives the heap blocks of the draws, and the banks of a draw that equal those of the draw before | first 200 submits, then every 20th | medium |
| `cmd` | rt | live | each command buffer call: add memory, begin, end, call, memory queries, submits, clipped memory and overflow blocks | first 2000 per call site | medium |
| `decodeprof` | nvn | live | where the time of a submit goes: lookups, hits, misses, call depth, records read and commands appended. It also gives the time of the cache lookup (with the check of the references), of a miss (records, splices, end of decode) and of the append to the frame. Hits that skipped the check of their references are counted. The clocks read the time twice per step, so the numbers include that cost. | every 300 submits | low |
| `decode` | nvn | live | a record that the decoder could not read, with the recording and its spans | first 50 | low |
| `tex` | gpu | live | why a texture was not resolved or created, and the unresolved handles of each draw | first 300, and 400 distinct draws | low |
| `buf` | gpu | live | a vertex, index or uniform buffer outside guest memory | first 300 | low |
| `gputs` | gpu, rt | live | the times of the timestamp reports (`docs/nxkit-gpu.md`, "The time of a report"). For each report written: `DIAG gputs report addr <hex> kind <n> fence <n> start/end <start\|end> ns <n> delta-to-previous-on-queue <n> source <batch\|kept\|clamped>`. `source` is `batch` for the times of the batch and `kept` for a batch without GPU time (the last time written). It is `clamped` for times raised to the last time written, so they do not go back. `delta` is the difference in ns to the report written before it (it can be negative inside a frame, where start and end alternate). For each call of `nvnDeviceGetTimestampInNanoseconds`: `DIAG gputs read addr <hex> ticks <n> ns <n> delta-to-previous-read <n>` (ns). A game that divides by the difference of two reports should see no 0 and no huge value here | first 200 of each, then every 600th | low |
| `pull` | gpu | live | with `NXKIT_PULL=1`: a pulled draw whose layout block makes `dm_fetch` read outside guest memory (`stray read`). The line gives the location, the address and bytes, the guest range, the vertex and instance range and the streams. It checks arrays draws and indexed draws (it reads the indices), but not indirect draws | first 32 | medium |
| `target` | gpu | live | render targets that cannot form a pass (not renderable, size mismatch, the same slot twice, no target), and alias conflicts | first 300, alias lines on sampled frames | low |
| `draw` | gpu | live | the state of each draw (programs, args, depth, raster, masks, blend, inputs, streams, uniform buffers, textures) and the attachments | sampled | medium |
| `ubo` | gpu | live | the uniform buffers of each draw, from guest memory now and from the snapshot of the walk, and the first floats of two vertex streams. For the uniform buffers it prints the first 16 floats, or every float for a draw of a program of `NXKIT_DIAG_FS` | sampled, and every listed draw | high |
| `pixel` | gpu | live | nine pixels of the presented texture, and draw and texture details | every 60th present, and draws on sampled frames | medium |
| `pixelall` | gpu | live | a counters line per frame and the lit pixels of each present, to match frames while bisecting with `nobar` | every 10th frame or present | medium |
| `present` | gpu | live | the counters of the present path (ticks, asked, found, blitted) and the presented texture | every 120th tick or present | low |
| `memory` | gpu | live | the Metal memory of the process (with the guest buffer, textures and buffers apart), the live libraries and pipelines and the number of resident allocations | every 120 frames | low |
| `pools` | gpu | live | `DIAG pools frame <n>`: the memory pool calls of the game (register, finalize, map and unmap a window) since the start. It counts the calls that changed the encoder state at once, and those that found the encoder busy and were queued for its next lock | every 300 frames | low |
| `depth` | gpu | live | distinct depth and stencil states, and the depth attachments of the passes | 40 distinct, 20 of every 500 passes | low |
| `clear` | gpu | live | depth and stencil clears (20 of every 1000). It also prints the colour clears: `DIAG clear walk colour` for each `ClearColor` record that the walker sees (the slot, the kind, the raw words, the decoded values and the scissor) and `DIAG clear encode colour` for each colour attachment that loads with Clear (the slot, the texture, the format, the size and the colour it gets). | depth and stencil as before, 60 for each colour line | low |
| `copy` | gpu | live | copies that the encoder refused, and the buffer copies and fills (kind, addresses, bytes, value, and whether they ran or why not) | first 400 | low |
| `tcopy` | gpu | live | copies that touch a texture that passes `NXKIT_DIAG_TEX`. It needs the filter. | first 300 | low |
| `comp` | gpu | live | each dispatch with its program, groups (and `indirect 0x..` for an indirect one) and result | sampled | low |
| `ubodump` | gpu | live | for each dispatch of the compute programs listed in `NXKIT_DIAG_FS`, one line per uniform buffer: `DIAG ubodump pass P cmd I program <key> cbuf <slot> addr 0x.. size N u64 [@192 0x.. @208 0x.. ... @448 0x..]`. The values are the little-endian 64-bit values at the byte offsets 192 to 448 in steps of 16 (the game keeps record pointers at 224 + 16 * slot). They come from the snapshot of the draw or from guest memory (`unreadable` when neither is there). Before them comes one line per storage buffer (`DIAG ubodump pass P cmd I program <key> sbuf <slot> addr 0x.. size N`). When push constants were set, the category also prints `... push N bytes u32 [first 16 words]`. Compare the values with the `indirect` address of the `zero-draw` lines of `meshvis`. For the draws of the render passes whose fragment program is listed, it prints one line per program, stage, storage buffer slot and frame: `DIAG ubodump pass P draw N fs <key> stage <fs\|vs\|ms\|ts> sbuf <slot> addr 0x.. size N snapshot <yes\|no> f32 [words 16 to 31 as floats] nonfinite <count>`. With a snapshot of the walk (what the shader reads), the line holds `snap [..] guest [..]`: the snapshot and guest memory at encode time, so a stale snapshot shows. `nonfinite` counts the non-finite words among the first min(size / 4, 4096) words (`snapshot (guest N)`). With the knob `ubodumpall` the category dumps every draw, once per program set (`vs/fs/ts/ms` keys), stage, slot and frame. It uses the short form `DIAG ubodump pass P draw N program <vs/fs/ts/ms> stage S sbuf <slot> addr 0x.. size N snapshot <yes\|no>` (no word lists, no `rw` flag: the writable bit belongs to the compiled program and is not reachable there). The limit is 600 lines per frame. | sampled frames, at most 64 lines per frame (400 with the knob `ubodumpall`, which dumps every dispatch) | low |
| `barr` | gpu | live | for each pass, the bound textures that the barrier planner does not know | sampled | medium |
| `nanscan` | gpu | live | after each render pass, counting kernels count the texels with a NaN or Inf in any channel (an all-ones exponent). They count each stored colour target of a float format (`Rgba16Float`, `Rg16Float`, `R16Float`, `Rgba32Float`, `Rg32Float`, `R32Float`, `Rg11B10Float`) in 2D, 2D array, 3D, cube and cube array textures that shaders may read. After the frame completed, a line per target with a count above 0: `DIAG nanscan frame F pass P prog <fragment program keys of the pass, at most 6> target <TexId> <format> <w>x<h>[x<d>] nonfinite N r R g G b B a A \| nan r R g G b B a A inf+ N inf- M \| rgb with alpha 0: X, finite: Y, non-finite: Z [at load L] first (x,y) ...`. The line has only the channels the format has. The `nan` part tells NaN per channel from Inf, and `inf+` and `inf-` count the infinities of RGB together. The alpha classes count the texels with a non-finite RGB. `first` gives the first 8 texels the kernel found, in no fixed order. A layered texture prints (x,y,slice) and ` slices [z: count, ...]`, at most 16 slices, then `left out N`. `NXKIT_DIAG_FS` limits it to the passes that hold a draw of the listed programs (and to the dispatches of the listed compute programs). For such a pass (and for every pass with the knob `nanload`) a target that the pass loads is counted before the pass too (`at load L`: a line whose `at load` equals the total says the pass did not make the values). With the filter, once per frame for each texture, the float textures that the listed programs sample are counted: `DIAG nanscan frame F pass P prog <key> samples <TexId> <format> <w>x<h>[x<d>] stage <fs\|ms\|ts\|vs> slot N nonfinite N ...` (a texture that the pass renders to is left out). With the filter (the listed keys may be compute program keys) or the knob `nancomp` (every dispatch), it counts the float storage images that the dispatches of a compute pass bind. It counts them before the pass and after it, outside the pass of the game: `DIAG nanscan frame F pass P prog <compute program keys> image <TexId> <format> <w>x<h>[x<d>] slot N nonfinite N r R g G b B a A [at load L] first ... slices [...]` (once per frame for each texture and program set, when the count changed over the pass, so an image that the pass cleaned shows too, or when the texture shows non-finite texels for the first time in the frame). For the listed compute programs the float textures that a dispatch samples are counted too, once per frame: `samples ... stage cs slot N`. Once per run for each distinct set: `DIAG nanscan pass programs (n): <keys>` and `DIAG nanscan pass mesh programs (n): <keys>`. A texture that cannot be counted (memoryless, no shader read, a view that Metal refuses) prints one note (first 10) | every frame, at most 40 target lines, 40 sample lines and 40 image lines per frame | high (waits for the GPU every frame) |
| `tiny` | gpu | live | the contents of textures of at most 256 bytes (for example 1x1 `Rgba32Float` pitch textures) and of watched guest ranges, as little-endian floats and hex words. `DIAG tiny upload <TexId> <format> WxH at 0x<guest addr> f32 [..] hex [..]` at each upload from guest memory (the guest bytes that it uploads). On sampled frames, after the GPU finished the frame, it prints `DIAG tiny copyback <TexId> <format> WxH at 0x.. f32 [..] hex [..]` for each such texture copied back into guest memory (read from guest memory). It also prints `DIAG tiny watch 0x<addr> f32 [..] hex [..]` for each range of `NXKIT_DIAG_WATCH` | upload: the first 400 lines of the run, then sampled frames. Copyback and watch: sampled frames only | low (waits for the GPU on sampled frames) |
| `lum` | gpu | live | nine pixels of each stored colour pass that passes the filter, read after the frame | sampled | high (waits for the GPU) |
| `img` | gpu | live | with `lum`: targets of 640x360 or more are sampled on a 96x54 grid and written as PPM pictures into `<NXKIT_DIAG_DIR>/img`. With the knob `imgfull` they are written texel by texel (formats of 4 bytes per texel) | sampled | high |
| `upload` | gpu | live | failed uploads, refused pitch-linear uploads, the pitch-linear textures made over their guest bytes (`DIAG upload in place`), and every upload of a texture that passes `NXKIT_DIAG_TEX` | first 300 / first 200 | low |
| `stale` | gpu | live | guest bytes of a texture that changed between frames, with the residency of the front end. A change while the texture is not `GuestNewer` is a CPU write that the front end did not see, unless the line says `in place true` (the texture is the guest bytes). | first 200 | high (hashes every texture that passes the filter every frame, so set `NXKIT_DIAG_TEX`) |
| `life` | gpu | live | Metal textures made and dropped, for textures that pass the filter | first 300 | low |
| `contain` | gpu | live | a new texture whose storage lies inside another one, or holds another one | first 300 | medium |
| `split` | gpu | live | a new texture at the address of another one with a different description | first 300 | low |
| `copyback` | gpu | live | the first copy-back of each texture, refused copy-backs, pins at the native size, the decision for each texture and why (`decision`: copied, off, frame-local, presented, format, no-pool, pool-cpu-no-access, window-cpu-no-access, compressed, with the texture flags and the pool flags) and a summary with the textures counted by reason | first 300 textures, 100 refusals, summary every 300 frames | low |
| `fit` | gpu | live | views made for the texture type that a shader declares | first 300 | low |
| `border` | gpu | live | once for each texture and border colour, a sampler that clamps to the border (wrap mode 4 or 5): the handle, the texture, the wrap modes, the colour, whether Metal can show the colour exactly (only transparent black, opaque black and opaque white exist), and the colour that the sampler gets. Every line also says that the colour is not applied where a program reads it through the `border` binding or a `_w` twin sampler, which is a known gap (`docs/nxkit-gpu.md`, "Limits") | 200 pairs | low |
| `hnd` | gpu | live | each distinct handle with its backing texture and view | 2000 distinct | medium |
| `bc` | gpu | live | the first bytes of each compressed texture in guest memory | 200 distinct | low |
| `prog` | gpu | live | programs that did not load, unknown binding kinds, draws without a vertex stage | once per program, first 40 draws | low |
| `pipe` | gpu | live | render, compute and depth-stencil states that Metal did not build. Every 60 frames, the state of the background builds (see "Archive coverage") | first 300, and one line per 60 frames | low |
| `gmem` | gpu | live | the size of the raw global memory table when it is built (`DIAG gmem table of N ranges (pools P, overlapping O, unresolved U, over V)`), and each pool that the write logs showed written (`docs/nxkit-gpu.md`) | each rebuild and each new pool | low |
| `overlap` | gpu | live | the flushes of the CPU, the dynamic pages, and per kind of buffer binding the bytes bound and the bytes changed since the last frame (`docs/plans/fence-overlap.md`) | every 120 frames | high (hashes every bound range) |
| `rename` | gpu | live | with the overlap on, on one frame in 60 and on the 20 frames after a range becomes `Rename`: `DIAG rename capture` (the non-zero words of each renamed range at the flush) and `DIAG rename wb` (each word the GPU changed, with the guest word and whether it was swapped, skipped or unchanged, which call wrote it back and on which thread). Every 120 frames it prints `DIAG rename waits` (honest waits by reason, overlapped waits, candidates by class) and `DIAG rename cpu-in-flight` (with the overlap on, in the frames that are judged one in 16, the ranges that frames bind as storage buffers, also those that are only read and those that are `Never`: the words that changed between the flush of the frame and the start of the wait for it and stayed until the GPU was done, with the frames in which that happened, and for a range that is only read these are CPU writes while the frame runs). `DIAG rename conflict addr A size S words N frames H of F judged` lists written ranges (`Never`) with CPU-changed words in flight in at least 2 judged frames. The words may include GPU writes that finished before the wait began. The host changes nothing for them. `DIAG rename cost ms per call (mean/max (calls))` times, with `NXKIT_DIAG=rename`, the entry points of the renamer in the last 120 frames (`at_flush`, of which `judge` and `capture`, `note_rw`, `begin_slot`, `before_wait`, `after_wait`, `merge` for the encode of the two merge passes, `bindings` for the look at the storage bindings of the frame and the classes of their keys) and shows the judging period. `DIAG rename waits forced by knobs` counts the full waits that each of the `wait*` knobs forced. The class counts of the waits line include `watch`, `snapshot`, `merge` and `conflict`, and the honest waits are split by `conflict` too. The knobs `renamenowb` (no write-back) and `renameoff` (no renaming at bind time, learning goes on) isolate a defect | 120 frames, sampled | low |
| `private` | gpu | live | with `NXKIT_PRIVATE=track` (which prints the lines without this category): `DIAG private track` has the tracked pages (count, MiB, peak, new in the window), the pages a frame may write per frame (pages, MiB, runs) and the ranges left out for their size. `DIAG private faults per frame` has the CPU writes to clean pages, writes in flight and reads in flight. It gives each by guest code, host code and internal threads (the encoder thread, the writer of the counter reports). It also has the other faults (races) and the time in the handler. `DIAG private per frame` has the pages dirtied (and how many by system calls), the system call accesses in flight and the flush and plan time. It also has the `mprotect` calls and their time, failed calls and log overflows. `DIAG private top program counters` has, per frame, the 10 program counters with most faults for a write in flight, a read in flight and a write to a clean page. Each is `module+offset` of a guest module, or the host image and symbol. With `NXKIT_PRIVATE=1` it also prints `DIAG private mirror`. This line has the mirror's MiB, runs and retired buffers, the pages staged per frame and the pages written again after they were staged. It has the pages the copy-in pass moved and lost, and the pages copied from the mirror into guest memory (and by a fault of guest code). It has the pages the closure over the bindings added and the waits for a writer (per frame, milliseconds, timeouts). It has the accesses of internal threads and the waits at the flush for frames in flight. Last, it has the resolves that reached past the members (`partial`, which should be 0) and whether `privpoison` is on. Then come `DIAG private writes to member pages per frame` and `DIAG private top program counters, write between flush and commit`. The first gives the first writes of pages by where the newest frame was (after its GPU work or before the next flush, between its flush and its commit, after its commit and before the GPU is done). It also gives the pages `privlatestage` staged again. The faults counted as other include the pages copied from the mirror. Without `NXKIT_PRIVATE` the category prints one line that nothing is tracked | every 300 frames | low (the tracking itself is not) | With `NXKIT_PRIVATE=1` and step 3 it also prints these lines. `DIAG private fences` has the waits answered at the flush and those that also waited for the frame 3 before (count, milliseconds per frame, longest). It also has the full waits by reason (`privhonest`, copy-back, idle GPU, flush not finished). `DIAG private CPU waits for a writer` has, for reads and for writes in flight, the waits per frame, their milliseconds per frame (all threads) and the longest. It also has how many began before the frame was committed. `DIAG private in flight` has the first accesses of game threads to pages of a frame in flight, sorted by address (inside the bytes of a binding or copy the frame may write, inside a raw-store pool, outside those bytes in a page they share, frame not kept) with the addresses touched most. It also prints `DIAG private between flush and commit` (per frame, the first writes of the CPU to member pages between the flush of a frame and its commit: in all, in bytes the frame writes, in bytes it reads through a binding by kind, in a page of one of its bindings but outside its bytes, in no binding of the frame, and how many frames had a flush that ran submits before the CPU signal they wait for) and its `exact` line (the bound read ranges per frame whose bytes in a written page differ at the commit from what the frame sees). It prints `DIAG private between sample` (up to 12 per window: a first write inside a bound read range, with the kind and the range). It prints `DIAG private forced submits` when a flush ran such submits (the frames, the time the encodes waited for the CPU signals, waits that ended at the 15 ms limit). The line `writes to member pages per frame` counts the first writes by where the newest frame is. `staged a second time` counts the pages of the late captures for forced submits and of `privlatestage`. With step 4 it also prints `DIAG private emulated accesses per frame` and `DIAG private top program counters, not emulated` (per frame). The first line has the loads and stores that the fault handler did itself, outside the bytes a frame in flight writes, by form: `ldr`, `ldrs`, `ldp`, `ldpsw`, `ldar`, `simd ldr`, `simd ldp`, `str`, `stp`, `stlr`, `simd str`, `simd stp`, `atomic`, `cas`, `ldxr`, `stxr`, `ld1`, `st1`. It also has the milliseconds they took in the handler and the pages the flush staged for the stores. Last, it has the accesses that were not emulated and waited for the writer, by class: `exclusive`, `atomic`, `ld1/st1`, `dc zva`, `prefetch`, `other`, `touches written bytes`, `page border or address mismatch`. The faults `read in flight` and `write in flight` and the waits for a writer count only what was not emulated. With step 7 (late results, on unless `NXKIT_PRIVATE_LATE=0`) it also prints `DIAG private late results, per frame`. This line has the late reads and writes (accesses to bytes a frame in flight writes, served from guest memory without a wait). It has the 64-byte blocks late writes took from the frame and the blocks copies left alone for them. It has the age of the data served (mean and longest in frames, and the count of 1, 2, 3 and 4 or more). It also has the pages and blocks copied at frame completions and by the flush, and the waits for a writer per frame (about 0).
| `judder` | nvn | live | the histogram of the refreshes each shown frame stayed, and the mean (see above) | every 120 shown frames | low |
| `hitch` | gpu | live | each frame whose encode took 7 ms or more or that built pipelines or textures, with the wall clock in ms, to put next to the late frames of `NXKIT_PACE_DIAG` | each such frame | low |
| `kpass` | gpu | live | the render passes that could run as compute dispatches (`docs/plans/kernel-passes.md`, step 0), the runs of them, and why the others cannot | every 300 frames | low |
| `ndve` | gpu | live | for each distinct set of movie weights, once: the capture list of `docs/plans/ndve-native.md` section 10 (the run, and for each dispatch its key, groups, storage and uniform buffers with the offset from the blob start, push constants as words and floats, textures and images with format, size and sampler). It also prints whether the plan was built or why it was refused, and which of the game's intermediate pictures the frame reads later. Also each reason the game's chain ran instead (first 50), and the `NXKIT_NDVE_COMPARE=1` results | once per set of weights, and the compare every 120 movie frames | low (the compare reads two pictures) |
| `ndvetrace` | gpu | live | with the Neural Engine path on (the default), at the first NDVE dispatch and 60 dispatches later: the descriptors of the luma and chroma textures and their memory pool. It prints whether the decoder's latest picture lies in the same pool or the same VM object (two views of one memory), with 4 KiB compared. It prints every command of this and the previous frame that writes the planes (copies, image and storage buffer binds, attachments, dirty tracking). It also prints the first 64 bytes of each plane and the fingerprints | two reports | low (copies the commands of two frames while it waits) |
| `passprof` | gpu | live | the GPU time of each encoded pass. Every render, compute, copy and CPU-read pass writes a precise timestamp as the last command of its encoder (one heap of 4096 entries of a ring of 6, read when its turn comes round). The copy-back at the end of the frame writes one more. The time of a pass is its end minus the end of the timed pass before it. The report is the last read frame: `DIAG passprof frame <n>: total <ms> ms over <k> passes; render <ms> (<count>), compute ..., copy ...`. Then come the 40 most expensive passes as `DIAG passprof   pass <i> <ms> ms <kind> cmds <n> ...`, and the mean total. For render, a pass line gives the draws, the fragment program of the first draw, and size and format of the first attachment. For compute, it gives up to 3 programs with their groups or `indirect@addr`. For copy, it gives kind, size, source and destination. For cpuread, it gives the textures. `[chunk start]` marks the first pass of a command buffer, whose time includes the gap before it. The times are a guide: precise timestamps make the passes run one after the other. Also, on the tile GPU the end of a render pass can show up in the pass after it | every 120 frames | low (the passes run one after the other) |
| `nativeprof` | gpu | live | the mean GPU time of each step of a native plan (precise timestamps, one heap of a ring of 6) and the sum | every 120 frames | low (the steps run one after the other) |
| `ndsr` | gpu | live | for each distinct NDSR run, once: the capture list (the three SMAA dispatches and the network), whether the plan was built or why it was refused, and the `NXKIT_NDSR_COMPARE=1` results | once per run | low |
| `tupscale` | gpu | live | the temporal upscale (R72). Once per new set of textures, it prints `DIAG tupscale config`. This line has the texture id, format and size of the picture, the depth, the velocity, the target and each pass-through copy, and the row direction of the source pass. Every 120 frames it prints `DIAG tupscale frame`: the frames that ran, the resets, the frames without the upscale draw and the frames that fell back by reason. For the last frame, this line also has the jitter (clip units and pixels), how far `ClipToPrevClip` is from the identity, its row 3 and whether it was a cut | once per configuration, every 120 frames | low |
| `codecall` | video | live | every `nn::codec` call with its time (`codec` prints the opens and the decoder in use) | every call | low |
| `rendercond` | nvn, gpu | live | per frame with a render condition, a frame line. It has the `SetRenderEnable` calls (and how many turned rendering off) and the conditions true, false and unknown (a counter that did not land, or that the host does not count, renders). It has how many of them read landed counters, stale ones (the last that landed at the address, one frame late) and ones by place (the report at the same place of an earlier frame). It has the emulated reports that were 0 at once (right after a reset, no draw). It has the reports the front end took (values, emulated GPU counters, GPU-only ones) with the GPU counter reports by `NVNcounterType`, and the slots of the frame. Last, it has the draws, clears, dispatches and copies left out. Then come the first evaluations with the two counters they read. Once per condition address (the first 256), it prints what the condition read of each counter (table entry, landed at the address and at the place). It also prints the landed addresses nearest to it and the addresses of the reports of the frame so far. From the backend, per finished frame with counters: the reports landed (samples, vertices or primitives), not counted (a draw of a slot was not encoded, or was indirect) and outside guest memory | first 300 frames, then every 60th. 200 evaluations. Backend lines: first 30, then every 120th. With a render knob on, the frame line ends with `forced decisions rcflag N rcunk N rcall N`. On a sampled frame (one in 300 from frame 150), it prints one line for each command a condition covers, per distinct (kind, fragment program, target format, condition source, decision). It prints at most 200 per run: `DIAG rendercond gate <draw\|clear\|dispatch\|copy> pass <index or -> <pass kind or none> prog <fragment program key, dispatch program key or -> blend <off or on src/dst NVN factors> depth test <function or off> write <on\|off> mask <colour write mask in hex> target <format> <w>x<h> cond <counter\|memory\|unknown> decision <render\|skip\|forced-rcflag-render\|forced-rcunk-skip\|forced-rcall-render> addr <hex> [samepass yes\|no\|unknown] mode <n> reads <kind:value> <kind:value> [clear ...]`. The state comes from the detail of the frame, which also holds the draws that the condition left out. A draw's target is the first colour target of the render targets set before it, else the depth target. The size is that of the Metal texture (scaled) or of the NVN record. `cond memory` means both operands are plain guest memory, `counter` that an operand is a report of the table, `unknown` that the result is unknown. `samepass` (counter conditions) tells whether the draws between the reset of the counter and the report lie in the pass of the command. For a memory condition, when the GPU finished the frame, it prints `DIAG rendercond flag addr <hex> at-append <a>,<b> after-gpu <a>,<b>` (the two words when the host decided, and read again), at most 40 per sampled frame. It also prints `DIAG rendercond flag writer <storage-buffer\|uniform-buffer-bound\|copy-buffer> <start>..<end> (flag <hex>)` for bindings and buffer copy destinations of the frame that overlap the 32 bytes (the inline uniform updates of the walker are not listed). With the `freshzero` knob, on a sampled frame under `rendercond` or `life`: `DIAG freshzero cleared N skipped M` | low |
| `queues` | rt | live | per flush with waits or signals between queues, the submits whose waits were read and the waits that ran. It also prints the submits held, released by their signal and forced at a flush, the GPU event signals and fences, and the CPU event signals. For the first holds it prints the queue, the wait (event, mode, value) and the counter the gate saw. It also prints the release (after how many ops of other queues) and the forced runs with the wait they did not get. A flush first forces the held op whose wait no held work can satisfy (typically a CPU signal that comes after the present). Then it runs what that releases. So a queue that waits for the GPU signal of another held queue still comes after it | first 300 flushes, then every 60th, and 100 lines each | low |
| `gateseq` | rt | start | the calls that matter to the queue gate of one frame in 300, in order, as `GATESEQ f<frame> +<ms since the last present> <what>`: `SUBMIT` (queue, recordings, the waits and signals inside), `QUEUE FENCE`, `QUEUE WAIT`, `CPU EVENT SIGNAL`, `nvnSyncWait`, `CPU WAIT`, `HOLD` (the wait, its counter, held queues that would satisfy it, the last GPU and CPU signal of it), `RELEASE`, `FORCE`, `RUN` (the op goes into the frame), `FLUSH` (reason, fence, cmds, work, held ops) and `PRESENT` (a flush after a release is `FLUSH Release` with `RELEASE FLUSH` before it). Every line has the thread (`t<n>`). A waiting call (`nvnSyncWait`, `CPU WAIT`) has a second line `RETURNS ... after N ms`. Every batch committed in the traced frame prints `BATCH` when it completes (also after the frame ended). This line has its command buffer count and GPU start and end on the same clock (`CLOCK_UPTIME_RAW`, as Metal's GPU times). When the batch was committed in the traced frame, the line also has two times. The first is from the start of its flush (for a release flush, from the CPU event signal) to its first commit. The second is from the commit to the GPU start Start only: the host builds the hooks for the batches of the GPU once, so a change of `NXKIT_DIAG` does not turn this category on or off. Start the game with it. | one frame in 300 | low |
| `events` | gpu | live | per completion of a frame with GPU event signals (`nvnCommandBufferSignalEvent`, `docs/nxkit-gpu.md`, "GPU event signals"): `DIAG events fence <n>: GPU signals written <n> added <n> dropped for a newer CPU signal <n> outside guest memory <n>`. `dropped` counts writes from a frame that a CPU signal after an early wait replaced | first 100, then every 600th | low |
| `walkmiss` | gpu | live | with `NXKIT_WALK_CACHE=1`: the field of the walker state that made a call miss the templates of its handle | every 20000 misses | medium |
| `passmerge` | nvn, gpu | live | what the pass plan still leaves. It gives the render passes (with one draw, empty) and the neighbours with equal attachments that stayed apart and why (clear, sampled, other). It gives the pairs split by a compute or copy pass, and how many of them touch none of the textures. It gives the loads that are `Load` only by the default rule, the attachments no draw wrote and the stores kept only by assumption. From the encoder, it gives whether the first draw of such a load covers the whole target | every 30 frames (nvn line), sampled frames (cover lines) | low |
| `passes` | gpu | live | the render passes of a frame by their number of draws and by the size of their first attachment | every 300 frames | low |
| `indirect` | gpu | live | draw records without arguments (transform feedback, `DrawTexture` or unknown draw kinds) | at powers of two per kind | low |
| `mesh` | gpu | live | the mesh and indirect draws of the frame (`EncodeStats::kinds`): encoded and skipped by reason for each class (mesh, mesh-indirect, indirect, multi-indirect-count). It also gives the Metal draws they became, the compute pre-passes and their records, and what was clamped. The first frame with such draws prints the line without the category. `stats` turns it on too. | every 30 frames | low |
| `meshvis` | gpu | live | why mesh geometry does not show. On a sampled frame (the first 3 frames with a mesh draw, then every 120th frame, or every frame with the knob `meshvisall`) every mesh draw gets a visibility offset of its own and counts its samples. When the GPU finished the frame, it first prints a line for each render pass with mesh draws (colour and depth attachments, mesh draws, counted draws). Then it prints a line for each group of its mesh draws with the same kind, task, mesh and fragment key, depth function, depth write and cull mode. A group line has the draws, draws with samples, samples, the largest count of one draw, the workgroups and the draws with none. `unread` counts indirect draws whose record could not be read. It prints at most 200 groups per frame. Then it prints the time line of each depth texture that a pass with mesh draws uses. The time line has one line for each pass on it (load, clear value, store, draws, mesh draws) and for each depth clear under a render condition (condition source, decision). It also has a line for each copy that reads or writes the texture: `refused` when the sizes differ, `skipped` when a false condition leaves it out. It prints at most 120 of these lines per frame. Last, it prints a summary (`counted`, `shared`, `over-limit`, the workgroups, and the knobs that are on). The workgroups of an indirect draw are read from its record in guest memory after the frame, because the game's compute writes the record on the GPU. A draw that the game's own occlusion report counts is `shared` and stays uncounted. At most 8192 mesh draws of a frame are counted. It looks at the first 8 indirect draws with the workgroup count 0 (`zero-draw`) and the first 4 with a task shader and a count above 0 (`ok-draw`). For each, it prints the guest address of the record and its first 4 words. It also prints the range of the gmem table and the learned pool (`written`) that hold the address. The frame waits for the GPU. The category also makes the front end record the render conditions of every frame (as `rendercond` does). | first 3 frames with mesh draws, then every 120th frame | medium |
| `frame` | rt, gpu | start | where the time of a late frame went on the threads that call NVN: the time in `nvnSyncWait`, in the submits and flushes, in the present and in the rest, and the parts of it (flush, backend, decode, walk-join, acquire). A frame that takes more than 1.2 vsyncs of 60 Hz prints one line. Start only: it needs the hooks for the batches of the GPU, which the host builds at the start. | one line per late frame | low |

## Knobs (`NXKIT_KNOB`)

Knobs are experiments. A frame with a knob does not render correctly. Use knobs to find out which part of the state causes a defect.

| knob | effect |
|---|---|
| `nodepth` | Turns off the depth and stencil tests. |
| `nocull` | Turns off culling. |
| `nooffset` | Sets the depth bias to zero. |
| `noeq` | Changes the depth test `equal` to `always`. |
| `flat` | A draw into four colour targets that its fragment shader all writes gets constant G-buffer values. |
| `paint` | Paints magenta every draw into one colour target, or only the programs of `NXKIT_PAINT_FS` in every pass. |
| `paintdir` | Same as `paint`, but the colour is the absolute value of the first varying. |
| `cubearr` | Makes a cube view as a cube array. |
| `allbar` | Gives every pass a full barrier from all earlier work. This is the same as a `nobar` range that covers every pass. |
| `nowait` | `nvnSyncWait` returns at once. The game may write memory that the frame on the GPU reads. The knob measures what the wait costs. |
| `overlap` | The same as `NXKIT_OVERLAP=1`, switched while the game runs. |
| `nosnap` | With the overlap on, the walk does not copy the uniform buffers. The picture is wrong. The knob measures what the copies cost. |
| `vsbar` | A render pass that samples what an earlier pass wrote holds its vertex stage too, also when no vertex shader of the pass samples (the behaviour before). |
| `nosnapsbuf` | With the overlap on, storage buffers that draws only read use the buffer itself instead of the walk's copy. This is the behaviour before, and the dialog text of Breath of the Wild then flies in. |
| `privpoison` | With `NXKIT_PRIVATE=1`, the host overwrites the guest bytes of the pages that a frame writes with a quiet NaN pattern when it hands the pages to the GPU. The CPU never sees it (its access copies the page from the mirror). A GPU path that still reads guest memory instead of the mirror shows the pattern as NaN in the picture. A CPU store in the instant of the overwrite is lost. |
| `privnoubo`, `privnosbuf`, `privnostream`, `privnoupload`, `privnogmem`, `privnocopy` | With `NXKIT_PRIVATE=1`, bisect which class of resolves goes wrong. While the knob is on, the class binds guest memory instead of the mirror in each frame. The classes are uniform buffers, storage buffers, vertex streams (with indices, indirect arguments and the pull layout), texture upload sources and texture and buffer copies, the table of raw global memory, and buffer copies and fills. The pages stay members. Guest memory is only current for pages that a frame wrote if they were copied back. So while any of these knobs is on, the host copies every page that a frame wrote from the mirror into guest memory when the frame completes. The CPU's pages for the next frame are staged as usual. Caveat: in the honest-wait mode, and for pages that the GPU did not write, guest memory equals the mirror anyway. So a defect that stays with the knob on is not in that class, and a defect that goes away is. The picture is not meant to be right in every case: guest memory sees a CPU write after the flush that the mirror misses. |
| `privlatestage` | With `NXKIT_PRIVATE=1`, the host stages a second time the pages that the CPU wrote between the flush of a frame and the commit of its first command buffer. A pass at the end of that buffer copies them into the mirror. Tests whether CPU writes after the flush are the defect. It is a comparison for honest waits (`privhonest`): with early fences it gives a frame the data that the CPU wrote for the next one. `DIAG private` shows how many pages it moved. |
| `privnoemu` | With `NXKIT_PRIVATE=1`, the fault handler does not itself do the CPU's accesses to a page that a frame in flight writes (outside the bytes it writes). Every such access waits for the frame, as in step 3. Compares the picture and the waits with and without the emulation of step 4. The copy of stale blocks from the mirror stays exact. `DIAG private` prints no line for the emulation while it is on. |
| `privnoeager` | With `NXKIT_PRIVATE=1`, a completed frame does not copy back the pages that game threads faulted on for a copy lately (on a thread of its own). The faults copy them as in step 4. Compares the faults and the handler time with and without. |
| `privnosplit` | With `NXKIT_PRIVATE=1`, frames are not cut for pass signals: a CPU access to a page a frame writes waits for the whole batch, as in step 5. |
| `privnoahead` | With `NXKIT_PRIVATE=1`, the first group of a cut frame is committed with the rest after the whole frame is encoded, not while the other chunks still encode. |
| `privnohot` | With `NXKIT_PRIVATE=1`, pages that the CPU dirties in several flushes in a row are protected after every flush as before, not kept writable and compared at the flush. Compares the clean-page write faults with and without. |
| `privhonest` | With `NXKIT_PRIVATE=1`, a wait for a fence waits for the GPU in full (the behaviour of step 2) and the walk takes no copies of the bound ranges that are not private. For comparing the early fences with the honest waits. |
| `renamenowb` | With the overlap on, a renamed storage range is bound to its copy as usual but the changed words are never written back into guest memory. |
| `conflictwait` | With the overlap on, a frame that binds a written range that the CPU also writes while the frame runs waits in full. Checks whether such a range causes a defect. |
| `waitnever` | With the overlap on, a frame that binds a written storage range that is `Never`, `Idle` or not decided yet waits in full. For bisecting a defect: with the knob on, the flicker or the wrong picture goes away if such a range is the cause. |
| `waitwatch` | The same for a frame that binds a read-only storage range that is not a snapshot (watched or not decided). |
| `waitmerge` | The same for a frame that binds a merge range (to test whether the merge itself is wrong). |
| `waitsnap` | The same for a frame that binds a snapshot. |
| `waitrename` | The same for a frame that binds a renamed range. |
| `waitother` | The same for a frame with a vertex, index or indirect buffer, or the source of a buffer copy, of at most 1 MiB whose bytes changed since the flush before. These are CPU-written resources that the renamer does not cover. The bytes are hashed once per flush, only while the knob is on. |
| `waitall` | Every frame waits in full: the control, which should behave like the overlap off. |
| `renameoff` | With the overlap on, no storage range is renamed at bind time. The learning still runs. |
| `nopassmerge` | The pass plan keeps render passes with no draw and no clear, and merges only after a barrier (the behaviour before). Not wrong, only slower: it compares the frame time. |
| `noloadstore` | The pass plan does not learn dead stores of textures that start every frame with a clear. Not wrong in a game that behaves. It compares the frame time. |
| `coverload` | Opt in. A colour attachment whose first draw visibly covers the whole target (full-screen triangle, no vertex input, full viewport, no blend or test) is loaded as `DontCare`. A fragment shader that discards would show garbage. |
| `nobarrier` | Gives no pass a barrier. The picture is wrong. The knob measures what the barriers cost the GPU. |
| `rcflag` | A render condition whose two operands both are plain guest memory (not a report of the table) renders. Tells whether the memory conditions leave out work that the console does not. The decisions it forced are counted on the `rendercond` frame line (`forced decisions rcflag N`). |
| `rcunk` | A render condition with an unknown result (a counter only the GPU knows, or `NXKIT_COUNTERS=0`) is left out instead of rendered. |
| `rcall` | Every `SetRenderEnableConditional` renders. `SetRenderEnable` is not changed: the front end records it as a plain on or off mark, and it has no operands to compare. It wins over `rcflag` and `rcunk`. |
| `rcnostale` | The same as `NXKIT_RENDERCOND_STALE=0`, switched while the game runs: a condition reads no counter of an earlier report. |
| `rc3d` | The same as `NXKIT_RENDERCOND=3d`, switched while the game runs: a false condition leaves out draws and clears only. The front end marks the commands under a false condition when it appends a stream. The encoder chooses the commands when it encodes them, so the switch acts on the next frame. |
| `freshzero` | A Metal texture that the store made new for the frame and that gets no upload (a record that the GPU wrote last, a scaled or a widened texture) is cleared to zero before the frame uses it. The clear sets colour 0,0,0,0, depth 0 and stencil 0 in every level, layer and depth plane, with one render pass each in the first command buffer. Compressed, memoryless and stencil-only textures, and textures without render target usage, are skipped. A scaled texture whose guest copy is newer gets the scaled upload and is not cleared. |
| `nosupersede` | The supersede rule of the texture store is off, the same as `NXKIT_TEX_SUPERSEDE=0`, switched while the game runs. It compares both at one place. |
| `nobz` | The blend word of every pixel shader is 0. So the shader does not set to 0 a source colour whose blend factor is 0, and the Apple blender gives NaN for `0 x NaN`. It shows the blots that the rule removes. |
| `meshvisall` | With `NXKIT_DIAG=meshvis`, every frame is a sampled frame. |
| `nanload` | With `NXKIT_DIAG=nanscan`, a pass that loads its float colour target counts the target before the pass too, also when `NXKIT_DIAG_FS` is empty. |
| `ubodumpall` | With `NXKIT_DIAG=ubodump`, every dispatch of a sampled frame is dumped, not only those of the programs in `NXKIT_DIAG_FS`. The budget is 400 lines per frame. |
| `fsquiet` | The draws of the programs of `NXKIT_DIAG_FS` print nothing and use none of their 200 lines. Set it in the file of `NXKIT_KNOB_FILE` until the game shows the scene to look at, then clear it. |
| `imgfull` | With `NXKIT_DIAG=lum,img`, the targets of 640x360 or more are written at their full size instead of the 96x54 grid: one copy of the whole target and one read. Only formats of 4 bytes per texel (`Rg11B10Float`, `Rgba8Unorm`, `Bgra8Unorm`, `Rgb10A2Unorm` and the like). Set it in the file of `NXKIT_KNOB_FILE` for the frame to look at, a sampled frame writes about 3.5 MB per target. |
| `tupjx`, `tupjy`, `tupmx`, `tupmy` | The temporal upscale (R72) flips the sign of the jitter in x or y, or of the motion in x or y, that it gives MetalFX. Set them in the file of `NXKIT_KNOB_FILE` to find a wrong sign while the game runs. A wrong jitter sign shimmers on a still picture. A wrong motion sign smears or ghosts what moves. |
| `nancomp` | With `NXKIT_DIAG=nanscan`, the float storage images of every compute dispatch are counted before and after the compute pass (`image` lines), also when `NXKIT_DIAG_FS` is empty. |

## Patch modules

A patch module lets you test a change of the host's behaviour in a running game. You do not build the host again. You do not restart the game. The state stays in the host, so the game keeps its scene. A fix that a patch proves goes into the host for good (W38).

The host cannot replace its own Rust code. Rust has no stable ABI, and the host keeps its state in objects and statics. So the host has fixed hook points and a small C ABI. A patch module is a separate crate that builds to a dynamic library. The control socket loads it.

### The hook points

The hook points are in the encoder of `nxkit-gpu`. A hook gets plain data and gives an answer. The default answer changes nothing.

| hook | the patch gets | the patch can answer |
|---|---|---|
| frame begin | the frame number, the number of passes and draws | nothing |
| frame end | the frame number, the render passes, the draws, the skipped commands, the command buffers, the pipeline builds, the encode time in nanoseconds | nothing |
| render pass, before the host encodes it | the frame, the index of the pass, its first draw and draw count, each attachment (texture id, slot, size, format, load and store action), the program pairs of its draws (vertex and fragment key, at most 16) | skip the pass, force a full barrier before it, change the load action of colour slots (with a clear colour), mark it for capture |
| draw, before the host encodes it | the frame, the pass, the draw number, the topology, the vertex and fragment key, the texture binds (stage, slot, texture id, handle), the blend state and write mask of each colour slot, the depth and stencil flags, the stencil reference | skip the draw, replace the blend state of colour slots, bind another texture in up to four slots, replace the stencil reference |
| log (the host gives it to the patch) | nothing | a line of text. The host prints it in the log with the prefix `nxkit-patch:` |

The structs are in `crates/nxkit-patch-abi`. A texture id of a draw is the id of the texture that the game's handle resolved to in this frame. A forced texture must be a texture that a draw of this frame binds too. Else the draw keeps its own texture. The pass hook runs for render passes only. Compute and copy passes have no hook.

A pass that a patch marks for capture becomes the focus pass of `NXKIT_DIAG=lum`. Without that category the mark does nothing.

### Write and build a patch

Make a new patch from the example:

```bash
tools/nxkit-patch new my-fix
```

This writes `tools/patches/my-fix`. The folder is not in git. Edit `src/lib.rs`. The example skips the draws of one fragment program (`SKIP_FRAGMENT`) and logs each render pass of its first frames. A patch sets its name and its hooks in the init function:

```rust
extern "C" fn draw(info: &DrawInfo, answer: &mut DrawAnswer) {
    if info.programs.fragment.as_str() == "KEY" {
        answer.skip();
    }
}

nxkit_patch_abi::export_patch!(init);

fn init(host: &HostApi, table: &mut PatchTable) -> bool {
    table.set_name("my-fix");
    table.draw = Some(draw);
    host.log("loaded");
    true
}
```

Build it in its folder. It takes about one second:

```bash
cd tools/patches/my-fix && cargo build --release
```

The result is `target/release/libmy_fix.dylib`. `tools/nxkit-patch build DIR` builds and prints this path.

The example is not a member of the workspace. A dynamic library in the workspace would build with every run of the workspace. It has its own `[workspace]`, its own lock file and its own target folder. It uses only the contract crate `nxkit-patch-abi` by path.

### Load, change and unload

Use absolute paths. The verbs are:

```bash
tools/nxkit-ctl patch load /abs/path/libmy_fix.dylib
tools/nxkit-ctl patch list
tools/nxkit-ctl patch reload
tools/nxkit-ctl patch unload my-fix
```

- `patch load PATH` copies the library to a path of its own, loads it and starts its hooks. The host replaces a patch of the same name at once.
- `patch list` shows each loaded patch with its hooks and its path.
- `patch reload [NAME]` loads the file of each loaded patch (or of one) again. Use it after a rebuild. If the load fails, the old patch stays.
- `patch unload [NAME]` stops the hooks and unloads the library. Without a name it unloads all patches.

The loop of an experiment is: edit, `cargo build --release`, `patch reload`, look at the game, `log 100`. You can load more than one patch. The host calls them in load order and merges the answers. A later answer wins when two patches change the same thing.

### How unload stays safe

The host copies the library to a new path before it loads it. Thus a rebuild of the same file is a new image, and the dyld cache of the old one is not used. The host checks the ABI version first. It refuses a patch of another version.

A patch call runs under a read lock. Unload and replace take the write lock. They return only when no encoder thread is inside a patch call. After that, no thread can enter the old patch. Then the host calls `dlclose`.

Without a loaded patch, each hook point costs one relaxed atomic load. A draw hook has one more effect. The encoder then does not replay recorded runs of draws, as with `NXKIT_SKIP_PROGRAMS`. A loaded patch also makes the frame note its texture handles.

### Limits

- Only the hook points can change behaviour. A patch cannot change anything else in the host.
- A patch runs in the host process. A bug in it can crash the game. A panic in a hook aborts the process. Keep hooks small. Do not use `unwrap`.
- A patch must not start a thread, keep a callback in the host or keep a pointer past the unload. Its code can go away.
- A library that has thread-local variables (a Rust `cdylib` that prints with `println!` has them) stays mapped after `dlclose`. The hooks stop and the memory stays until the game ends. A reload still works, because each load is a new image.
- The ABI version is exact. Any change of a struct changes the version. Build the patch again after you update the host.
- The pass and draw hooks run on the encoder threads. Several threads can call a hook at the same time. A patch keeps its state in atomics or behind a lock.
- A forced texture needs a texture that this frame binds in some draw. A texture that a shader reads through the bindless pool, with no bound handle, has no hook.
- A skipped pass or draw counts as `skip.listed`. The frame does not render correctly.
- The host does not check what a patch does with `unsafe` code. The loader is an experiment tool. Do not ship a patch.

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
| `skip.listed` | encoder | A draw whose program is listed in `NXKIT_SKIP_PROGRAMS` or `NXKIT_SKIP_FILE`, or a draw that a patch module skips (also each draw of a pass that it skips). A debugging aid: the frame does not render correctly. |
| `walk.draw-no-args` | walker | A draw record without draw arguments (transform feedback, `DrawTexture` or an unknown draw kind). The draw gets an empty detail. |
| `walk.draw-no-vertex` | walker | The bound program has no vertex stage, or a mesh draw finds no program with a mesh stage. |
| `pipe.depth-mode-ignored` | encoder | The device is in the depth mode 0 to 1 (`nvnDeviceSetDepthMode`), but a vertex or mesh function has no entry that keeps the clip depth (a pulling `_bp` library, or a library from before `main0_z`). The draw converts the depth as for -1 to 1. |
| `walk.stage-ignored` | walker | A geometry or tessellation stage. The encoder runs the vertex, fragment, task and mesh stages only. It counts once per program key. |
| `decode.unresolved` | decoder | A texture handle or address in a recording names no texture. It counts once per decoded recording. A recording that the cache answers does not count again. A handle of texture id 0 is an unbind (NVN reserves the first descriptors) and does not count. The examples name the handle or address. |
| `decode.bad-record` | decoder | A record could not be read. The rest of the recording is lost. |
| `tex.handle-unresolved` | encoder | A texture handle has no descriptor or no texture record. This is the case of a pool slot that points to a dropped texture. |
| `tex.ubo-handle-unread` | encoder | The handle word of a `tex_ubo` or `samp_ubo` member could not be read. Its uniform slot is not bound, the word lies past the end of the buffer, or guest memory is not readable there. The member reads the null texture and the default sampler. |
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
| `bind.tex-pool` / `bind.samp-pool` | encoder | A texture or sampler pool table written into an argument buffer. A count, not a loss. |
| `pool.wanted` | encoder | The texture IDs that shaders read through the pool table (marked in the bitmap in the last two sweeps), summed over the frames that have a pool binding. A count, not a loss. |
| `pool.filled` | encoder | Elements of the texture table written because a shader marked their ID. A count, not a loss. |
| `pool.unresolved` | encoder | A marked ID with no slot in the pool or with a dead texture. Its element is 0 and the shader reads the null texture. The example names an ID. |
| `gmem.pools-over` | encoder | Memory pools that do not fit the 8191 entries of the raw global memory table. Raw loads into them read 0. |
| `gmem.pool-unresolved` | encoder | A registered memory pool that the pool snapshot does not resolve. It is left out of the raw global memory table, so raw loads into it read 0. The example gives the range. |
| `pool.cleared` | encoder | Elements set to 0 because the Metal texture behind them was dropped or replaced. A count, not a loss: the next frame fills a wanted ID again. |
| `overlap.renamed` | encoder | Storage ranges that a frame of the overlap read and wrote from a copy in its batch slot (`docs/nxkit-gpu.md`, "Renamed storage ranges under the overlap"), counted per range and frame. A count, not a loss. |
| `overlap.writeback-words` | wait | 32-bit words of a renamed range that the GPU changed and that went back into guest memory in a CPU wait. A count, not a loss. |
| `overlap.writeback-skipped` | wait | Words that the GPU changed and that did not go back, because the CPU had written the word since the flush of the frame. A count, not a loss. |
| `overlap.snapshot-bytes` | flush | Bytes of snapshots (read-only storage ranges that the CPU writes while a frame runs) that a frame copied. A count, not a loss. |
| `overlap.snapshot-over-budget` | flush | Snapshots that a frame did not copy because its snapshots went over 8 MiB. They are bound to guest memory as before. A count, not a loss. |
| `overlap.merge-ranges` | encoder | Merge ranges that a frame bound to a copy that the GPU fills and merges back (`docs/nxkit-gpu.md`), counted per range and frame. A count, not a loss. |
| `overlap.merge-conflict` | wait | Written ranges whose words that the CPU writes in flight and words that the GPU writes overlap, counted once per range. The wait for a frame that binds one is a full wait. A count, not a loss. |
| `overlap.key-disagree` | flush | A range that two bindings (programs or slots) need in ways that cannot be combined (a merge and a renamed range). The wait for a frame that binds it is a full one. A count, not a loss. |
| `overlap.rename-excluded` | flush, encoder | A learned range that a frame did not rename. The cause is one of these: a copy, a vertex, index, indirect or uniform buffer overlaps it, a storage binding covers it only in part, two learned ranges overlap, or the pool holds raw global writes. It is bound to guest memory as without the rename. A count, not a loss. |

With the overlap on, the host always prints one line when it has decided about a storage range (`docs/nxkit-gpu.md`): `DIAG overlap rename <addr> <size> Rename|Never after <n> frames`. `Rename` means that the GPU writes the range and the CPU re-initialises every word it wrote, in each of the last 8 frames in which the GPU wrote. `Never` means that the GPU keeps a word from frame to frame, or that a renamed range failed its check (every 300 frames).

The native replacement of a chain of dispatches (`ndve`, see `NXKIT_NDVE`) prints its own block after the report, with the same cadence. It also prints the block when a counter shows up that the last block did not have. It is not in the table above, because the reasons live in `nxkit-nvn`, which knows no network:

| line | meaning |
|---|---|
| `ndve.native` | Frames in which the plan ran in place of the game's dispatches. |
| `ndve.fallback(reason)` | Frames in which the run was found and the game's dispatches ran. The reasons are `run` and `interleaved` (the commands around the run are not only dispatches and barriers), `binding` and `memory` (a weight buffer is not bound or lies outside guest memory), `weights` (the bytes are not the network's), `build`, `buffer`, `pipeline` and `tgmem` (the plan or its kernels could not be made), `push`, `resource` and `spec` (a texture, image or push block the plan names is not bound where the replacement expects it), `encode` (Metal refused the pass, and the picture of that frame is lost). The example gives the sentence. |

The walker counts only when it reads a record. A recording that plays from the template cache does not count again. Without `NXKIT_WALK_CACHE=1` every frame counts. With `NXKIT_VERIFY_CACHE=1`, a recording is walked twice and counts twice.

To add a reason, add it to `reasons!` in `nxkit-nvn/src/diag.rs`, call `nxkit_nvn::diag::note` where the work is dropped, and add a line to the table above.

## Input and replay

An agent or a person can move in a running game through the control socket. The input service (`nxkit-svc-input`) merges the injected input with the controllers, the keyboard and the input file. Player 1 is the default. The Home menu still takes the controllers.

### Verbs

| verb | effect |
|---|---|
| `press BUTTONS [MS] [player N]` | Presses the buttons for MS milliseconds (100), then lets them go. |
| `hold BUTTONS [player N]` | Holds the buttons until a `release`. |
| `release [BUTTONS] [player N]` | Lets the buttons go. Without buttons it lets everything go. |
| `stick l\|r X Y [MS] [player N]` | Sets a stick. X and Y go from -1 to 1. Y up is positive. Without MS the stick stays until the next `stick`. |
| `input COMMAND` | One line of the input script. It takes the commands above. |
| `record start FILE [frames\|ms]` | Writes what player 1 gives the game to FILE. The default timing is `frames`. |
| `record stop` | Ends the recording. `record` alone shows the state. |
| `replay FILE` | Plays an input script now. `replay stop` stops it. `replay` alone shows the state. |

BUTTONS are the names `a`, `b`, `x`, `y`, `l`, `r`, `zl`, `zr`, `plus`, `minus`, `lstick`, `rstick`, `dpad_up`, `dpad_down`, `dpad_left`, `dpad_right`, and the stick directions `ls_up` and so on. Write several buttons as `a b` or `a+b`. N is 1 to 8. FILE is a path that the game process can open. Use an absolute path.

```bash
tools/nxkit-ctl press a
tools/nxkit-ctl press dpad_down 50
tools/nxkit-ctl hold zr
tools/nxkit-ctl release zr
tools/nxkit-ctl stick l 0 1 2000
tools/nxkit-ctl input press plus+minus 200 player 2
```

### The input script

`NXKIT_INPUT_FILE`, `record` and `replay` use one grammar. A line is one command. A comma also ends a command. A line that starts with `#` is a comment. The commands are the five above and these waits:

| command | effect |
|---|---|
| `wait MS`, `wait 500ms`, `wait 2.5s` | Waits on the wall clock. |
| `wait frames N` | Waits for N presented frames. |
| `wait frame N` | Waits until N frames were presented since the start of the process. |

A script that has one of these commands is a timeline. Its commands run in order. Only a `wait` stops the cursor. A `press` and a `hold` do not wait. A `hold` lasts until a `release`. The old lines `hold ACTION` and `pulse ACTION ON PERIOD` still work in a file that has no timeline command. A `pulse` line in a timeline runs from the start. A timeline starts at the first read of the file. When the text of the file changes, the timeline starts again. When it ends, the host prints `nxkit-input: scripted input: the timeline ended at frame N`.

Example script `to-the-shop.txt`:

```text
# Wait for the title screen, then start the game and walk to the shop.
wait frame 1800
press a
wait 2s
press a
wait frames 120
hold ls_up
wait 4s
release
stick l 1 0 1500
press a
```

### How the timing stays stable

There are two clocks.

- Frames. The count is the number of presents of the game since the start of the process. The game steps once for each frame. Thus a frame wait gives the game the same input in the same step. This holds also when the machine is slower or when the frame rate changes. A wait does not drift. A late poll runs every command that is due, each at its own frame.
- Wall clock. A wait on the wall clock ends at the time it was due, not at the time of the poll. Use it where no frame is presented, for example for a loading screen. A `press` that is shorter than the gap between two polls of the game is still shown once.

Limits:

- The time to load differs between runs. The frame count includes the frames of the loading screens. Thus a `wait frame N` before the scene can come at a different moment. Use `wait SECONDS` for a load.
- A script cannot see the game. A scene that depends on chance, on the time of day or on the speed of the load can differ. A recording replays the input, not the game.
- A game that does not present (a crash, the Home menu) stops every frame wait.
- A game polls the controller at its own pace. A recording stores the state that the game saw, so it cannot lose a button. A `press` that you write by hand with a very short time can fall between two polls. The service shows it once.
- Real controllers join a replay. Do not touch them during a replay.
- A recording stores player 1 only.

### Record and replay

`record` writes each change of the buttons or of a stick as one line, with the wait before it. The input is what the game read: the controllers, the keyboard, the mouse camera, the input file and the injected input, all merged. It skips the time that the Home menu holds the controllers. In `frames` timing the waits count frames. In `ms` timing they count milliseconds. The waits are totals, so the rounding does not add up. A recording in `frames` timing that starts after the first present begins with `wait frame N`. When no frame was presented yet, `frames` timing writes milliseconds. At the end `record stop` writes the wait up to the stop and a `release`.

Example of a recording:

```text
# nxkit input recording of player 1, timing frames, started at frame 0
wait frames 212
hold a
wait frames 4
release a
wait frames 31
stick l 0.00000 1.00000
wait frames 240
stick l 0.00000 0.00000
release
```

### Workflow: back to the scene after a new build

1. Start the game. Run `tools/nxkit-ctl record start /abs/path/scene.txt` as early as you can, before the game reads input. Then the frame count of the recording and the frame count of the replay agree.
2. Load the save by hand. Walk to the scene of the problem.
3. Stop the recording with `tools/nxkit-ctl record stop`.
4. Open the file. Replace the gaps that wait for a load with `wait SECONDS`. Delete the part that you do not want.
5. Change the host code. Build it. Start every new build with `NXKIT_INPUT_FILE=/abs/path/scene.txt`. The game then comes to the scene without a person. You can also start the replay later with `tools/nxkit-ctl replay /abs/path/scene.txt`.
6. At the scene, use the other verbs (`keys`, `log`). Move on with `press` and `stick`.
7. If the replay is off, record again from the point where it went wrong. Or put a `wait` before that point.
## Shader hot swap

You can change the MSL of a program while the game runs. The next frame draws the scene again with the new code. Use it to find a drawing bug and to test a fix. A fix that works goes into the translator (`nxb-xlate`) afterward. The verbs are on the control socket (W35, W37):

```bash
tools/nxkit-ctl shader list
```

```bash
tools/nxkit-ctl shader dump 0123456789abcdef
```

The override folder is `<diag_dir>/shaders` (`NXKIT_DIAG_DIR`, default `/tmp/fmap`). `shader status` prints it.

| verb | effect |
|---|---|
| `shader list [FILTER]` | Counts the next 3 frames. It prints a line `KEY STAGE DRAWS` for each program: the draws and dispatches for each frame. The stage is `vs`, `fs`, `ts`, `ms`, `gs` or `cs`. A program with a swap has a `*` at the end. `FILTER` keeps the keys that contain it. It fails when the game draws no frame in 5 s. |
| `shader dump KEY` | Writes the MSL of the program to `<override>/KEY.metal`. It does not replace a file that exists, so your edits stay. |
| `shader load KEY` | Compiles `<override>/KEY.metal` and puts it in place of the library of the program. It fails when the file does not compile or lacks a function that the host calls. The answer has the first error lines. The whole text of Metal is in `<override>/KEY.errors.txt`. A failed load changes nothing. |
| `shader reset KEY` or `shader reset all` | Goes back to the shipped function. The file stays in the override folder. |
| `shader status` | Prints the override folder and the programs that have a swap. |

To skip a program, use `tools/nxkit-ctl set NXKIT_SKIP_PROGRAMS=KEY`. That key is live too.

### Where the MSL comes from

The app has the compiled libraries only. The host cannot translate a shader again, because it does not contain the translator. So `dump` reads the source from `<run>/shader-src/<key>_b.metal`. A normal build does not write that folder. Build the game with `NXB_KEEP_MSL=1` to get it:

```bash
NXB_KEEP_MSL=1 nxb build ...
```

The sources about double the size of `<run>/shaders`. Without them, `dump` says so and names the tool `xlate-msl` of `nxb-xlatedev`. That tool translates a shader of a split package and writes its MSL. You can write `<override>/KEY.metal` by hand from its output.

With `NXKIT_PULL=1`, a vertex shader that has a `pull` field is drawn through its `_bp` library. `dump` and `load` then use the `_bp` source. The host uses one library for each program, so the source that you load replaces the library that the host uses now.

### Rules for the source

- Keep the names of the functions. The host calls `main0` and its variants (`main0_u`, `main0_m`, `main0_l`, `main0_z` and `main0_lz`) by name. `load` refuses a source that lacks one.
- Keep the bindings. The argument buffer, the buffer slots and the stage inputs are the contract with the encoder. Change the code inside the functions.
- Task and mesh shaders cannot be swapped yet. `load` refuses them.
- Metal compiles the source with its default options at run time. The build compiles it with its own flags. So `dump` followed by `load` without an edit can give a slightly different result. Compare an edit with that baseline.

### What happens in the host

A swapped program has its own pipelines. The host builds them when a draw needs them, on the encode thread, before it encodes the draw. The first frame after `load` can take longer, because one compile takes some milliseconds for each state of the program. The pipelines of the shipped library stay in the cache, so `reset` is fast.

A swapped pipeline is never saved. It does not go into the runtime pipeline cache, into the list of recorded states or into an archive. The next start of the game uses the shipped shaders again. Your files in the override folder stay, and you must `load` them again.

Without a swap, the cost is one atomic load for each frame and two integer compares for each draw. The picture of the game does not change.

## Archive coverage

The pipeline archive of the build holds one pipeline for each shader pair, with the state that the build assumes. This state is eight unspecialized colour attachments, no vertex layout, no primitive class, one sample, no alpha to coverage and no ICB support. At run time a pipeline is the pair and the state of the draw. An archive lookup finds only the state it holds, so a lookup with the formats and blend of a draw misses. The host therefore asks the archive once for the unspecialized pipeline of the pair (a *base*). It makes each draw's pipeline with `MTL4Compiler newRenderPipelineStateBySpecializationWithDescriptor:pipeline:`. This call fills in the colour attachments and costs a fraction of a compile. Only colour attachment state can be specialized. A draw with a vertex layout, several samples, alpha to coverage or ICB support needs a base of its own, which the archive does not have. With `NXKIT_PIPELINE_BASES=1` the host compiles such a base, and the base of a pair the archive lacks, once with its colour attachments unspecialized. Every colour state of it after that is a specialization too (a vertex function that writes the point size gets a base without a class, as a full compile does). A pipeline without a base is compiled from the functions. It compiles on a background thread and the draw waits (`skip.pending`).

The archive holds the bases the host asks for with `NXKIT_PULL=1`. For the vertex shader, this is its `_bp` library with the `entry` of its `pull` field when it has one (vertex pulling, `nxkit-gpu` `pull`). Else it is its `_b` library with its `entry`. For the pixel shader, it is its `_b` library with its `entry`. A pulled draw has no vertex layout, so the base of its pair serves every layout. The plan leaves a vertex shader with stage inputs and no `pull` field out of the plain bases (`left[2]` of `plan::plan`). The recorded states of the list add `_b` bases with a vertex layout (see [pipeline-stages.md](pipeline-stages.md#recorded-states)). Archives built before pulling hold `_b` bases only for vertex shaders without inputs, and older ones hold `main0_z` bases. With these archives, the pairs that pull now show as `archive named the pair but missed` or not at all, and the app needs a new archive. Without `NXKIT_PULL=1` (the default) the host asks for `_b` with the vertex layout of the draw. Only the recorded states of a list hold those, and the plain `_bp` bases then serve nothing.

`NXKIT_DIAG=pipe` prints every 60 frames how the background builds keep up:

```
DIAG pipe frame 3900: pending draws 48210 (total 214286), new jobs 85, failed 0 | queue 40, compiling 6 of 6 threads | cached 0 archive 0 specialized 70 (1.8/9.0 ms mean/max) compiled 15 (210.4/850.2 ms mean/max) | draws pulled 33012 stage_in 0 | bases from archive 31 (archive named the pair but missed 0) compiled 0 missing 2, specialized 410 refused 0
```

`pending draws` are the draws of the window that waited for their pipeline. `new jobs` are the pipelines queued in it, and `queue` and `compiling` are the jobs now. Per kind of creation it gives the count and the mean and longest time of a job in the window. `archive` is an exact archive hit. `specialized` is a specialization of a base (the time includes compiling the base when it was the first of it). `compiled` is a compile from the functions. `draws pulled` and `stage_in` count the vertex draws of the window prepared through the `_bp` library and with the stage_in layout. The stage_in layout is for a vertex shader without inputs, one that cannot pull, or every vertex draw without `NXKIT_PULL=1`. A draw that an ICB records counts once, when the ICB is built. With `NXKIT_PULL=1` and a current archive, `bases from archive` should cover nearly every pair. `bases` are totals: found in the archive, looked up in an archive that names the pair and missed, compiled, missing, the pipelines specialized, and the refusals of Metal with the first one. A miss in an archive that names the pair means that the archive holds other functions or another state than the host asks for. `missing` means not archived with compiling off, or refused.

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
| `mesh (not in the build's archive)` | a mesh pipeline. The build makes none | |

With shards, `pair not archived` also covers a pair that the index lacks and a shard that could not be opened (the host prints the error once).

The example of `pipeline.archive-miss` in the health report carries the same text with the shader entries and the compile time of the pipeline.

### The runtime pipeline cache lines

At the start the host prints `nxkit-gpu: runtime pipeline cache <folder>: N archives (M MiB), K pipelines listed`. With a list, it then prints `nxkit-gpu: compiling shaders: making K pipelines on T threads`. While it waits, it prints a line `pipeline prewarm progress: done of K in S s (...)` every 2 s. Then it prints `pipeline prewarm done: K of K in S s (archive a, specialized b, compiled c, cached d, compute e, failed f, skipped g)`. Read the fields as follows. `archive` is a load: an exact pipeline of the build's archive (a recorded state), of an `rt-` archive, or a compute pipeline of the shard. `specialized` is a specialization of an archived base, for a draw that no archive has. The start compiles instead, because a specialization is not kept. `compiled` is what the archives lacked and the start compiled once. Mesh pipelines are always compiled. The rest is zero in the second run with the same build, host, macOS and GPU. If it is not zero, the size limit cut the archives. `failed` is a pipeline that Metal refused. `skipped` is one whose library is not in `shaders/` (a library of the host's own source, or a build that changed). `continues in the background` means the wait ended first. With `NXKIT_DIAG=pipe`, `pending draws` in the first frames is the number of draws that still found no pipeline.
