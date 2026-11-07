# nxkit-host

`nxkit-host` is the main program of nxkit. It reads a run folder, puts the game's rewritten code into memory, answers the game's svc calls and runs the window. This page is the contract between the build stage (`nxb build`) and the host. The decisions are R18 and R45 to R47c in [decisions.md](decisions.md#part-3-runtime-decisions).

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
| `base` | The preferred guest address of offset 0. It is a multiple of 0x4000. dyld may slide the library, so the real address can differ (R47a). |
| `size` | The offset after the last byte of the highest segment, counted from `base`. A far layout has a patch segment below `base` that `size` does not count. |
| `text`, `rodata`, `data` | `{ "offset", "size" }` relative to `base`. The offset of `data` is a multiple of 0x4000. |
| `bss_size` | The size of the bss. It starts at the end of `data`. |
| `image` | The rewritten flat image. Byte 0 belongs to `base`. The copy path and the relocation step read it. |
| `imports` | A JSON list of `{ "name", "slot" }`. `slot` is the offset from `base` of an 8-byte slot in a writable segment. |
| `exports` | The same form. `slot` is the offset of the symbol. |
| `entry` | The offset of the entry point. Exactly one module has it. |
| `key_page` | The offset of the key page. The page is 0x4000 bytes (`KEY_PAGE_OFF` of the rewriter). |
| `gates` | The gate list of the rewriter for this module (`<name>.gates.tsv`). |
| `segments` | The segments of the library for this module, lowest offset first. Each has `name`, `offset` (negative for the patch of a far layout), `size` (a multiple of 0x4000), `file_offset` and `file_size` (the bytes in `image`) and `prot` (`r-x` or `rw-`). |
| `patch` | `{ "offset", "size" }` of the patch area of the rewriter. It lies in the `__NX_<NAME>_PATCH` segment. |
| `build_id` | The build ID of the module as 64 hex digits. The host reads it and keeps it. |
| `fixups` | The path of `<name>.fixups.tsv`. It holds header comments of the rewriter. The host does not read the file, and it is not a relocation table. |

### nxgame.dylib

The Mach-O library of all modules. Each module has three segments: `__NX_<NAME>_TEXT` (r-x) at `base`, `__NX_<NAME>_DATA` (rw-) at `base + data.offset`, and `__NX_<NAME>_PATCH` (r-x) for the patch area. In a near layout the patch segment lies behind the data. In a far layout it lies below `base`. `<NAME>` is the module name in upper case. The library has a header segment below the first module segment and an empty `__LINKEDIT` behind the highest. It has no symbols and no rebase information. The pages that no segment covers are mapped by the host.

### The gate list

One row for each rewritten site, with four tab-separated columns: the return address, the kind, the argument and the original instruction word. The return address is hex with an optional `-` and an optional `0x`, relative to the real base of the module. The kind is `svc` (the argument is the svc number), `cntpct` or `cntvct` (the argument is the target register), `obs` or `spin`. Lines that start with `#` are comments.

### settings

The title module and its family module answer the settings through their `nxtitle` hooks. The build writes them here. The host types the keys that name a mechanism of the core and gives them to the part that uses them (see Seams). It does not use them itself.

Required keys: `args`, `env`, `launch_param`, `display_versions` (a list of `{ "build_id", "version" }`), `core_preempt`, `present_display`, `fps_target`, `fps_max`, `fps_hold`, `present_snap`, `pso_mtl4`, `enc_thread`, `screen_aspect`, `pad_layout`, `op_mode`, `perf_boost`, `tex_flush_upload`, `tex_dirty_upload`, `tex_dyn_slots`, `render_cond`, `vertex_stream_null_bind`, `vconv_range`, `gpu_ts_segment`, `draw_barriers_game`, `shader_key_skip` (`{ "offset", "length" }`) and `upscaler_keys`.

Optional keys: `shared_font`, `nvn_api` (`{ "major", "minor" }`) and `optional_imports`, a list of import names that may stay 0 because the game checks them before it calls them. Any other import that nothing provides stops the load.

Any other key goes unread, in order, to the service that the title module addressed. The core has no name for it. An engine setting is one example.

### aoc

A list of `{ "index", "title_id", "version", "path" }`. `title_id` is a hex string and `path` is `aoc/<index>`. The host passes the list to the fs service as `Config.aoc`.

### Other files

`romfs/`, `shaders/` and `pipelines.mtl4archive` are read by the services and the GPU backend, not by the loader.

## Guest memory

One reservation holds all guest memory. It is the guest range of `nxkit_abi::guest` and the no-copy buffer of R3. In address order it holds:

1. The modules, at their real addresses. A far layout starts at the patch segment below `base`.
2. The stack of the main thread, with a guard page at the low end.
3. The thread blocks: 1024 blocks of 0x4000 bytes, one for each guest thread that runs at the same time.
4. The heap, aligned to 2 MiB.
5. The stack region, aligned to 2 MiB.
6. The alias region, aligned to 2 MiB.

The host regions start behind the highest module. When dyld mapped the library, they start behind the `__LINKEDIT` segment, which lies behind the last module.

### The memory profile

Every key is required. Numbers are JSON numbers or strings with a decimal or `0x` value.

| key | meaning |
|---|---|
| `reserve` | The total that the game gets, as nxnative counts it. The heap is `reserve - alias - stack - 0x10000000`. |
| `alias` | Size of the alias region. The host does not widen it. |
| `stack` | Size of the stack region. `MapMemory` takes its destination from it. |
| `app_pool` | The application memory pool. `SetHeapSize` and `MapPhysicalMemory` fail beyond it. `GetInfo` reports it as total memory. |
| `mem_used` | The used memory that `GetInfo` reports beside the heap. |
| `sys_resource` | The system resource size of the NPDM. 0 means no virtual address memory. |
| `map_limit` | `true` makes `MapPhysicalMemory` fail past the application pool. |
| `stdalloc_cap` | The largest block that the standard allocator of the guest hands out. 0 means no cap. |
| `alloc_big_align` | Whether the allocator of the guest aligns big blocks to 2 MiB. |

The host gives these numbers and the region ranges to the kernel at start (`configure_kernel`).

## The loader

### Placement

1. The host reads every `image` file.
2. It calls `dlopen` on `nxgame.dylib` and reads the header and the load commands of the mapped image. It checks that every `__NX_<NAME>_*` segment has the offset, size and protection of `game.json` relative to the image.
3. dyld may slide the library. The host takes the slide, moves every module by it and reserves the rest of the guest range behind `__LINKEDIT` with an address hint. It maps `PROT_NONE` memory over the gaps between the segments.
4. If dyld refuses the library, or the range behind it is not free, the host closes the library, prints the reason and copies. It reserves the range at the preferred addresses and copies each segment from `image` with `file_offset` and `file_size`. The segment is `rw-` while it is written, and then `r-x` if it is code. This uses `mprotect` only. It has no `MAP_JIT` and signs nothing. A hardened runtime would need the entitlement `com.apple.security.cs.allow-unsigned-executable-memory`.

A mismatch between the library and `game.json` is an error and never a reason to copy.

### Finishing

For each module:

1. The part of the bss that shares a page with file data is zeroed. The other bss pages come zeroed.
2. The key page is filled.
3. The relocations are applied. The NSO image holds module offsets in its pointers, so the host rewrites each pointer for the real base. It handles `R_AARCH64_RELATIVE`, `DT_RELR`, and `ABS64`, `GLOB_DAT` and `JUMP_SLOT` of defined symbols. A symbol that a service provides is taken from the service first (R14). An unknown relocation type is an error with its number. A relocation of an undefined symbol is left to the import list. The step is the same for the library and the copy, and it gives the same result when it runs again.
4. The import slots are filled. A name comes first from the functions of the enabled services (`nxkit_rt::entries()`), then from the exports of the modules. A name that nothing provides stops the host with the module and the name (R15), unless it is in `settings.optional_imports`. Then it gets 0 and a line on stderr.
5. The key page becomes read-only.

Then the host checks the thread-specific data, reads the gate lists with the real bases and installs the gate table. The entry is the real base of the main module plus its `entry`.

## Gates and threads

The rewritten code has two kinds of branch into the host. A rewritten `svc #n` branches to the svc gate. A rewritten `mrs tpidrro_el0` loads the thread-local region from a slot of the key page.

The key page has four 8-byte entries. Entry `i` is `8 * key` of a pthread key, so the rewritten code reads its value from the thread-specific data of the thread. The order is fixed: `K18`, `KTLS`, `KTLR`, `KGATE`.

| entry | value on a guest thread |
|---|---|
| `K18` | The saved value of `x18`. It starts as 0. |
| `KTLS` | The ELF thread-local block, 8 KiB. |
| `KTLR` | The Horizon thread-local region, 0x200 bytes. The thread object follows at 0x200. |
| `KGATE` | The address of the gate stub of the thread. |

Each guest thread owns a slot `k` in a pool of 1024. The host takes a free slot with one atomic operation and holds no lock. Slot `k` is a 0x4000 byte block in the reservation, a gate stub and a gate stack of 1 MiB. The stub loads the top of the gate stack, saves the registers `x0` to `x30`, calls the kernel's svc table with the number and `x0` to `x7`, writes the results back and returns to the guest. The gate runs on its own stack, so a guest stack of a few KiB is enough.

A thread enters the guest through `nxkit_thread_enter` and leaves through ExitThread or by returning from its entry function. The main thread runs on a host thread named `guest-main`. It attaches to the kernel with the priority and the core of the NPDM and starts the module entry on the main stack with `x0 = 0` and `x1 = 0xFFFF8000`. The threads that the game creates run through `thread_entry` of `nxkit_kernel_init`.

### Big stack

A game can give a thread a small stack, and a service function that builds host paths needs more. A service crate lists such functions in `pub fn big_stack_exports()`. For these names the loader writes the address of a stub (`bigstack.rs`) into the import slot or the relocation. The stub switches a call from a guest stack to a host stack of 512 KiB that belongs to the thread, with a guard page. It keeps `x0` to `x8` and `d0` to `d7`, and switches back on return. A call from a host thread or from the host stack goes straight through. A listed function has at most eight integer and eight floating-point arguments and is not variadic. The pool has 512 stubs.

## The window

AppKit runs on the main thread. The host opens one window with a `CAMetalLayer` that has the pixels of the main display, so the game renders at the native resolution, for example 4K on a 4K display. The display link runs at the highest rate of the screen, 120 Hz where the screen has it. The game runs on its own threads. A window close, a lost application or ExitProcess sets a stop flag. The next display tick ends the loop. A watchdog ends the process after 5 seconds if the loop does not. `--fullscreen` starts in full screen.

## Startup order

1. Read `game.json`.
2. Place the modules and call `nxkit_abi::guest::set_range`.
3. Start the kernel and the services that `game.json` names.
4. Register the kernel and the IPC server with `nxkit_abi::kernel`.
5. Finish the load: relocations, imports and gates.
6. Open the window and the display link.
7. Start the main guest thread and run the loop.

## The probe

`nxkit-host probe <run folder>` runs these measurements and prints each result:

1. Whether each fixed range is free in a fresh process.
2. Whether `MAP_FIXED` and `mach_vm_allocate` with `VM_FLAGS_FIXED` get each range.
3. The cost of the copy: map, copy and `mprotect` for the size of each module.
4. Where dyld puts the segments of `nxgame.dylib`, and its slide.
5. Whether `newBufferWithBytesNoCopy` accepts the whole reservation.
6. How much wired memory the system has before and after step 5.

## Seams

`src/seams.rs` holds the places where the host calls another crate's API. Each is a function with one job. A seam that has no counterpart returns `HostError::Unwired` and stops the host at start (R15).

| function | job |
|---|---|
| `register_kernel` | Registers the kernel with `nxkit_abi::kernel::set_kernel`. |
| `configure_kernel` | Gives the kernel the heap, the stack region, the alias region, the memory profile and `settings.core_preempt`. |
| `attach_main_thread` | Attaches the `guest-main` thread with the NPDM priority and core. |
| `big_stack_exports` | Collects the names from `big_stack_exports()` of the service crates. |
| `register_server` | Builds the IPC server from all `services()` and registers it with `set_server`. |
| `init_services` | Calls the `init` of each enabled service crate with the settings, the aoc folders (`Config.aoc` of fs) and `settings.extra`. |
| `focus_changed` | Tells the `system` and `input` services about the focus of the window. |
| `frame_hooks` | Gives the display link the hooks that encode and present a frame, with the pacing and encoder settings. |
