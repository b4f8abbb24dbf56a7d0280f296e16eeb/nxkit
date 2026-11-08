# nxkit-hos

`nxkit-hos` is Horizon for the nxkit host. It has no global lock (R7). Each guest thread is one host thread. The kernel reads and writes guest words through the `GuestMemory` trait, so the logic runs in tests without a guest. `unsafe` code is in the module `sys` only. The decisions are R6 to R11, R18, R31, R32 and R33 in [decisions.md](decisions.md#part-3-runtime-decisions).

## What the kernel holds

- Handles: events, threads, sessions, shared memory, transfer memory and resource limits. The table has 16 shards.
- Threads, with a QoS class from their role and a lend table for lock owners.
- Waits: lock, condition variable and address waits in sharded tables, on one parker word per thread.
- With `Config::memory`: the block map of the reservation, the thread-local slots and the shared memory area.

## Memory model

The host makes the reservation and gives the kernel its base and size. The layout from the low address up:

- The front region: the modules, the thread-local area and the shared memory area. The host maps the last two readable and writable.
- The heap region. It takes what the other regions leave.
- The stack region.
- The alias region, at the end.

The map records the state, permission and attribute of each block and answers `QueryMemory`. `SetMemoryAttribute` takes the uncached bit (8) and the permission lock bit (0x10, R58). The kernel asks the host to change page protection through `HostSpace`. The attribute bits do not change the host pages. `sys::HostMemory` is the implementation over the Mac's virtual memory. `MapMemory` of host-page aligned ranges uses `mach_vm_remap`, so both addresses see the same bytes. Other ranges are copied, and `UnmapMemory` copies back.

A thread has one slot of 0x4000 bytes in the thread-local area. The first slot is the user exception context. A slot holds:

| offset | content |
|---|---|
| 0x0000 | TLS block, `tpidr_el0` |
| 0x2000 | thread-local region, `tpidrro_el0`. Its first 0x100 bytes are the IPC buffer. The thread id is at +0x110 and the thread object address at +0x1F8 |
| 0x2200 | thread object |

The host's thread entry reads both values with `nxkit_kernel_thread_tls` and sets the registers before guest code runs. A thread must not read its region after `ExitThread`, because the slot goes back to the pool.

## IPC

`ConnectToNamedPort` calls `nxkit_abi::kernel::server()` and keeps the session in a handle. `SendSyncRequest`, `SendSyncRequestWithUserBuffer` and `SendAsyncRequestWithUserBuffer` call `Session::request` with the IPC buffer of the thread or the buffer the game gives. `CloseHandle` of a session calls `Session::close`.

## The svc table

The table is in `crates/nxkit-hos/src/svc.rs`. It lists every svc number that a game can call and the state of each here. A number that the table does not answer returns false and the host goes on with its own handler. Two numbers are not done: 0x32 SetThreadActivity and 0x33 GetThreadContext3, because they need a signal that pauses a host thread. 0x20 SendSyncRequestLight is also not answered.

An unknown svc does not return success (R15).

## SDK take-overs

The host runs the SDK's own `nn::os` and replaces the kernel under it. Four functions of the kernel side replace SDK or libc functions. `nxkit_hos::exports()` lists them:

| name | function |
|---|---|
| `getentropy` | random bytes from the entropy stream |
| `_ZN2nn6crypto36GenerateCryptographicallyRandomBytesEPvm` | random bytes from the entropy stream, forced over the SDK |
| `getenv` | the environment from `OsConfig::game_env`, otherwise the SDK's own `getenv`. Forced over the SDK |
| `_ZN2nn3mem17StandardAllocator8AllocateEm` | calls the SDK's aligned `Allocate(Em, Em)` with 64 KB for sizes of 64 KB or more when `alloc_big_align` is set. Forced over the SDK |

The environment of a game profile is empty for most games, as Horizon gives a program no environment.

The forced names are in `nxkit_hos::forced_names()`. All references to such a name must go to the export. The host sets `set_guest_sym`, which finds the SDK's own definition of a name and never the export. A missing symbol stops the game with a message (R15).

`SystemEvent` binding (`bind_system_event`, `signal_bound_system_event` and `clear_system_event`) and `GuestThread::spawn` and `join` bind the system event and spawn a guest thread. A guest thread runs on the host through the SDK's own `CreateThread`, with a stack from the SDK's `malloc`.

`GetInfo` type 11 and the take-overs share one entropy stream. `entropy::set_deterministic(true)` gives a fixed stream.

### Not ported

There are no scheduling helpers for hand-off between threads, because the core gates keep no hand-off state. There is no guest clock or `objmap` either. The host passes the environment through `OsConfig`.

## What the host fills

| field | source |
|---|---|
| `MemoryConfig.base`, `size` | the reservation (R18) |
| `front_size`, `stack_size`, `alias_size` | the memory profile of `game.json` |
| `app_pool`, `used_memory`, `system_resource`, `map_limit`, `program_id` | the memory profile and the NPDM |
| `tls`, `shared` | areas inside the front region, readable and writable. `tls` is aligned to 0x4000 and holds at least two slots |
| `main_stack` | the stack of the main thread, inside the stack region |
| `modules` | the segments of each loaded module |
| `Config.main_thread` | `main_thread` of `game.json`: priority, core and stack size from the NPDM |
| `Config.process_exit`, `process_id` | the host's exit and the process id |
| `OsConfig.game_env`, `alloc_big_align` | the profile of the game |
| `set_guest_sym` | the lookup in the game's symbol table |

## Not verified

- `pthread_override_qos_class_end_np` from a thread other than the starter. Only the close of an exiting owner does it.
- `mach_vm_remap` from a source that has no access, followed by a raise.
- Whether Breath of the Wild needs a shared mapping address aligned to 16 KiB.
- The state checks of `SetMemoryPermission` and the error codes of the alias region.
- The values of the resource limits and the peak value.
- `FlushDataCache` as a checked no-op.
- The process id 0x80.
- The registers of `GetSystemTick`. Horizon returns the tick in x0. The kernel writes both x0 and x1.
- The clear mode values of a `SystemEvent` (0 manual, 1 auto).
- Calls of Switch SDK functions through `extern "C"` pointers with the Apple AAPCS64 convention.
- The thread names of Breath of the Wild. On 2026-11-07 no extracted main NSO was found.
