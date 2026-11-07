# nxkit-host

`nxkit-host` is the main program of nxkit. It reads a run folder, puts the game's rewritten code into memory, answers the game's svc calls and runs the window. This page is the contract between the build stage (`nxb build`) and the host. The decisions are R18, R45, R46 and R47 in [decisions.md](decisions.md#part-3-runtime-decisions).

## Command line

```text
nxkit-host [--fullscreen] <run folder>
nxkit-host probe <run folder>
```

The run folder is `Contents/Resources/nxkit/` of the app bundle. `probe` prints the measurements of R18 and starts no game.

## The run folder

### game.json

Every number is a JSON number or a string in decimal or with a `0x` prefix. Every path is relative to the run folder.

| key | meaning |
|---|---|
| `title_id` | The title ID as a hex string. |
| `version` | The game version. |
| `modules` | The modules, in load order. |
| `services` | The enabled services, by lowercase name: `fs`, `input`, `audio`, `video`, `system`, `offline`, `pacing`, `kernel`, `gpu`. `kernel` is required. |
| `romfs` | The romfs folder. |
| `patches` | The patch data of the title module. It may be absent. The host does not apply it, because the build applied the patches (R2). |
| `main_thread` | `priority`, `core` and `stack_size` from the NPDM of the game. |
| `npdm` | The raw `main.npdm`. It may be absent. |
| `memory` | The memory profile of the game. It may be absent. See below. |

Each module has these keys:

| key | meaning |
|---|---|
| `name` | `rtld`, `main`, `subsdk0` and so on, and `sdk`. |
| `base` | The fixed guest address. It is a multiple of 0x4000. |
| `size` | The size in bytes from `base`. It includes the bss, the key page and everything behind it. |
| `text`, `rodata`, `data` | `{ "offset", "size" }` relative to `base`. The offset of `data` is a multiple of 0x4000. |
| `bss_size` | The size of the bss. It starts at the end of `data`. |
| `image` | The rewritten flat image. Byte 0 belongs to `base`. The copy fallback reads it. |
| `imports` | A JSON list of `{ "name", "slot" }`. `slot` is the offset from `base` of an 8-byte slot in the data of the module. |
| `exports` | The same form. `slot` is the offset of the symbol. |
| `entry` | The offset of the entry point. Exactly one module has it. |
| `key_page` | The offset of the key page. The page is 0x4000 bytes (`KEY_PAGE_OFF` of the rewriter). It lies inside `size` and behind the bss. |
| `gates` | The gate list of the rewriter for this module (`<name>.gates.tsv`). |

### nxgame.dylib

The signed Mach-O library of all modules. Each module has the segments `__NX_<NAME>_TEXT` (r-x) at `base` and `__NX_<NAME>_DATA` (rw-) at `base + data.offset`. `<NAME>` is the module name in upper case. The data segment may end before the end of the module. The host maps the rest as anonymous memory.

### The gate list

One row for each rewritten site, with four tab-separated columns: the return address, the kind, the argument and the original instruction word. The return address is hex with an optional `-` and an optional `0x`, relative to `base`. The kind is `svc` (the argument is the svc number), `cntpct` or `cntvct` (the argument is the target register), `obs` or `spin`. Lines that start with `#` are comments.

### Other files

`romfs/`, `shaders/` and `pipelines.mtl4archive` are read by the services and the GPU backend, not by the loader.

## Guest memory

One reservation holds all guest memory. It is one `mmap` with no access and no commit, at the address of the lowest module. It is the guest range of `nxkit_abi::guest` and the no-copy buffer of R3. In address order it holds:

1. The modules, at their `base`.
2. The stack of the main thread, with a guard page at the low end.
3. The thread blocks: 1024 blocks of 0x4000 bytes, one for each guest thread that runs at the same time.
4. The heap, aligned to 2 MiB.
5. The stack region, aligned to 2 MiB.
6. The alias region, aligned to 2 MiB.

The module ranges have no access until the loader handles them.

### The memory profile

The keys of `memory` come from the per-game profile of nxnative (`mem_reserve`, `mem_alias`, `mem_stack`, `app_pool`, `mem_used`, `sys_resource`, `mem_map_limit`). Every key is optional. Numbers are JSON numbers or strings with a decimal or `0x` value.

| key | meaning | default |
|---|---|---|
| `heap` | Size of the heap region. | derived from `reserve`, else 0x1_4000_0000 |
| `reserve` | The total as nxnative counts it. The heap is `reserve - alias - stack - 0x10000000` when `heap` is absent. | none |
| `alias` | Size of the alias region. The host does not widen it. | 0x8000_0000 |
| `stack` | Size of the stack region. `MapMemory` takes its destination from it. | 0x1000_0000 |
| `app_pool` | The application memory pool. `SetHeapSize` and `MapPhysicalMemory` fail beyond it. `GetInfo` reports it as total memory. | 0xcd50_0000 |
| `mem_used` | The used memory that `GetInfo` reports beside the heap. | 0x400_0000 |
| `sys_resource` | The system resource size of the NPDM. 0 means no virtual address memory. | 0x200_0000 |
| `map_limit` | `true` makes `MapPhysicalMemory` fail past the application pool. | `false` |

The host gives these numbers and the region ranges to the kernel at start (`configure_kernel`). The kernel uses them for `GetInfo`, `SetHeapSize` and `MapMemory`.

## The loader

1. The loader gives each module range back to the system and calls `dlopen` on `nxgame.dylib`.
2. It reads the header of the loaded library and compares the address of each `__NX_<NAME>_TEXT` and `__NX_<NAME>_DATA` with `game.json`.
3. If every segment is at its address, the loader maps anonymous memory over the parts that dyld did not cover. If one is not, it closes the library, prints the module names to stderr and maps every module from its `image`. The text is then made r-x with `mprotect`. nxnative does the same. It has no `MAP_JIT` and signs nothing, which works while the process has no hardened runtime.
4. It zeroes the bss.
5. It fills the key page and makes it read-only.
6. It fills the import slots. A name comes first from the functions of the enabled services (`nxkit_rt::entries()`), then from the exports of the modules. A name that nothing provides stops the host with the module and the name (R15).
7. It reads the gate lists and installs the gate table.

## Gates and threads

The rewritten code has two kinds of branch into the host. A rewritten `svc #n` branches to the svc gate. A rewritten `mrs tpidrro_el0` loads the thread-local region from a slot of the key page.

The key page has four 8-byte entries. Entry `i` is `8 * key` of a pthread key, so the rewritten code reads its value from the thread-specific data of the thread. The order is fixed: `K18`, `KTLS`, `KTLR`, `KGATE`.

| entry | value on a guest thread |
|---|---|
| `K18` | The saved value of `x18`. It starts as 0. |
| `KTLS` | The ELF thread-local block, 8 KiB. |
| `KTLR` | The Horizon thread-local region, 0x200 bytes. The thread object follows at 0x200. |
| `KGATE` | The address of the gate stub of the thread. |

Each guest thread owns a slot `k` in a pool of 1024. Slot `k` is a 0x4000 byte block in the reservation, a gate stub and a gate stack of 1 MiB. The stub loads the top of the gate stack, saves the registers `x0` to `x30`, calls the kernel's svc table with the number and `x0` to `x7`, writes the results back and returns to the guest. The gate runs on its own stack, so a guest stack of a few KiB is enough.

A thread enters the guest through `nxkit_thread_enter` and leaves through ExitThread or by returning from its entry function. The main thread runs on a host thread named `guest-main`. It attaches to the kernel with the priority and the core of the NPDM and starts the module entry on the main stack with `x0 = 0` and `x1 = 0xFFFF8000`. The threads that the game creates run through `thread_entry` of `nxkit_kernel_init`.

## The window

AppKit runs on the main thread. The host opens one window with a `CAMetalLayer`, starts a display link at 60 Hz and runs the application loop. The game runs on its own threads. A window close, a lost application or ExitProcess sets a stop flag. The next display tick ends the loop. A watchdog ends the process after 5 seconds if the loop does not. `--fullscreen` starts in full screen.

## Startup order

1. Read `game.json`.
2. Reserve the guest memory and call `nxkit_abi::guest::set_range`.
3. Start the kernel and the services that `game.json` names.
4. Register the kernel and the IPC server with `nxkit_abi::kernel`.
5. Load the modules and fill the imports.
6. Open the window and the display link.
7. Start the main guest thread and run the loop.

## The probe

`nxkit-host probe <run folder>` runs these measurements and prints each result:

1. Whether each fixed range is free in a fresh process.
2. Whether `MAP_FIXED` and `mach_vm_allocate` with `VM_FLAGS_FIXED` get each range.
3. The cost of the copy fallback: map, copy and `mprotect` for the size of each module.
4. Where dyld puts the segments of `nxgame.dylib`.
5. Whether `newBufferWithBytesNoCopy` accepts the whole reservation.
6. How much wired memory the system has before and after step 5.

### Big stack

A game can give a thread a small stack, and a service function that builds host paths needs more. A service crate lists such functions in `pub fn big_stack_exports()`. For these names the loader writes the address of a stub (`bigstack.rs`) into the import slot. The stub switches a call from a guest stack to a host stack of 512 KiB that belongs to the thread, with a guard page. It keeps `x0` to `x8` and `d0` to `d7`, and switches back on return. A call from a host thread or from the host stack goes straight through. A listed function has at most eight integer and eight floating-point arguments and is not variadic. The pool has 512 stubs.

## Seams

`src/seams.rs` holds the places where the host calls another crate's API. Each is a function with one job. A seam that has no counterpart returns `HostError::Unwired` and stops the host at start (R15).

| function | job |
|---|---|
| `register_kernel` | Registers the kernel with `nxkit_abi::kernel::set_kernel`. |
| `configure_kernel` | Gives the kernel the heap, the stack region, the alias region and the memory profile. |
| `attach_main_thread` | Attaches the `guest-main` thread with the NPDM priority and core. |
| `big_stack_exports` | Collects the names from `big_stack_exports()` of the service crates. |
| `register_server` | Builds the IPC server from all `services()` and registers it with `set_server`. |
| `init_services` | Calls the `init` of each enabled service crate. |
| `focus_changed` | Tells the `system` and `input` services about the focus of the window. |
| `frame_hooks` | Gives the display link the hooks that encode and present a frame. |
