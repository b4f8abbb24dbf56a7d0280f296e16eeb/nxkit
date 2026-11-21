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

The host pins the range of its no-copy GPU buffer (`pinned::set`). The GPU driver keeps using the pages it wired there, so inside that range `HostSpace::can_remap` is false: `MapMemory` copies instead of remapping, and `release` (heap shrink, `UnmapMemory`, `UnmapPhysicalMemory`) zeroes the pages in place and takes their access instead of mapping fresh pages. `UnmapSharedMemory` takes its view away with `HostSpace::drop_view`, which never writes to the pages of the service. `usage::get` reports the peak heap size, the peak of `MapPhysicalMemory`, and the highest ends reached in the alias and stack regions, so the host can say how much of the regions a title needs.

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

The table is in `crates/nxkit-hos/src/svc.rs`. It lists every svc number that a game can call and the state of each here. A number that the table does not answer returns false and the host goes on with its own handler. 0x20 SendSyncRequestLight is not answered. 0x32 SetThreadActivity and 0x33 GetThreadContext3 need `Config::threads` and are not answered without it.

## Pausing a thread

A garbage collector stops the world with `SetThreadActivity(thread, Paused)`, reads the registers of each thread with `GetThreadContext3` and resumes it with `SetThreadActivity(thread, Runnable)` (Unity's Boehm GC through `nn::os::SuspendThread`, Patrick's Parabox). The code is `pause.rs` and `thread/activity.rs`. A pause must not stop a thread while it holds a lock of the host, or the pausing thread stops at its next svc. So the kernel pauses a thread only where it holds none:

- In a svc. The host's gate calls `nxkit_svc_frame`, which calls `Kernel::gate_enter` with the gate's register frame before the svc and `Kernel::gate_leave` after it. A pause of a thread in a svc returns at once. The svc runs on, and at its end the thread waits on its state word until it is resumed. A thread that blocks in a svc is paused that way. A thread that exits in the svc does not wait, and its context is zero, as Horizon gives for a terminated thread.
- In guest code. The kernel stops the host thread with `thread_suspend` (`sys::HostThread`, a send right from `mach_thread_self`) and reads `ARM_THREAD_STATE64` and `ARM_NEON_STATE64`. When the pc is not in the guest's code (`ThreadControl::is_guest_code`: the `r-x` segments of the modules), the thread is in host code that the guest reached with a plain branch, such as an SDK take-over. The kernel lets it run again for a moment and tries again until it is in guest code or in a svc. While the thread is stopped there, the kernel allocates nothing and takes no lock. When the thread does not get there within `Config::pause_budget` (10 seconds), the process stops with the thread and its last pc.

The thread itself takes no lock on this path, only atomics on its state word (`IN_GATE`, `REQUESTED`, `READING`). The pausing thread holds the thread's activity lock, which only other pausing threads take. The results follow Mesosphere: an activity other than 0 and 1 is `InvalidEnumValue`, a handle that is not a thread `InvalidHandle`, the calling thread (also as the pseudo handle) `Busy`, a thread that is not started or has exited `InvalidState`, a second pause and a resume or context read of a thread that is not paused `InvalidState`, and an output that cannot be written `InvalidPointer`.

`GetThreadContext3` writes the 0x320 bytes of `svc::ThreadContext`: x0 to x28, fp, lr, sp, pc, pstate (the condition flags), v0 to v31, fpcr, fpsr and tpidr. For a thread in a svc, x0 to x18 are zero as on Horizon, x19 to x30 and the vector registers come from the gate frame, sp is the guest's sp before the gate block, and pc is the return point of the gate block in the patch area. For a suspended thread all registers come from Mach. tpidr is the TLS block of the kernel's model in both cases.

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
| `Config.threads` | the gate frame reader of `gate.rs` and the `r-x` segments of the modules (`NxkitKernelConfig::gate_context` and `code`) |

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
- The pc that `GetThreadContext3` gives for a thread in a svc. Horizon gives the address after the `svc`, the kernel gives the return point of the gate block.
- The guest `x18` of a suspended thread. The rewritten code keeps it in the `K18` slot, and Mach reports the host's `x18`.
- Code that the game loads at run time (NRO modules) is not in the guest's code ranges, so a pause of a thread that runs there does not end.
