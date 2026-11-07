# The nxkit host: Breath of the Wild on nxkit alone

| | |
|---|---|
| Owner | v0lumehi |
| Game | The Legend of Zelda: Breath of the Wild (`01007EF00011E000`), update 1.9.0 (v1114112) |
| Platform | Mac first |
| Builds on | [runtime-hooks.md](runtime-hooks.md), part B |
| Status | H1 written, not yet run with the game |

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
| `nxkit-rt` | the guest ABI, the entry points the host calls, the only crate with `unsafe` besides `nxb-sys` |
| `nxkit-hos` | Horizon: kernel objects, the svc calls, handles, events, threads |
| `nxkit-svc-*` | one crate per service: `fs`, `input`, `audio`, `system`, `video`, `offline` |
| `nxkit-nvn` | the NVN front: the C functions, the objects in guest memory, the recording into a frame format |
| `nxkit-metal` | the Metal 4 backend: it runs a recorded frame |
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
- The work inside a function avoids copies. The romfs is mapped with `mmap`. The input thread writes into the shared memory the game reads. Audio goes through a ring buffer without locks. The svc gates call the kernel code directly.
- A function that nxkit does not replace runs in the game's SDK. Its IPC request reaches an IPC server in nxkit. The server counts each command. A command that is called often becomes a function of the fast path.
- Nothing falls back in silence. A command the IPC server does not know stops the game with its name, service and command ID.
- The game's code is not changed for this. The binding is the loader's work.

## Decisions

- The shader translator is `nxb-xlate`, nxkit's own.
- The rewrite rules are nxkit's own. The core and the title module hold them.
- `nxkit-host` owns the window. It uses AppKit and a `CAMetalLayer`.
- Saves have the same layout as on nxnative, so a save moves between the two. If this costs speed, the layout changes.
- Measurement is part of the host from the start: CPU and GPU time of every frame, and the count of every IPC command.

## Apple hardware

- Unified memory: the guest's memory is one `MTLBuffer` without copies. Textures and buffers of the game are views into it where Metal allows it.
- Tile-based GPU: `nxkit-nvn` records a whole frame before `nxkit-metal` encodes it. The backend merges passes, keeps attachments in tile memory and drops loads and stores nobody reads. Breath of the Wild loads and stores about 800 MB a frame on nxnative (game note, 2026-10-21).
- Metal 4 only. The pipelines are built ahead of time for the Apple GPU by `nxb`.
- Performance and efficiency cores: guest threads get QoS classes. The render and audio threads run at user-interactive QoS.
- Frame pacing follows `CADisplayLink` and the display's refresh rate, not a fake vsync.
- MetalFX does the upscaling.

## Modular builds

- `nxb build --platform mac` builds the game's parts once: the extract, the rewritten modules, the shader libraries and the pipeline archive. Each stage has a content key and is reused.
- The host is a separate program in the run folder. It reads the game's parts from there.
- While nxnative still runs services, `dmhost` loads `libnxkit_rt.dylib` with `dlopen`. `DM_NXKIT_SERVICES` picks the services for a test without a new build.
- To change nxkit, run `cargo build --release -p nxkit-rt` and start the game again. The host and the game's parts stay as they are.

## Steps

| step | result | done when |
|---|---|---|
| H1 | the seam and the service `offline` | Breath of the Wild runs with `offline` on nxkit, and the log shows its lookups going to nxkit |
| H2 | `fs`, `input`, `audio`, `system`, `video` on nxkit | each one ran a fixed route of the game with the same result as nxnative |
| H3 | `gpu` on nxkit for Breath of the Wild | the game's NVN function list is complete in nxkit, and the route runs with GPU time at or below nxnative |
| H4 | `pacing` on nxkit | the game holds 60 and 120 fps on a 120 Hz display at the right game speed |
| H5 | `kernel` and the `nxkit-host` program | the game starts from `nxkit-host` without `dmhost` |
| H6 | nxnative is off | `nxb build --platform mac` for Breath of the Wild has no nxnative stage, and the route runs |

The route is one save and one path through the game: the title screen, loading the save, the Great Plateau outdoors, a shrine, the menus and a cutscene.

## Sources

- nxnative is MIT. Its code can be ported into nxkit, which is GPL-3.0-only. `THIRD_PARTY.md` names the ported files and keeps the notice of seedds.
- mush8 (`c22dev/mush8`, Mario Kart 8 Deluxe) is GPL-3.0. Its code can be ported with its notice in `THIRD_PARTY.md`.
- seedds/nxnative#76 brings Breath of the Wild above 60 fps and above 1080p on nxnative. Its frame step is the reference for H4.

## Open questions

- How does guest memory map to host memory and to GPU addresses?
- How do guest threads map to host threads, and which QoS class does each get?
- Which language binds Metal: Rust through `objc2-metal`, or a thin Objective-C layer?

- Which frame format does `nxkit-nvn` record, and how much of a frame does it hold before `nxkit-metal` encodes it?
- How does iOS load the host later? iOS has no `dlopen` of unsigned code, so the library is linked there.
- Does Ocarina of Time move to the same host later? It is a family `agl` game on NVN2 with mesh shaders.
