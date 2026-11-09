# nxkit-host

`nxkit-host` is the main program of nxkit. It reads a run folder, puts the game's rewritten code into memory, answers the game's svc calls and runs the window. This page is the contract between the build stage (`nxb build`) and the host. The decisions are R18, R45 to R47c and R57 in [decisions.md](decisions.md#part-3-runtime-decisions).

## Command line

```text
nxkit-host [--fullscreen] <run folder>
nxkit-host probe <run folder>
```

The run folder is `Contents/Resources/nxkit/` of the app bundle. `probe` prints the measurements of R18 and starts no game.

## The run folder

### game.json

A number is a JSON number or a string in decimal or with a `0x` prefix. Every path is relative to the run folder. The host stops with the key name when a required key is missing or has the wrong type. It has no defaults, because the title module answers every value (R2).

| key | meaning |
|---|---|
| `title_id` | The title ID as a hex string. |
| `name` | The name of the game. |
| `version` | The game version as a string, for example `1.6.0`. |
| `version_number` | The version as the number of the update. |
| `modules` | The modules, in load order. |
| `services` | The enabled services, by lowercase name: `fs`, `input`, `audio`, `video`, `system`, `offline`, `pacing`, `kernel`, `gpu`. `kernel` is required. |
| `romfs` | The romfs folder. |
| `patches` | The patch data of the title module. It may be absent. The host does not apply it, because the build applied the patches (R2). |
| `main_thread` | `priority`, `core` and `stack_size` from the NPDM of the game. |
| `npdm` | The raw `main.npdm`. It may be absent. |
| `memory` | The memory profile of the game. See below. |
| `settings` | The run settings of the title module. See below. |
| `aoc` | The DLC packages. It may be absent, which means no DLC. See below. |
| `dylib` | The library with the module segments, `nxgame.dylib`. |
| `page_size` | The page size of the build, 16384. The host stops if it differs from its own. |
| `nvn_functions` | The NVN functions that the game imports. The GPU backend reads it. The loader ignores it. |

### Modules

| key | meaning |
|---|---|
| `name` | `rtld`, `main`, `subsdk0` and so on, and `sdk`. |
| `base` | The preferred guest address of offset 0. It is a multiple of 0x4000. dyld may slide the library, and the host moves the guest below `1 << 39`, so the real address can differ (R47a, R56). |
| `size` | The offset after the last byte of the highest segment, counted from `base`. A far layout has a patch segment below `base` that `size` does not count. |
| `low` | The offset of the first byte of the lowest segment. It is 0 or negative. |
| `span` | The memory the module maps, `size - low`. The host reserves `[base + low, base + low + span)` for the module, and it stops if `low` and `span` do not fit the segments. |
| `text`, `rodata`, `data` | `{ "offset", "size" }` relative to `base`. The offset of `data` is a multiple of 0x4000. |
| `bss_size` | The size of the bss. It starts at the end of `data`. |
| `image` | The rewritten flat image. Byte 0 belongs to `base`. The loader reads it for the data segments and the relocations. |
| `imports` | A JSON list of `{ "name", "slot", "addend", "weak" }`, one entry for each relocation of an undefined symbol in the image, in the order of the relocation tables. `slot` is the offset from `base` of an 8-byte slot in a writable segment. The host stores the address of the name plus `addend`. `addend` is 0 for a `JUMP_SLOT`. `weak` is `true` for a weak undefined symbol. The host stops if the image has another number of undefined relocations than the list has entries. |
| `exports` | A JSON list of `{ "name", "slot" }`. `slot` is the offset of the symbol. Only global and weak symbols that the module defines and that are not hidden are listed. |
| `entry` | Exactly one module has it, and that module is the main module. The host does not start there. The offset 0 of a module is its header (R57). |
| `key_page` | The offset of the key page. The page is 0x4000 bytes (`KEY_PAGE_OFF` of the rewriter). |
| `gates` | The gate list of the rewriter for this module (`<name>.gates.tsv`). |
| `segments` | The segments of the library for this module, lowest offset first. Each has `name`, `offset` (negative for the patch of a far layout), `size` (a multiple of 0x4000), `file_offset` and `file_size` (the bytes in `image`) and `prot` (`r-x` or `rw-`). |
| `patch` | `{ "offset", "size" }` of the patch area of the rewriter. It lies in the `__NX_<NAME>_PATCH` segment. |
| `build_id` | The build ID of the module as 64 hex digits. The host reads it and keeps it. |
| `fixups` | The path of `<name>.fixups.tsv`. It holds header comments of the rewriter. The host does not read the file, and it is not a relocation table. |

The relocations are not in `game.json`. The host reads `RELATIVE`, `RELR`, `ABS64`, `GLOB_DAT` and `JUMP_SLOT` from the dynamic section of the `image` file (through MOD0) and applies them at the real base. The build lists only what the image cannot say, the undefined symbols, in `imports`. A relocation of a symbol that the module defines binds to a service, else to the export of the first module that has the name, else to the module itself.

### nxgame.dylib

The Mach-O library of all modules. Each module has three segments: `__NX_<NAME>_TEXT` (r-x) at `base`, `__NX_<NAME>_DATA` (rw-) at `base + data.offset`, and `__NX_<NAME>_PATCH` (r-x) for the patch area. In a near layout the patch segment lies behind the data. In a far layout it lies below `base`. `<NAME>` is the module name in upper case. The library has a header segment (`__TEXT`, one page) below the first module segment and an `__LINKEDIT` of one page behind the highest. It has no symbols and no rebase information, so dyld only maps it and slides it. The header page lies below the start of the first module and is never part of the guest range. The host starts its own regions behind `__LINKEDIT` and reserves the rest of the range behind it. The pages inside the module range that no segment covers are mapped by the host read and write. In a near layout this includes the zero bytes between the key page and the patch area, which the rewriter aligns to 64K.

### The gate list

One row for each rewritten site, with four tab-separated columns: the return address, the kind, the argument and the original instruction word. The return address is hex with an optional `-` and an optional `0x`, relative to the real base of the module. The kind is `svc` (the argument is the svc number), `cntpct` or `cntvct` (the argument is the target register), `obs` or `spin`. Lines that start with `#` are comments.

### settings

The title module and its family module answer the settings through their `nxtitle` hooks. The build writes them here. The host types the keys that name a mechanism of the core and gives them to the part that uses them (see Seams). It does not use them itself.

Required keys: `args`, `env`, `launch_param`, `display_versions` (a list of `{ "build_id", "version" }`), `core_preempt`, `present_display`, `fps_target`, `fps_max`, `fps_hold`, `present_snap`, `pso_mtl4`, `enc_thread`, `screen_aspect`, `pad_layout`, `op_mode`, `perf_boost`, `tex_flush_upload`, `tex_dirty_upload`, `tex_dyn_slots`, `render_cond`, `vertex_stream_null_bind`, `vconv_range`, `gpu_ts_segment`, `draw_barriers_game`, `shader_key_skip` (`{ "offset", "length" }`) and `upscaler_keys`.

Optional keys: `shared_font`, `nvn_api` (`{ "major", "minor" }`), `optional_imports`, a list of import names that may stay 0 because the game checks them before it calls them, `shared_font_decode` (a flag, false when absent), `set_refresh` (a refresh rate in Hz that the display is switched to for the run, 0 when absent; `NXKIT_SET_REFRESH` wins), `fps_free` (a flag, false when absent: the game accepts any frame rate, see `docs/pacing.md`) and `sf_services` (a list of service names that the system service answers with zeroed output). Any other import that nothing provides stops the load.

Any other key goes unread, in order, to the service that the title module addressed. The core has no name for it. An engine setting is one example.

### aoc

A list of `{ "index", "title_id", "version", "path" }`. `title_id` is a hex string and `path` is `aoc/<index>`. The host passes the list to the fs service as `Config.aoc`.

### Other files

`romfs/`, `shaders/` and `pipelines/` (the pipeline archive shards and `index.nxpi`, or the single `pipelines.mtl4archive` of an older build) are read by the services and the GPU backend, not by the loader.

### input.json

The keyboard and mouse layout of player 1 (R63) can be changed in `input.json`. The host reads `~/Library/Application Support/nxkit/input.json` first and `input.json` of the run folder second, each if it exists. A later value wins. Each file has a top-level section and an optional `titles` object with a section per title ID, which follows the top-level one. A file or key that does not parse gives a warning on stderr and is skipped.

```json
{
  "keyboard": true,
  "mouse": { "sensitivity": 1.0, "invert_y": false, "deadzone": 0.12, "capture": true },
  "bind": { "f": "rstick", "mouse4": "zr", "c": "none" },
  "titles": { "01007EF00011E000": { "mouse": { "sensitivity": 1.5 } } }
}
```

| key | meaning |
|---|---|
| `keyboard` | Keyboard and mouse for player 1. Default `true`. |
| `mouse.sensitivity` | Scales the mouse speed, 0.05 to 20. At 1 a motion of 1500 points per second is a full stick. |
| `mouse.invert_y` | Mouse up turns the camera down. |
| `mouse.deadzone` | The stick value of the smallest mouse motion, 0 to 0.9, to get past the dead zone of the game. Default 0.12. |
| `mouse.capture` | The window may hold the mouse. `false` turns the mouse off for the game. |
| `bind` | `key: action` pairs, applied in key order. A pair replaces the action of that key. `none` removes it. |

Keys: `a` to `z`, `0` to `9`, `f1` to `f12`, `space`, `enter`, `tab`, `backspace`, `delete`, `escape`, `up`, `down`, `left`, `right`, `shift`, `ctrl`, `alt` (both sides; `lshift`, `rshift`, `lctrl`, `rctrl`, `lalt`, `ralt` for one), `capslock`, `minus`, `equal`, `lbracket`, `rbracket`, `backslash`, `semicolon`, `quote`, `grave`, `comma`, `period`, `slash`, `mouse_left`, `mouse_right`, `mouse_middle`, `mouse4`, `mouse5`, `wheel`, `wheel_up`, `wheel_down`. Actions: `a`, `b`, `x`, `y`, `l`, `r`, `zl`, `zr`, `plus`, `minus`, `lstick`, `rstick` (the stick clicks), `dpad_up`, `dpad_down`, `dpad_left`, `dpad_right`, `ls_up`, `ls_down`, `ls_left`, `ls_right`, `rs_up`, `rs_down`, `rs_left`, `rs_right`, `walk` (half deflection of the left stick) and `none`.

The default layout:

| input | Switch |
|---|---|
| W A S D | left stick, full |
| Alt + W A S D | left stick, half (walk) |
| mouse motion | right stick (camera) |
| E, J | A |
| Shift, K | B |
| Space, U | X |
| left mouse button, I | Y |
| Q | L |
| R | R |
| right mouse button, Z | ZL |
| wheel (either way), F | ZR |
| Ctrl, C | left stick click |
| middle mouse button, G | right stick click |
| Tab, Return | Plus |
| M | Minus |
| 1 2 3 4 | D-pad up, right, down, left |
| arrows | D-pad |

## Guest memory

One reservation holds all guest memory. It is the guest range of `nxkit_abi::guest`. The no-copy buffer of R3 covers the part behind the modules. In address order it holds:

1. The modules, at their real addresses. A far layout starts at the patch segment below `base`.
2. The thread blocks: 1024 blocks of 0x4000 bytes, one for each guest thread that runs at the same time. The kernel hands them out as the thread-local regions.
3. The shared memory area, 64 MiB, which backs the shared memory of the services.
4. The heap, aligned to 2 MiB.
5. The stack region, aligned to 2 MiB. The stack of the main thread is at its start, with a guard page at the low end.
6. The alias region, aligned to 2 MiB.

The reservation is mapped read and write, and the pages are not committed until they are touched. The GPU buffer is created before any page becomes `PROT_NONE`, because Metal refuses a no-copy buffer over such a page. The guard page of the main stack is set after that.

The GPU buffer covers the memory after the modules. It starts at the first thread block and ends with the alias region. The code of the modules is mapped from a file, and a residency set over file-mapped pages makes them unreadable (measured 2026-11-08). So the buffer holds no module page. The data and bss of the modules are not GPU visible. A guest address below the buffer gives an error that names the address and says that module memory is not GPU visible. The address map of the front end uses the host range of the buffer as its base. It is an open question whether a game puts an NVN memory pool into the data or bss of a module.

The whole reservation ends at or below `1 << 39`, the end of the address space that the kernel models. The host regions start behind the highest module. When dyld mapped the library, they start behind the `__LINKEDIT` segment, which lies behind the last module.

### The memory profile

Every key is required. Numbers are JSON numbers or strings with a decimal or `0x` value.

| key | meaning |
|---|---|
| `reserve` | The total that the game gets, as the Horizon memory profile counts it. The heap is `reserve - alias - stack - 0x10000000`. The host derives it, and the build writes no `heap` key (checked on 2026-11-07). |
| `alias` | Size of the alias region. The host does not widen it. |
| `stack` | Size of the stack region. `MapMemory` takes its destination from it. |
| `app_pool` | The application memory pool. `SetHeapSize` and `MapPhysicalMemory` fail beyond it. `GetInfo` reports it as total memory. |
| `mem_used` | The used memory that `GetInfo` reports beside the heap. |
| `sys_resource` | The system resource size of the NPDM. 0 means no virtual address memory. |
| `map_limit` | `true` makes `MapPhysicalMemory` fail past the application pool. |
| `stdalloc_cap` | The largest block that the standard allocator of the guest hands out. 0 means no cap. |
| `alloc_big_align` | Whether the allocator of the guest aligns big blocks to 2 MiB. |

The host gives these numbers and the region ranges to the kernel when it starts it (`sys::kernel_init`), together with the modules, the thread-local area, the shared memory area, the main stack and the main thread of the NPDM. `settings.core_preempt` turns the core rules of the kernel on.

## The loader

### Placement

1. The host reads every `image` file.
2. It calls `dlopen` on `nxgame.dylib` and reads the header and the load commands of the mapped image. It checks that every `__NX_<NAME>_*` segment has the offset, size and protection of `game.json` relative to the image.
3. dyld may slide the library. The host takes the slide, moves every module by it and reserves the rest of the guest range behind `__LINKEDIT` with an address hint. It maps read and write memory over the gaps between the segments. If the slid layout would end above `1 << 39`, the host skips this step and gives the reason in the log.
4. If dyld refuses the library, or the range behind it is not free, the host closes the library, prints the reason and maps the code itself. This is the file map. macOS arm64 does not run code in anonymous pages, even after `mprotect` to `r-x` (measured 2026-11-08). It runs code only from pages of a signed file. The host does these steps:
    1. It reads the load commands of `nxgame.dylib` and checks every `__NX_<NAME>_*` segment against `game.json` by offset, size and protection. The library must have an `LC_CODE_SIGNATURE`.
    2. It registers the signature once with `fcntl(F_ADDFILESIGS_RETURN)`.
    3. It reserves the whole range, read and write. It asks for the preferred address first if the range ends at or below `1 << 39`. Otherwise it asks for `0x70_0000_0000`, which is the only range that macOS gives below that limit. It takes the address only if the range ends at or below `1 << 39` and the slide is a multiple of 2 MiB.
    4. It moves every module by the slide, plans the layout again and prints `nxkit-host: the guest moved by <slide> to <base>`. If the system gives no such address, the host stops and names the range.
    5. It maps every `r-x` segment (the text and the patch area) from the library file with `MAP_FIXED` over the reservation, at the moved address. The bytes of the file are the file bytes of the segment. The rest of the segment stays read and write and zero.
    6. It copies each data segment from `image` with `file_offset` and `file_size`. The data stays read and write.

    The loader relocates for the moved bases, as for a slid library. All mappings exist before the GPU buffer is created, because a `MAP_FIXED` after that would detach the pages from the buffer. A missing or unsigned library is an error with a message. There is no other way to run the code.

Nothing writes into the code pages after the mapping. The loader refuses a relocation that targets a segment that is not writable.

A mismatch between the library and `game.json` is an error and never a reason to map the code in another way.

### Finishing

For each module:

1. The part of the bss that shares a page with file data is zeroed. The other bss pages come zeroed.
2. The key page is filled.
3. The relocations are applied. The NSO image holds module offsets in its pointers, so the host rewrites each pointer for the real base. It handles `R_AARCH64_RELATIVE`, `DT_RELR`, and `ABS64`, `GLOB_DAT` and `JUMP_SLOT` of defined symbols. A symbol that a service provides is taken from the service first (R14). An unknown relocation type is an error with its number. A relocation of an undefined symbol is left to the import list. The step is the same for the library and the copy, and it gives the same result when it runs again.
4. The import slots are filled. A name comes first from the functions of the enabled services (`nxkit_rt::entries()`), then from the exports of the modules. A name that nothing provides stops the host with the module and the name (R15), unless it is in `settings.optional_imports`. Then it gets 0 and a line on stderr.
5. The key page becomes read-only.

The relocation pass also reads `DT_INIT` from the dynamic section. A module without `DT_INIT` has none. The offset must lie in an `r-x` segment. The host keeps the real address of each one in module order.

Then the host checks the thread-specific data, reads the gate lists with the real bases and installs the gate table.

## Gates and threads

The rewritten code has two kinds of branch into the host. A rewritten `svc #n` branches to the svc gate. A rewritten `mrs tpidrro_el0` loads the thread-local region from a slot of the key page.

The key page has four 8-byte entries. Entry `i` is `8 * key` of a pthread key, so the rewritten code reads its value from the thread-specific data of the thread. The order is fixed: `K18`, `KTLS`, `KTLR`, `KGATE`.

| entry | value on a guest thread |
|---|---|
| `K18` | The saved value of `x18`. It starts as 0. |
| `KTLS` | The ELF thread-local block. The kernel makes it (`nxkit_kernel_thread_tls`). |
| `KTLR` | The Horizon thread-local region. The kernel makes it and its IPC buffer. |
| `KGATE` | The address of the gate stub of the thread. |

Each guest thread owns a slot `k` in a pool of 1024. The host takes a free slot with one atomic operation and holds no lock. Slot `k` is a 0x4000 byte block in the reservation, a gate stub and a gate stack of 1 MiB. The stub loads the top of the gate stack, saves the registers `x0` to `x30`, calls the kernel's svc table with the number and `x0` to `x7`, writes the results back and returns to the guest. The gate runs on its own stack, so a guest stack of a few KiB is enough.

A thread enters the guest through `nxkit_thread_enter` and leaves through ExitThread or by returning from its entry function. The main thread runs on a host thread named `guest-main`. It attaches to the kernel with the priority and the core of the NPDM and runs the boot sequence on the main stack (see Start of the game). The threads that the game creates run through `thread_entry` of `nxkit_kernel_init`.

### Big stack

A game can give a thread a small stack, and a service function that builds host paths needs more. A service crate lists such functions in `pub fn big_stack_exports()`. For these names the loader writes the address of a stub (`bigstack.rs`) into the import slot or the relocation. The stub switches a call from a guest stack to a host stack of 512 KiB that belongs to the thread, with a guard page. It keeps `x0` to `x8` and `d0` to `d7`, and switches back on return. A call from a host thread or from the host stack goes straight through. A listed function has at most eight integer and eight floating-point arguments and is not variadic. The pool has 512 stubs.

## The window

AppKit runs on the main thread. The host opens one window with a `CAMetalLayer` that has the pixels of the main display, so the game renders at the native resolution, for example 4K on a 4K display. The display link runs at the highest rate of the screen, 120 Hz where the screen has it. The game runs on its own threads. A window close, a lost application or ExitProcess sets a stop flag. The next display tick ends the loop. A watchdog ends the process after 5 seconds if the loop does not. `--fullscreen` starts in full screen.

The content view takes the key events, so AppKit plays no alert sound, and passes the mouse events to `sys/mouse.rs`. The window holds the mouse for the camera (R63): the cursor is hidden and does not move, and each motion goes to the input service. It takes the mouse when it becomes the key window while no controller is connected, and on a click, which does not reach the game. Escape, Cmd+Tab or any other loss of the key focus, and a controller that connects let go. The display tick follows the controllers.

## The frame path

`frames.rs` builds the frame path. `seams::frame_hooks` calls it with the run folder.

1. It makes the `GpuBackend` of `nxkit-gpu` over the guest memory. The shader folder is `<run folder>/shaders`. It holds `<key>_b.metallib`, `<key>_b.json` and `.tsv` files, one set per shader, with the 16 hex digit shader key as the name. The pipeline archive is `<run folder>/pipelines/` (one archive file for each build batch and `index.nxpi`, opened when the first pipeline in it is asked for, 32 open at most) or, in an app of an older build, `<run folder>/pipelines.mtl4archive` (read whole into memory), and is loaded when it exists or `pso_mtl4` is true. Refer to docs/shader-step.md, "The pipeline archive as shards". `enc_thread` false encodes on one thread, true picks the thread count from the cores. The render scale is 2.0.
2. It installs the backend with `nxkit_rt::nvn::install`. The settings are `nvn_api` and `shader_key_skip`. It calls `nxkit_rt::nvn::set_command_settings` with `vertex_stream_null_bind`.
3. It asks `choose_present_plan` for the plan of a 60 Hz game on a 120 Hz display and makes the `Pacer` of `nxkit-metal` with the backend as its tick sink.
4. The returned hooks run on each display tick. They call `nxkit_svc_pacing::on_display_tick`, which signals the vsync event of the game. They read `nxkit_svc_pacing::hold`. A hold above zero sets the source of the `Pacer` to `Hold { ticks_per_frame: hold * ticks_per_vsync }`. Then they pass the tick to the `Pacer`.

At most two frames are in flight on the GPU. The backend presents at the target presentation time of the tick. The game renders at the native size of the display. No title asks for MetalFX.

## Start of the game

The main guest thread does the work of `rtld` and `nn::init::Start` (R57). The sequence was checked on 2026-11-08. The thread calls each guest function with `gate::enter` on the top of the main stack. The call returns when the function returns. Each step prints `>>> <name>` on stderr.

| Step | Call |
| --- | --- |
| 1 | `_init_libc0()` |
| 2 | `nnosInitialize(handle, 0)`. `handle` is the kernel handle of the main thread, which `attach_main_thread` returns. |
| 3 | `_init_libc1()` |
| 4 | `nninitInitializeSdkModule()` |
| 5 | `nn::os::SetHostArgc(1)` and `nn::os::SetHostArgv(argv)`. `argv` is `{ "game", NULL }` in host memory, which is never freed. |
| 6 | `nninitStartup()` |
| 7 | `_init_libc2()` |
| 8 | The `DT_INIT` function of each module, in module order. A module without one is skipped. The log line is `>>> DT_INIT <module> +<offset>`. |
| 9 | `nnMain()`. The line `nnMain returned` follows its end. |

The host finds the guest functions in the export lists of the modules. A missing symbol stops the host before the window opens. The message names the symbol (R15). When `nnMain` returns, the host stops.

## Startup order

1. Read `game.json`.
2. Place the modules and call `nxkit_abi::guest::set_range`.
3. Start the kernel with the regions, the memory profile and the main thread. It registers itself with `nxkit_abi::kernel`.
4. Register the IPC server and start the services that `game.json` names.
5. Finish the load: relocations, imports and gates. Then set the symbol lookup of the game.
6. Open the window and the display link.
7. Start the main guest thread, which runs the boot sequence above, and run the loop.

## The probe

`nxkit-host probe <run folder>` runs these measurements and prints each result:

1. Whether each fixed range is free in a fresh process, and whether the whole layout ends below `1 << 39`.
2. Whether `MAP_FIXED` and `mach_vm_allocate` with `VM_FLAGS_FIXED` get each range.
3. The cost of a copy of the size of each module into anonymous memory, which is the cost of the data segments.
4. Where dyld puts the segments of `nxgame.dylib`, and its slide.
5. Whether `newBufferWithBytesNoCopy` accepts the memory behind the modules, made as the host makes it: read and write, below `1 << 39`, with the guard page set after the buffer.
6. How much wired memory the system has before and after step 5.

## Seams

`src/seams.rs` holds the places where the host calls another crate's API. Each is a function with one job. A seam that has no counterpart returns `HostError::Unwired` and stops the host at start (R15). A hook of a service crate that the host cannot answer yet stops the game with the name of the hook.

| function | job |
|---|---|
| `register_kernel` | Checks that `nxkit_kernel_init` registered the kernel with `nxkit_abi::kernel::set_kernel`. |
| `attach_main_thread` | Attaches the `guest-main` thread with the NPDM priority and core, which the kernel got at its start. |
| `big_stack_exports` | Collects the names of the big-stack entries of the enabled services from `nxkit_rt::entries()`. |
| `register_server` | Builds the IPC server from the `services()` of the enabled service crates and registers it with `set_server`. |
| `init_services` | Starts the kernel's OS facts and the enabled service crates with typed values of `game.json`: the romfs, the save and temp folders and the DLC folder for fs, the console facts and the font facts for system, the pad layout and `input.json` for input, the host seams for audio and video, and the step mode with the state page of the main module for pacing. It sets the symbol lookup of every crate. |
| `focus_changed` | Tells the `system` and `input` services about the focus of the window. |
| `mouse` | Passes the motion, buttons and wheel of the held mouse to the input service. |
| `mouse_mode` | When the window holds the mouse: never when `input.json` turns it off, on a click while a controller is connected, else whenever the window is key. |

### The symbol lookup

`guestsym.rs` holds the game's own symbols: every export of every module, by Switch mangled name, with the address of the SDK's definition. It is never the take-over export. The loader fills it after the load. The kernel, fs, system, audio and video each have their own seam for it, and all of them read this table.

### The hooks of the system service

`wire.rs` holds the adapters. System events bind and signal through the kernel. The guest threads of the audio service and of the video service come from `nxkit_hos::GuestThread`. The system hooks `text_input`, `nro_load`, `nro_unload` and `nro_symbol` stop the game with their name. `quit` asks the host to stop.

### The display tick

The hooks of the display link call `nxkit_svc_input::tick()` and `nxkit_svc_pacing::on_display_tick()` on each tick.
| `frame_hooks` | Gives the display link the hooks that encode and present a frame, with the pacing and encoder settings. |
| `frame_hooks` | Gives the display link the hooks that encode and present a frame. It calls `frames::install` with the run folder and the settings. |
