# The nxkit host: Breath of the Wild on nxkit alone

| | |
|---|---|
| Owner | v0lumehi |
| Game | The Legend of Zelda: Breath of the Wild (`01007EF00011E000`), update 1.9.0 (v1114112) |
| Platform | Mac first |
| Builds on | [runtime-hooks.md](runtime-hooks.md), part B |
| Status | H1 (the `nxkit-host` program) written and aligned with the build output (third segment, slide, relocations, `settings`, `aoc`), not yet run with the game. The seams in `crates/nxkit-host/src/seams.rs` are open |

## Goal

- Breath of the Wild runs on the Mac with nxkit only. No code of nxnative runs, and there is no `dmhost`.
- The host is written for Apple silicon and Metal 4. It has no path for other GPUs or older Metal versions.
- A change to the host does not rebuild the game. A change to nxkit is a `cargo build` and a restart of the game.
- Each step is checked against nxnative with a run of the game, until nothing of nxnative is left.

## Structure

The host follows the layers of the workspace and the title layer.

### Crates

| crate | function |
|---|---|
| `nxkit-rt` | the guest ABI, the entry points the host calls, with `unsafe` code besides `nxb-sys`, `nxkit-metal` and the module `sys` of `nxkit-hos` |
| `nxkit-hos` | Horizon: kernel objects, the svc calls, handles, events, threads, the process memory (heap, stack, alias, shared and transfer memory, `QueryMemory`, `GetInfo`), IPC sessions and the thread-local areas. `nxkit-rt` exports `nxkit_svc`, which the host gates call, and the service `kernel` turns it on. The host fills `nxkit_kernel_config` with the reservation, the NPDM main thread and the memory profile, and implements the thread entry. Refer to [nxkit-hos.md](../nxkit-hos.md) |
| `nxkit-svc-*` | one crate per service: `fs`, `input`, `audio`, `system`, `video`, `offline` |
| `nxkit-nvn` | the NVN front: the objects in guest memory, the recording into a frame format, and the `Backend` trait that the GPU backend implements |
| `nxkit-rt` (module `nvn`) | the NVN C functions. A game gets them by name from `nvnBootstrapLoader`. The modules device, queue, sync, pool, buffer, window, event and program hold the objects without a recording. The modules texture, sampler, state and cmdbuf hold the texture, sampler, state and command buffer functions |
| `nxkit-hos` | Horizon: kernel objects, the svc calls, handles, events, threads. The crate exists with the thread model. `nxkit-rt` exports `nxkit_svc`, which the host gates call, and the service `kernel` turns it on |
| `nxkit-ipc` | the IPC server: HIPC and CMIF, domains, the service manager `sm:`, the count of every command, the stop on an unknown command |
| `nxkit-svc-*` | one crate per service: `fs`, `input`, `audio`, `system`, `video`, `offline`. Each has `exports()` and `services()`, and a service that needs the big stack has `big_stack_exports()`. `fs` and `offline` exist |
| `nxkit-hos` | Horizon: kernel objects, the svc calls, handles, events, threads. The crate exists with the thread model. `nxkit-rt` exports `nxkit_svc`, which the host gates call, and the service `kernel` turns it on |
| `nxkit-svc-*` | one crate per service: `fs`, `input`, `audio`, `system`, `video`, `offline`. `nxkit-svc-video` exists: it replaces `movie::Decoder` and the `nn::codec` video decoders and decodes VP9 with VideoToolbox |
| `nxkit-nvn` | the NVN front: the C functions, the objects in guest memory, the recording into a frame format |
| `nxkit-gpu` | the frame encoder: it turns a held `nxkit-nvn` frame into Metal 4 work with `nxkit-metal`. It has no `unsafe` code and no game knowledge. Refer to [nxkit-gpu.md](../nxkit-gpu.md) |
| `nxkit-metal` | the Metal 4 backend: it runs a recorded frame. It contains the `unsafe` calls into Metal and gives a safe API |
| `nxkit-host` | the program: loader, memory, threads, the gates of the rewritten code, the window |

### Core, family and title

- The core holds every service. It knows no game.
- The family `agl` holds facts about how the engine uses the host, for example which barriers it needs. The core reads these facts and has no code path for one family.
- The title module holds what is true for Breath of the Wild only, and the choice of `Runtime::Nxkit` for each service.

### The GPU is chosen for a whole game

- A game gets each NVN function by name through `nvnBootstrapLoader` and `nvnDeviceGetProcAddress`. In nxnative both end in `nvn_lookup(name)` in `dm/gfx/nvn/hle_nvn.c`.
- Device, queue, memory pools and textures are shared state. An object of nxkit cannot go to a function of nxnative. So `gpu` moves for a whole game, not one function at a time.
- The title module lists the NVN functions the game fetches. A run of the game on nxnative records the list. The build moves `gpu` to nxkit only when nxkit has every function on the list.

### How a service answers the game

A service answers at the level of the SDK functions, and only falls back to Horizon's IPC.

- The loader writes the address of the nxkit function into the import table of the game's module. A call of the game is a plain branch, with no lookup at call time.
- The work inside a function avoids copies. The romfs is mapped with `mmap`. Input is read from the controller when the game calls the function, with no thread and no queue (R39). Audio goes through a ring buffer without locks. The svc gates call the kernel code directly.
- A function that nxkit does not replace runs in the game's SDK. Its IPC request reaches an IPC server in nxkit. The server counts each command. A command that is called often becomes a function of the fast path.
- Nothing falls back in silence. A command the IPC server does not know stops the game with its name, service and command ID.
- The game's code is not changed for this. The binding is the loader's work.

## Decisions

The decisions of the host and their causes are R1 to R21 and R48 to R50 in [decisions.md](../decisions.md#part-3-runtime-decisions).
The decisions of the host and their causes are R1 to R21 and R45 to R47 in [decisions.md](../decisions.md#part-3-runtime-decisions).

### The nxkit-host program

`crates/nxkit-host` is the program that starts the game. [nxkit-host.md](../nxkit-host.md) is its contract with the build stage.

- It reserves one range of address space for the modules, the main stack, the thread blocks, the heap, the stack region and the alias region. The range is also the no-copy buffer of R3 (R45).
- It loads `nxgame.dylib` at the fixed addresses, with a copy fallback, and fills the import slots from the service crates (R45).
- It holds the svc gate, the TLS slots and the big-stack entry in assembly (R46).
- It runs AppKit on the main thread and the game on its own thread (R47).
- `nxkit-host probe <run folder>` measures what R18 asks for: the free addresses, `MAP_FIXED` against `mach_vm_allocate`, the cost of the copy fallback and the no-copy buffer over the reservation.
- A part that another crate provides is a seam in `src/seams.rs`. The integration step fills the seams. A seam that is not filled stops the start with its name.

### Core rules act at svc boundaries only

The core rules of Horizon are off by default (R9). When a title turns them on, `nxkit-hos` holds a thread back only where it enters or leaves a svc, for example when it wakes from a wait or starts. A thread that runs guest code without a svc cannot be stopped, because macOS gives a process no way to suspend or preempt its own threads at a chosen point. The rules therefore order the threads that block and wake, and they do not replace the scheduler of the Switch. The gate of a core waits on one word and does not poll.

## Apple hardware

- Unified memory: the guest's memory is one `MTLBuffer` without copies. Textures and buffers of the game are views into it where Metal allows it.
- Tile-based GPU: `nxkit-nvn` records a whole frame before `nxkit-metal` encodes it. The backend merges passes, keeps attachments in tile memory and drops loads and stores nobody reads. Breath of the Wild loads and stores about 800 MB a frame on nxnative (game note, 2026-10-21).
- Metal 4 only. The pipelines are built ahead of time for the Apple GPU by `nxb`.
- Performance and efficiency cores: guest threads get QoS classes. The render and audio threads run at user-interactive QoS.
- Frame pacing follows `CAMetalDisplayLink` at 120 Hz, not a fake vsync (R19).
- The host renders at 4K itself. MetalFX upscales heavy games only (R19).

## Modular builds

- `nxb build --platform mac` builds the game's parts once: the extract, the rewritten modules, the shader libraries and the pipeline archive. Each stage has a content key and is reused.
- The host is a separate program in the run folder. It reads the game's parts from there.
- A game with a title module builds in seven stages: extract, romfs, rewrite, manifest, link, shaders and bundle. The manifest stage writes `game.json` from the NPDM and the title module. The link stage writes `nxgame.dylib` as a Mach-O file with each module at its fixed address and signs it ad hoc (R48). The stages need no nxnative and no `--engine` (R49). DLC goes into `aoc/<index>/` (R50).
- While nxnative still runs services, `dmhost` loads `libnxkit_rt.dylib` with `dlopen`. `DM_NXKIT_SERVICES` picks the services for a test without a new build.
- To change nxkit, run `cargo build --release -p nxkit-rt` and start the game again. The host and the game's parts stay as they are.

## Steps

| step | result | done when |
|---|---|---|
| H1 | the seam and the service `offline` | Breath of the Wild runs with `offline` on nxkit, and the log shows its lookups going to nxkit |
| H2 | `fs`, `input`, `audio`, `system`, `video` on nxkit | each one ran a fixed route of the game with the same result as nxnative. `fs` has the `nn::fs` functions of nxnative at SDK level and `fsp-srv` over `nxkit-ipc` |
| H2 | `fs`, `input`, `audio`, `system`, `video` on nxkit | each one ran a fixed route of the game with the same result as nxnative. For `video` the route plays a cut scene of Breath of the Wild (VP9, 1080p, 30 fps) with VideoToolbox (R43, R44) |
| H3 | `gpu` on nxkit for Breath of the Wild | the game's NVN function list is complete in nxkit, and the route runs with GPU time at or below nxnative |
| H4 | `pacing` on nxkit | the game holds 60 and 120 fps on a 120 Hz display at the right game speed |
| H5 | `kernel` and the `nxkit-host` program | the game starts from `nxkit-host` without `dmhost` |
| H6 | nxnative is off | `nxb build --platform mac` for Breath of the Wild has no nxnative stage, and the route runs. The build side is in `nxb-build` (`standalone/`, R48 to R50). The exact steps are in [build-botw.md](../build-botw.md) |

The part of H2 for `audio` exists in `nxkit-svc-audio` (R41, R42). It is written, but nobody has compiled or run it. The host must call
`nxkit_audio_init` with its callbacks, and ask `nxkit_hle_find_big` for the functions that need a big stack. `nxgame-botw` still
runs `offline` only, and its test `nxkit_runs_only_offline` changes when `audio` moves to nxkit.

The part of H3 that does not need the frame format exists in `nxkit-metal`:

- The device, the Metal 4 command queue, two command allocators for the frames in flight and a residency set.
- Guest memory as one no-copy `MTLBuffer`, with the mapping from a host address to a GPU address.
- Shader libraries from a `.metallib` file or from source, pipeline states from `MTL4Compiler`, and a pipeline cache under a hash from the caller, with an optional binary archive on disk.
- A `CAMetalLayer` drawable path, and a `CAMetalDisplayLink` driver at 60 or 120 Hz that calls `FrameHooks::display_tick`. The window belongs to `nxkit-host`.
- CPU encode time and GPU time of every command buffer.
- Full render pipeline state: blend per attachment, depth and stencil, vertex layout, topology, cull, winding, fill, depth bias and sample count.
- Textures with mip levels, array layers, 3D and cube kinds and views.
- Block-linear deswizzle and swizzle compute kernels for mips, layers and depth slices, and copy-back of a sub-rectangle. The layout rules follow deko3d.
- A public residency API for resources made outside the crate.
- A barrier API with explicit stages, for use between passes, inside a pass and towards later passes.
- Indirect command buffers with per-state inherit flags. The module docs of `icb.rs` list what an ICB can hold on the Xcode 27.1 SDK.
- A data-driven encoding API for render passes and compute passes. The frame encoder will call it from a loop over the recorded commands.

The part of H4 that exists (R28 to R30):

- `nxkit-metal` has the `Pacer`, which turns every display link tick into a decision (present a new frame with `presentAtTime`, hold the last frame, or make an interpolated frame). It has the present plan policy, the render scale policy, `SpatialUpscaler` and `FrameInterpolator` over MetalFX, and `PacingStats` (present time, missed ticks, frame interval histogram).
- `nxkit-svc-pacing` exports `nn::vi::GetDisplayVsyncEvent` and signals it from the display ticks at the game's vsync rate. It owns the frame step state page and the variable step.
- `nxgame-botw` carries the frame step, cut, render scale, render pool and output size rules as data, and its `pacing()` hook says `Display`.
- `nxkit-svc-pacing` has the load-driven step of nxnative (`StepMode::Load`, R30a), the mechanism of `hle_fpslock.c` for engines with their own limiter (`Limits`, R30b, fed by the `limits` hook of title and family modules) and the present interval rules. The host calls `frame_presented` once for each present and reads `hold()`, `present_vsyncs` and `deadline_vsyncs`.
- `nxtitle` has `pace_profile`, and `nxgame-botw` sets `fps_target 60`, `fps_hold on`, `present_snap on` and `present_interval game` (R30c).

What is still open for H4: the host that creates the `Pacer`, builds the `PresentSample` (GPU time of the frame, present snapshots, thermal state, performance counters) and calls the service, the map from `hold` to ticks of the link in the `Pacer`, the encoder that applies the render scale, a motion and depth source for the interpolator, the `nxkit-abi` function that looks up a guest symbol, and a run on a 120 Hz display.

What is still open: the frame format from `nxkit-nvn`, the encoder that reads it, compute ICBs, and the tuning of frame latency on real hardware.
The encoder that reads the frame format of `nxkit-nvn` is the crate `nxkit-gpu`. Compute ICBs and indirect execution ranges are wrapped in `nxkit-metal` (`compute_icb.rs`).

What is still open: the recorder that fills `FrameDetail` and calls `Gpu::encode_frame` from the NVN functions, the first build and run of `nxkit-gpu`, and the tuning of frame latency on real hardware.

The route is one save and one path through the game: the title screen, loading the save, the Great Plateau outdoors, a shrine, the menus and a cutscene.

## Sources

- nxnative is MIT. Its code can be ported into nxkit, which is GPL-3.0-only. `THIRD_PARTY.md` names the ported files and keeps the notice of seedds.
- mush8 (`c22dev/mush8`, Mario Kart 8 Deluxe) is GPL-3.0. Its code can be ported with its notice in `THIRD_PARTY.md`.
- seedds/nxnative#76 brings Breath of the Wild above 60 fps and above 1080p on nxnative. Its frame step is the reference for H4.

## Open questions

- How does iOS load the host later? iOS has no `dlopen` of unsigned code, so the library is linked there.
- Does Ocarina of Time move to the same host later? It is a family `agl` game on NVN2 with mesh shaders.
