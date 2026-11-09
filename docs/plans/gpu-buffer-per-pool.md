# One GPU buffer per memory pool: the GPU wires only the memory it can reach

Status: plan, 2026-11-09. Nothing is built. The code references are to `feat/nxkit-rt` at `b9987b0`. Line numbers
drift; the function names do not.

## Recommendation

1. **Give the game virtual GPU addresses.** The GPU address of a guest byte becomes `GUEST_VA_BASE + (host -
   host_base)`, with a fixed base that Metal never hands out (`0xC0_0000_0000`). It is no longer a Metal address. Every
   place that hands an address to Metal, or writes one into memory that a shader dereferences, resolves it first.
2. **Make one no-copy `MTLBuffer` per NVN memory pool** at `nvnMemoryPoolInitialize`, over the pool rounded out to
   16 KiB pages, and drop it when the pool is finalized and nothing in flight or cached names it any more. Memory
   outside pools stays plain anonymous memory. Only pool pages are wired.
3. **Resolve against an immutable pool snapshot** that the backend takes at the flush and hands to the encoder with the
   frame. The encode threads search it without a lock. The batch keeps the snapshot alive until the GPU is done.
4. **Pin only live pool buffers in the kernel.** `nxkit_hos::pinned` becomes a counted set of ranges. Outside them
   the kernel gives pages back again and `MapMemory` shares pages again.
5. **Keep the single buffer as a mode of the same code** (`NXKIT_GPU_BUFFER=whole`): the whole range is then one
   "pool" of the snapshot. Both modes share every resolve site, so the switch costs a few lines and gives A/B runs.
6. **Land it in two steps.** First every site resolves while the virtual base still equals the Metal address of the
   single buffer (no change in behaviour). Then the base moves to the fixed window, and a missed site faults loudly.
   Only then is per-pool mode turned on.

The direction stands or falls with two measurements, both cheap (package P0): that a resident no-copy buffer wires
only its own pages, and that two no-copy buffers over the same page are coherent for the CPU and the GPU. The other
open number is how much of the guest range the pools cover: the `nxkit-mem pools` line (`pages`) gives it.

## Why

Measured (docs/nxkit-host.md, "What the GPU buffer costs"): the driver wires every page of the resident guest buffer
from the start (mapped 12726, resident 12634, dirty 12634, wired 12626 MiB at t=0 for Ocarina of Time). With a
footprint of 10.9 GiB the process needs about 23.5 GiB and a 32 GB Mac swaps for the whole run. The game used at most
6.3 GiB of its 8 GiB heap. A smaller split only moves the limit. Everything the GPU can be asked to read or write lies
in an NVN memory pool, so the pools are the right unit.

## A. Where guest addresses and the guest buffer reach the GPU

Classes: (1) passed to a Metal API; (2) written into GPU-visible data that a shader dereferences without the gmem
table; (2v) written into GPU-visible data that a shader only compares or adds to other guest addresses, so it must
**stay virtual**; (3) kept across frames; (4) CPU only.

The rule behind 2 and 2v: a shader dereferences only pointers that the host gives it (argument words, the pull block,
the indirect jobs, the deswizzle parameters, the gmem delta). Values that come from guest memory (a UBO, c0 as NVN
fills it) are guest addresses; the translated code only subtracts or compares them (`gts_fn::multi_target`: offset =
address minus the base from the constant buffer, then a load through the bound storage buffer). A real pointer in such
a place would break that arithmetic.

### Class 1: passed to a Metal API

Metal side. These take whatever the GPU crate gives. They need no change, only the tripwire (B.1).

| site | what |
|---|---|
| `nxkit-metal/src/table.rs:63-69` `ArgumentTable::set_buffer` | `setAddress_atIndex` for every `SetBuffer`, `SetFragmentBuffer`, `SetObjectBuffer` |
| `nxkit-metal/src/render/pass.rs:301-318` | the three buffer bind commands |
| `render/pass.rs:461` `DrawIndexed` | `index_addr` as the index buffer address |
| `render/pass.rs:482` `DrawIndirect`, `:504` `DrawIndexedIndirect`, `:537` `DrawMeshThreadgroupsIndirect` | `indirect_addr` and `index_addr` |
| `nxkit-metal/src/compute.rs:220` `ComputeCommand::SetBuffer` | `setAddress_atIndex` |
| `compute.rs:349-378` → `compute_copy.rs:86-94` `guest_texture_box`, `:165-194` `copy_to_guest` | blits with `guest.buffer()` and an offset |
| `nxkit-metal/src/icb.rs:313-337` | `setVertexBuffer`/`setFragmentBuffer` with `IcbBuffer.guest.buffer()` and offset |
| `icb.rs:434-454` | indexed ICB draw with `indices.guest.buffer()` |
| `nxkit-metal/src/compute_icb.rs:216-230` | same for compute ICBs (no user in `nxkit-gpu` today) |

GPU crate side. These produce the guest addresses and must resolve them.

| site | what | after the change |
|---|---|---|
| `nxkit-gpu/src/draws.rs:243-258` `prepare` | vertex streams: `streams.push((slot, s.address))`, bound by `encode/emit.rs:61-72` | resolve `s.address`; the start must lie in a pool, the size is clamped to the pool's end |
| `draws.rs:425-450` `geometry`, `Indexed` | index buffer address, bound by `emit.rs:225-238` | resolve `count * width` bytes exactly |
| `draws.rs:452-461` `ArraysIndirect` | `indirect` bound by `emit.rs:184-191` | resolve 16 bytes |
| `draws.rs:462-480` `IndexedIndirect` and `index_room` (`:540-545`) | index and indirect address, `emit.rs:196-207`; `index_room` gives the bytes to the end of guest memory | resolve both; the room ends at the end of the pool |
| `draws.rs:481-500` `MultiIndirectCount` | index address, `emit.rs:149-175` | resolve, room to the pool's end |
| `draws.rs:529-537` `indirect_args`, `:104-108` `Shared::guest_offset` | the range checks of all of the above | become "inside one pool" |
| `icbs.rs:212-216` `slot_commands` `buffer` closure; `:246-249` streams; `:282` indices | `IcbBuffer { guest, offset }` | `IcbBuffer { guest: pool buffer, offset in it }` |
| `passes/copy.rs:222-250` `copy_buffer` | `CopyGuestToTexture` / `CopyTextureRegionToGuest` with `sh.guest` | pool buffer and offset; the range must lie in one pool |
| `upload/mod.rs:85-90` `guest_offset(rec)`, used at `:200` (deswizzle), `:314-330` (linear copy), `:351` (copy-back swizzle), `:428-450` (`copy_back_one`) | texture storage read or written on the GPU | resolve `rec.address .. rec.end()` to a pool buffer and offset |
| `upload/buffered.rs:50-145` | deswizzle into a staging buffer (`encode_to_buffer`, `encode_level_to_buffer`) | same |
| `gpu/frame.rs:329` `upload_scaled`, `:357` `upload`; `gpu/handles.rs:342` `copy_back_many`; `passes/mod.rs:166` `copy_back` | pass `&self.guest` / `sh.guest` down | pass the snapshot |

`nxkit-metal/src/deswizzle.rs` (`dispatch` `:180`, `encode_to_buffer` `:364`, `encode_level_to_buffer` `:394`) and
`compute_copy.rs` take an `Arc<GuestMemory>` and an offset from its start. Given a pool's buffer and the offset in it,
they work unchanged.

### Class 2: dereferenced by a shader, must become real

| site | what |
|---|---|
| `bindless/args.rs:198-205` with `:219-222` (`Cbuf`) and `:248-251` (`UboPtr`) | uniform buffer address `u.address` (when there is no snapshot) as an argument word |
| `bindless/args.rs:263-273` (`Sbuf`) | storage buffer address `s.address` as an argument word |
| `pull/mod.rs:66-98` `layout` | `dm_vlayout` stream base pointers (`s.address & !3`) that `dm_fetch.metal` reads through `device const` pointers |
| `indirect.rs:82-200` `jobs_of`, written at `:231-232` | `rec` and `cnt` of each job: the `nxkit_indirect` kernel loads through them |
| `nxkit-metal/src/deswizzle.rs:180-181`, `:364`, `:394` → `:420-441` | `KernelParams.base`: real already when the `GuestMemory` is the pool's buffer |
| `gpu/frame.rs:103-107` with `gmem.rs:72-110` `build_table` | the gmem delta: `real - virtual` per pool. Today 0 |

ICB arenas (`icbs.rs:178-200`) hold argument buffers built by `fill_args`, so they hold class 2 words too (see class 3).

### Class 2v: GPU-visible, but must stay virtual

| site | what |
|---|---|
| `bindless/args.rs:67-73` `drv_block` | compute c0 table at `C0_CS_UBO`: address and size of each uniform slot. Today it gets the same number as the argument word. It must get the guest address; the `UboPtr` argument carries the real one |
| `mesh.rs:43-60` `storage_table` | c0 storage buffer rows of the task and mesh stages (`C0_SSBO_TS`, `C0_SSBO_MS`). The shader takes offsets against them (`multi_target`) and loads through the `Sbuf` argument |

Open point, to settle with a test in `nxb-xlate` (D.5): that no emitted code dereferences a c0 table value itself.

### Class 3: kept across frames

| site | what | after the change |
|---|---|---|
| `icbs.rs:31-36` `Entry`, `:135-176` `get`, `:178-200` `build`; held in `gpu/mod.rs:118` | the ICB, its arena with resolved words, `IcbBuffer` Arcs; the signature (`:57-95`) hashes guest addresses only | the entry keeps the Arcs of the pool buffers it resolved into and their ids; `get` rebuilds when one id is no longer live; a snapshot change forgets entries that name a removed pool |
| `nxkit-metal/src/icb.rs` `keep` | the ICB retains `IcbBuffer.guest` | unchanged; keeps the pool buffer alive while the ICB lives |
| `gmem.rs:113-215` `Gmem` (`buffer`, `ranges`, `generation`) | the table buffer stays until the pools change | built from the snapshot; rebuilt when the snapshot's generation changes (not its own) |
| `gpu/frame.rs:301-306` `Keep` | the batch keeps textures, pool buffers of bindless and the gmem table | adds the pool snapshot |
| `gpu/mod.rs:125, 181-214` `pools`, `copyback.rs:56-104` | pool ranges in guest addresses | unchanged: they are virtual numbers |
| `encthread.rs:29-40` `Job` | the frame for the encoder thread | adds the pool snapshot taken at the flush |
| `nxkit-rt/src/nvn/ctx.rs:141` | `LinearSpace::new(base, backend.gpu_base(), len)`, made once | gets the virtual base through `Backend::gpu_base`; no code change |
| `nxkit-hos/src/pinned.rs` (one static range), `memsvc.rs:37-45` `shares` | "the pinned range never changes while the game runs": `UnmapMemory` recomputes the choice of `MapMemory` | a counted set of ranges; `MapMemory` records its choice and `UnmapMemory` reads it |
| `nxkit-host/src/memory.rs:61-90` `keep_gpu_pages`, `run.rs:78-79` | pins the whole range once | whole mode only; per-pool mode pins through the pool set |

Not affected: `ArgsMemo` (`bindless/mod.rs:110`, per pass), `uploaded` and `preloaded` (`frame.rs`, per batch,
scratch addresses), texture records and the texture store (guest addresses only), the walker's caches (host
addresses), the native plan cache (`native.rs`, guest addresses as keys).

### Class 4: CPU only

These read or write guest memory through the host pointer or only compute with guest addresses. They need the
address map (with the virtual base) and the host pointer, never a Metal buffer.

`nxkit-gpu/src/guest.rs:10-20` (`GuestView` of the walker); `queries.rs:293-345` and `backend/mod.rs:212-225` (GPU
counters written on the CPU); `walk/mod.rs:156,190`, `walk/ext/uniform.rs:34,133`; `gpu/handles.rs:156-160,
218-224` (descriptor reads); `draws.rs:506-520` (`widen_u8_indices`: reads on the CPU, the GPU reads the scratch
copy); `encode/diag.rs:35`; `native.rs:292-303, 537-541`; `native/offload.rs:152,183-185`;
`native/ane_trace.rs:119-158`; `pull/mod.rs:130-155` and `pull/model.rs` (diagnostic); `overlap.rs:110-115`,
`backend/api.rs:58,202-210` (diagnostic); `backend/api.rs:6-25` (`gpu_base`, `host_range`, pool registration);
`nxkit-nvn/src/space.rs`, `front.rs:152-266, 505-520`; `nxkit-rt/src/nvn/buffer.rs:149`, `pool.rs:187`,
`ctx.rs:299`, `cmdbuf/seam.rs:62`; `nxkit-host/src/sys/mod.rs:250-275`, `probe.rs:255`. The crates `nxkit-ndve`,
`nxkit-ndsr` and `nxkit-native-plan` have no GPU address.

Not encoded on the GPU today, so no site yet: `CopyBufferToBuffer` and `ClearBuffer` (only walked,
`walk/ext/records.rs:94-97`), buffer textures (`tbuf`), `sbuf_ubo` and `geometry` bindings (refused in
`programs.rs:343-379`). Each needs a resolve when it is built.

Count: class 1, 11 Metal-side sites and 16 producing sites in `nxkit-gpu`; class 2, 6; class 2v, 2; class 3, 9 (5
need work); class 4, about 25.

## B. Design

### B.1 The virtual guest address space

- `nxkit-metal/src/address.rs` gets `pub const GUEST_VA_BASE: u64 = 0xC0_0000_0000` and `GUEST_VA_MAX: u64 =
  0x100_0000_0000`. The guest range of OoT (12.6 GiB) fits below `1 << 40`. Keep the window below `1 << 40`: the
  gmem hint only works there (`gts_raw.rs`, an address with `addr_high >= 256` skips the hint), and the Maxwell GPU
  address space of the game is 40 bits. Metal's addresses seen so far are near `0x15_0000_0000`.
- `AddressMap` stays the map between host and guest addresses. Its `gpu_base` is the virtual base. Every CPU-only site
  keeps working.
- A miss fails loudly in three ways:
  1. **Compile time.** `Shared::guest`, `PrepareCx::guest` and the `guest` fields of `Gpu` and `GpuBackend` change
     type from `Arc<nxkit_metal::GuestMemory>` to a CPU-only `GuestSpace` (map plus host pointer, with `read`,
     `write`, `address_map`, `host_base`, `len`). Every place that handed the buffer to Metal stops compiling until it
     resolves through the snapshot.
  2. **Tripwire in `nxkit-metal`.** `ArgumentTable::set_buffer`, the index and indirect addresses of
     `RenderPass::encode`, and the address writers of `nxkit-gpu` (argument words, pull block, indirect jobs) check
     that the value is not in `[GUEST_VA_BASE, GUEST_VA_MAX)`. A hit is an error (`MetalError::Invalid("a guest
     address reached Metal")`) and a counted skip with the address. Two compares per bind.
  3. **Collision check.** Every Metal allocation that hands out a `gpuAddress` (`DataBuffer`, `Scratch` chunks,
     `GuestMemory`, heaps) asserts once at creation that it lies outside the window. If Metal ever does hand out such
     an address, the window moves; the check says so at once.
- A newtype for real addresses through the whole command layer was considered. It would touch every `u64` of
  `RenderCommand`, `ComputeCommand`, `Sink::put` and `Scratch::push`. The type swap of `guest` plus the tripwire catch
  the same misses for a fraction of the churn. Recommended: no newtype.
- Alignment: a pool buffer starts at the pool's start rounded down to 16 KiB, and `GUEST_VA_BASE` and `host_base` are
  multiples of 16 KiB. So a guest address and its real address agree modulo 16 KiB (if Metal's `gpuAddress` of a
  no-copy buffer is 16 KiB aligned; P0 checks). Index alignment, the 4-byte rule of indirect arguments, the storage
  buffer alignment mask of `multi_target` and the low bits of `dm_vlayout` keep working.

### B.2 The pool set

In `nxkit-gpu/src/pools.rs` (new):

```text
PoolSet (in GpuBackend, own Mutex, not the Gpu lock)
  current: Arc<PoolSnapshot>          copy on write; register and unregister build a new one
PoolSnapshot
  generation: u64
  entries: Vec<PoolEntry>             sorted by start, disjoint (a new pool replaces the ones it overlaps)
PoolEntry { start, end: u64 (guest addresses, not rounded), id: u64, buf: Arc<PoolBuffer> }
PoolBuffer { mem: GuestMemory (rounded range), va_start: u64 (guest address of mem's first byte), flags }
Loc { pool: u32 (index into entries), offset: u64 (in mem), real: u64, room: u64 (bytes to the pool's end) }
```

- `resolve(va, len) -> Result<Loc, Miss>`: binary search for the last entry with `start <= va`, then
  `va + len <= end`. `Miss::NoPool` or `Miss::CrossesEdge { pool_end }`. About 9 compares for 500 pools.
  `resolve_start(va)` checks the first byte only and gives `room`; the bindings whose size is only an upper bound
  (vertex streams, uniform and storage buffer sizes, indexed indirect draws) clamp to `room`.
- No lock on the hot path. The snapshot is immutable. `Shared` holds `&PoolSnapshot`; every encode thread reads it.
  A per-thread "last entry" hint is optional; measure first.
- The snapshot of a frame is the one at its flush. `GpuBackend::flush_frame` (game thread) clones `current` into the
  `Job` (or into the synchronous encode). This follows the game's timeline: a pool that the game finalizes after the
  flush still serves the frame, because the snapshot keeps its buffer. A pool that was finalized before the flush but
  is still named by the frame is the game's bug; its draws skip with `Skip::Buffer` and a reason.
- Registration no longer takes the `Gpu` lock for the buffer. Today `nvnMemoryPoolInitialize` waits behind a running
  encode (`backend/api.rs:14-19`). The `Gpu::register_memory_pool` call for `copyback` stays.
- gmem: `Gmem` builds its table from the snapshot's entries (delta `= real(start) - start`) and rebuilds when the
  snapshot's generation changes. Its own `pools` map goes away; otherwise a pool registered after a flush would be in
  gmem's map but not in that frame's snapshot, be left out, and never come back until the next change.
- Ranges in no pool: skip, with a reason that names the address and the nearest pools (`NXKIT_DIAG=pools`).
- Ranges across a pool edge: skip and count (`buffer.crosses-pool`). Two adjacent pools are two buffers; joining them
  is possible (they are one host range) but not worth it until a game does it.
- Whole mode: one entry, `start = GUEST_VA_BASE`, `end = start + len`, the single buffer. Same code.

### B.3 Overlapping no-copy buffers over one page

Pools are 4 KiB aligned (`0x1676d00000..0x1676e62000 | 0x1676e63000..0x1676ec6000` in a run log), so neighbours
round out to the same 16 KiB page and their buffers overlap. Metal documents no rule against two no-copy buffers over
the same memory, and the pages are the same physical pages for both. The GPU caches are kept coherent by the
barriers between passes and command buffers as for any memory. The same byte is never reachable through two buffers
in one snapshot (pools are disjoint, 4 KiB aligned, so they share no cache line). It can be through an old ICB and a
new pool at the same address, in different frames, with barriers between.

The test (P0, `nxkit-metal/tests/pool_buffers.rs`): allocate 64 KiB page aligned. Wrap A = `[0, 32K)` and B =
`[16K, 48K)`. (1) The CPU writes a pattern at 20K; a compute kernel copies 64 bytes from `A.gpu + 20K` and from
`B.gpu + 4K` into a `DataBuffer`; both equal the pattern. (2) A kernel writes through A at 24K; after the command
buffer the CPU reads it at 24K, and a second command buffer reads it through B at 8K. (3) Drop A, then B still reads
and writes. (4) `gpuAddress % 16384 == 0` for both. (5) Ten thousand buffers of one page each over one 160 MiB
region: creation time per buffer and residency commit time.

### B.4 Life time

- Make the buffer: `PoolSet::register` (game thread, in `nvnMemoryPoolInitialize`) pins the rounded range, then
  calls `GuestMemory::from_raw_uncommitted` (new: adds to the residency set, no commit). The next batch commits
  (`frame.rs:554`, `batch.rs:143`). One commit per batch, not per pool: pools come in bursts at load.
- If Metal refuses (a `PROT_NONE` or file-mapped page, a length above `maxBufferLength`), the pool is registered
  without a buffer, a line says why, and its GPU uses skip loudly. The guest call does not fail.
- Keep it alive: the batch holds the frame's snapshot in `Keep`; an ICB entry holds the Arcs of the buffers it names;
  a cached gmem table holds no buffer (the batches that bind it hold their snapshot).
- Drop it: when the last Arc goes, the `PoolBuffer` moves its `GuestMemory` into a graveyard (`Mutex<Vec>` in the pool
  set). The last Arc can go in a Metal feedback handler (`frame.rs:281`, `batch.rs` drop the batch's keep there), and
  a residency commit does not belong there. The encoder empties the graveyard at the start of each frame: drop (which
  removes the allocation), unpin, and the frame's commit applies it.
- Residency cost: `addAllocation` and `removeAllocation` are a lock and a set operation. `commit` and
  `requestResidency` wire the new pages; their cost grows with the bytes added, not the pool count (P0 measures it;
  the frame already reports the commit time in `PHASES[2]`).
- Wiring: a dropped buffer's pages leave the wired set only when the next buffer is made (P0). After the graveyard is
  drained, the memory of a large finalized pool is free one buffer creation later, and the memory report must not
  expect `wired` to fall in the same frame.
- Finalize and make a new pool at the same address: the new pool gets a new entry, id and buffer. Both buffers cover
  the same host pages, so they read and write the same bytes; the old one only costs its pin and wiring until the
  last frame or ICB that names it is gone. ICB entries check their ids and rebuild.
- Virtual pools are never registered (`nxkit-rt/src/nvn/pool.rs`, `pool_initialize`). A virtual pool is a window on
  its physical pool at the same addresses, so the physical pool's buffer serves it. Pools without storage are not
  registered and get no buffer.

### B.5 When to make the buffer, and which pools get one

Recommended: at registration, for every pool with storage. Lazy creation at first GPU use looks better (command
memory and shader code pools would never be wired) but has two problems. The gmem table must list every pool a raw
pointer may reach, and that is not known before the shader runs. And the first use is on an encode thread in the
middle of a frame, where a new buffer needs a residency commit before the batch.

`GPU_NO_ACCESS` (0x8) pools: they may not need a buffer, but a `PHYSICAL` pool with `GPU_NO_ACCESS` backs virtual
pools that the GPU reads. Texture uploads and copies read from GPU-accessible pools only. So a later exclusion is
"`GPU_NO_ACCESS` and not `PHYSICAL`", and only after the `nxkit-mem pools` line shows that it is worth it (its `gpu`
and `cpu-only` columns). The same holds for `SHADER_CODE` (0x40) pools: the host translates shaders on the CPU, but
the sizes decide.

### B.6 The pinned set in `nxkit-hos`

- `pinned.rs` becomes a counted set: `pin(start, end)` and `unpin(start, end)` on a `Mutex<BTreeMap<(u64, u64),
  u32>>`, and `overlaps(addr, len)` and a new `pinned_pages(start, end) -> Vec<(u64, u64)>` (the pinned parts of a
  range). The kernel asks it only in memory calls, not on a hot path. `set(start, end)` stays for whole mode.
- Who tells the kernel: `nxkit-gpu` defines `trait PagePins { fn pin(&self, start, end); fn unpin(&self, start, end); }`.
  `nxkit-host` implements it with `nxkit_hos::pinned` and hands it to `GpuBackend::new`. Tests use a fake that
  records calls. `nxkit-gpu` does not depend on `nxkit-hos`.
- Order: pin before the buffer is made, unpin after it is gone. Between the kernel's check and its remap a pool could
  be pinned, but only over memory the game hands back at the same time, which is the game's bug.
- `release` (`sys.rs:496-508`): split the range at pinned parts. Pinned host pages are zeroed in place and lose their
  access as today; the rest is given back (`drop_view`). Today one pinned byte anywhere keeps the whole range.
  Check only the whole host pages of the range against the pins; the edges are zeroed anyway.
- `MapMemory` (`memsvc.rs:170-195`): the choice between sharing and copying is recorded with the mapping (a set of
  `(d0, s0, size)` in the address space). `UnmapMemory` (`:199-225`) uses the recorded choice. With a changing pin set
  a recomputed choice can differ, and both mismatches lose data: copy-then-release over shared pages zeroes the
  source; share-then-unshare loses the stack's writes. Outside pools `MapMemory` shares again.
- `SetHeapSize` shrink (`memsvc.rs:85-99`) and `UnmapPhysicalMemory` (`:310-335`): through `release`, so only the pages
  of live pool buffers stay. A shrink over a live pool is the game's bug; over a finalized pool whose buffer an ICB
  still holds, the pages come back when the ICB goes.
- `MapSharedMemory` (`shared.rs:81`) keeps its log; `UnmapSharedMemory` drops its view as today.
- `NXKIT_GUEST_KEEP=0` still turns pinning off. `NXKIT_GUEST_WIRE=1` applies in whole mode only.

### B.7 `GuestMemory::allocate` and other users

`nxkit_metal::GuestMemory` keeps its meaning: one no-copy buffer over a host range. `allocate`, `from_raw`, `read`,
`write`, `gpu_address_of_offset` and `buffer` stay. The tests and tools that use it (`nxkit-metal/tests/gpu.rs`,
`blocklinear.rs`, `pull.rs`; `nxkit-gpu/src/upload/tests.rs`, `queries/tests.rs`; `nxkit-gpu/tests/ndsr_*.rs`,
`ndve_*.rs`) keep working. Where a test builds a `Gpu` or calls an upload with a `GuestMemory`, a helper
`PoolSnapshot::whole(Arc<GuestMemory>)` and `GuestSpace::over(&GuestMemory)` give the old behaviour in one line.
`nxkit-host probe` keeps measuring the whole range and gets a per-pool step (P0).

### B.8 The switch

`NXKIT_GPU_BUFFER=pools|whole`, read once at backend start, documented in `docs/diagnostics.md`. Default `whole`
until per-pool mode has run a full session of each title without new skips, then `pools`.

Keeping both is worth it: the shared code is the resolver, so whole mode costs the branch in `PoolSet::register`
(make a buffer or not), the single entry, and `keep_gpu_pages`. A clean cut would save about 60 lines and lose the A/B
run that tells a per-pool bug from an old bug. Revisit after a month; deleting whole mode is then one small package.

### B.9 The memory report

The `nxkit-mem pools` line (being added now) gets the buffers: `buffers` (count, sum of rounded lengths, union of
their pages), `retired` (buffers of finalized pools still held by a frame or an ICB: count and MiB), `graveyard`, and
`pins` (count and MiB of the pinned set). The Metal device split shows the pool buffers as `guest` (sum of lengths).
The guest reservation's `wired` should then be close to the union of `pages`; the line prints both, so a gap is
visible. The end-of-run line prints the peak of each. `NXKIT_DIAG=pools` prints each registration and release with
its range, rounded range, flags and time to make the buffer, and every resolve miss (first 50).

## C. Work packages

Every package leaves the workspace compiling, keeps files under 600 lines (`cargo run -p xtask -- limits`) and the
crate layers (`cargo run -p xtask -- layers`). The coding agents do not run tests. "Owns" means no package that runs
at the same time touches the file.

Order: P0, P1, P2 and P6 in parallel. Then P3. Then P4a, P4b, P4c, P4d in parallel. Then P5. Then P7 and P8 in
parallel. The first run on a game is after P5 (whole mode, virtual base): any missed site shows up there.

### P0: measure the two facts the plan rests on

- Goal: prove that a resident no-copy buffer wires only its own pages and that overlapping buffers are coherent.
- Owns: `crates/nxkit-metal/tests/pool_buffers.rs` (new), `crates/nxkit-host/src/probe_pools.rs` (new),
  `crates/nxkit-host/src/probe.rs` (one call and one `mod` line), `crates/nxkit-host/src/main.rs` or `lib.rs` only if
  the module must be declared there.
- Tests: the five cases of B.3 (skip without a Metal device, as `tests/gpu.rs:16` does).
- Probe step 7: reserve 2 GiB RW, wrap 3 buffers (1 MiB, 1 MiB overlapping by one page, 64 MiB), commit residency,
  print wired MiB before and after (`wired_bytes`, and `sys::vm::page_query` over the reservation); drop one buffer,
  commit, print again.
- Done: the test file compiles; the probe prints the numbers. The user runs both.

Run the test with `cargo test -p nxkit-metal --test pool_buffers --release -- --nocapture --test-threads=1`. Run the
probe with `cargo run -p nxkit-host --release -- probe <run folder>`; step 7 prints before steps 5 and 6.

Measured (`pool_buffers` passes; probe step 7, on this machine, 16 KiB pages):

- Wiring is per buffer. With A (1 MiB), B (1 MiB, one page shared with A) and C (64 MiB) resident, exactly 66.0 MiB of
  the 2048 MiB reservation are wired, and every touched page outside the buffers is not.
- A dropped buffer's pages stay wired. Time does not unwire them (2 s of polling), another `commit_residency` does
  not, and a drained autorelease pool does not.
- Making the next no-copy buffer unwires them (a small buffer made after the drop: 0 MiB wired at the first look). In
  the churn of 20 buffers of 64 MiB over 8 reused slots the wired memory never passed 64.0 MiB, so each make frees the
  buffer dropped before it.
- So the driver holds the pages of the buffers dropped since the last buffer was made, and no more. At about 50 pool
  buffers made per second, that is the pools finalized in about one frame.
- After 1,000 buffers of 64 KiB (made and dropped 50 at a time) 1.12 MiB stay wired, which is 18 buffers. The number
  of small buffers that the driver holds back is not exactly one, and the probe does not explain the 18.
- Cost: a 64 KiB buffer takes 7.0 us to make and 1.6 us to drop, each with its residency commit. A 64 MiB buffer is
  made in 0.6 to 0.9 ms (about 12 us per MiB), so the 2.4 GiB pool would take about 30 ms, once. The test made 10,000
  one-page buffers at 4.2 us each.
- Two buffers over the same page are coherent: the CPU and the GPU see each other's writes through either buffer, also
  after one of them is dropped, and a new buffer over a dropped range sees the bytes written in between.
- `gpuAddress` is 16 KiB aligned for both buffers. The GPU addresses of two buffers do not follow their host layout:
  B starts 16 KiB after A on the host, but A is at `0x1500878000` and B at `0x1500888000`, 64 KiB apart. So a GPU
  address is always computed per buffer, as `buffer.gpuAddress + (host - buffer host base)`.

### P1: `nxkit-metal` window and uncommitted buffers

- Goal: the virtual window constants, the tripwire and buffers that join the residency set without a commit.
- Owns: `crates/nxkit-metal/src/address.rs`, `guest.rs`, `table.rs`, `render/pass.rs` (one helper call per address,
  the file is at 571 lines: put the check in `table.rs` as `pub(crate) fn check_real(addr)`), `databuf.rs`,
  `scratch.rs` (collision assert), `lib.rs`.
- Adds: `GUEST_VA_BASE`, `GUEST_VA_MAX`, `is_guest_va(addr)`, `check_real`; `GuestMemory::from_raw_uncommitted`;
  `GuestMemory::drop` without a commit when made uncommitted.
- Tests: `address.rs` window tests (edges, below, above); `table.rs` refuses a window address (unit test without a
  device: test `check_real` directly).
- Done: compiles; nothing calls the new constructor yet; behaviour unchanged (real addresses are never in the window).

### P2: `nxkit-gpu` pool set (no users)

- Goal: `PoolSet`, `PoolSnapshot`, `Loc`, `Miss`, `PagePins`, the graveyard, `GuestSpace`.
- Owns: `crates/nxkit-gpu/src/pools.rs` (new), `crates/nxkit-gpu/src/pools/tests.rs` (new),
  `crates/nxkit-gpu/src/guest.rs` (add `GuestSpace` next to `GuestView`), `crates/nxkit-gpu/src/lib.rs` (`mod pools`).
- Make the map generic over the backing (`PoolMap<B>`), so the logic is tested without Metal.
- Tests: resolve inside, at both edges, across an edge, in no pool; `resolve_start` and `room`; a new pool replaces
  the ones it overlaps; rounding of 4 KiB aligned pools to 16 KiB; two pools sharing a rounded page each keep their
  own entry; generation counts every change; an old snapshot keeps a removed backing alive; `pin` before backing
  creation and `unpin` after drop (fake `PagePins`); whole mode resolves `va` to `real_base + (va - base)`.
- Done: compiles, unused code allowed with `#[allow(dead_code)]` on the module until P3.

### P6: kernel pinned set (parallel with P1 to P5)

- Goal: B.6.
- Owns: `crates/nxkit-hos/src/pinned.rs`, `memsvc.rs`, `sys.rs` (597 lines: move `release`, `release_edges` and
  `scrub` into a new `crates/nxkit-hos/src/sys/release.rs` or `release.rs` first), `memory.rs` (default
  `can_remap` doc), `shared.rs` (log text), tests in `crates/nxkit-hos/tests/memory.rs`.
- Keep `pinned::set` working (whole mode, and `nxkit-host/src/memory.rs:72` keeps compiling).
- Tests: counted pins (pin twice, unpin once, still pinned); `pinned_pages` splits a range; `MapMemory` that shared
  unmaps by sharing although a pin appeared in between, and the reverse (with `FakeMemory`); `release` over a range
  with one pinned page gives back the rest (a `FakeMemory` that records calls).
- Done: compiles; with today's single `set` the behaviour is unchanged except that `release` now splits.

### P3: plumbing in `nxkit-gpu` (whole mode, base still real)

- Goal: the snapshot flows from the flush to every encode site; gmem uses it; nothing resolves yet.
- Owns: `backend/mod.rs`, `backend/api.rs`, `encthread.rs`, `gpu/mod.rs`, `gpu/frame.rs` (580 lines: move the
  preload thread block, `frame.rs:110-140`, into a new `gpu/preload.rs` first), `gmem.rs`, `draws.rs` (the `Shared`
  struct only), `bindless/args.rs` (the `Env` struct only: add `pools`), `native.rs` (600 lines: no change here; if
  `PrepareCx` needs the snapshot, move `PrepareCx` into `native/cx.rs` first).
- `GpuBackend` gets a `PoolSet` in whole mode over the existing `Arc<GuestMemory>`, with `GUEST_VA_BASE` replaced by
  the buffer's `gpuAddress` for now (so `resolve` is the identity). `register_pool`/`unregister_pool` update it;
  `flush_frame` puts `current()` into the `Job` and the synchronous path; `encode_frame` takes it; `Shared` and `Env`
  carry `&PoolSnapshot`; `Keep` holds the Arc; `Gmem::refresh` takes the snapshot (B.2).
- Tests: `gmem.rs` (table from a snapshot, rebuilt on a new generation only); `encthread.rs` job test still builds.
- Done: compiles, identical behaviour.

### P4a: draws, indirect jobs, pull block

- Owns: `draws.rs` (545 lines: move `geometry`, `indirect_args`, `index_room`, `widen_u8_indices` into a new
  `geometry.rs` first), `geometry.rs` (new), `indirect.rs`, `pull/mod.rs`, `pull/tests.rs`, `mesh.rs` (only to keep
  `storage_table` on guest addresses and to use the new `indirect_args`), `encode/emit.rs` (only if `Geom` changes
  shape).
- `Geom::Indexed`, `IndexedIndirect`, `MultiIndirect` and `Indirect` carry real addresses plus the `Loc` the ICB path
  needs (`pool`, `offset`); `Prep::streams` carries real addresses and `Loc`s; `pull::layout(d, resolve)` writes real
  bases; `Job.rec`/`cnt` are real; `index_room` ends at the pool.
- Tests: `pull` layout with a resolver that adds a delta keeps the low bits in the attribute words; `index_room` stops
  at the pool end; a stream that starts in a pool and is longer than it is clamped, one that starts outside skips;
  indirect jobs hold real addresses.
- Done: compiles; with the identity resolver the output bytes are unchanged.

### P4b: argument buffers

- Owns: `bindless/args.rs` (except the `Env` struct, done in P3), `bindless/args/tests.rs`, `bindless/mod.rs` only if
  a helper moves.
- `Cbuf`, `UboPtr`, `Sbuf` words get real addresses (resolve start, clamp size to room); `drv_block`'s c0 CS UBO table
  gets the guest address and the size; a uniform with a snapshot keeps its scratch address in the word and its guest
  address in c0.
- Tests: with a delta resolver, the words are real and the c0 table is virtual; a binding outside every pool falls
  back as an unbound one does today (`statics.1`) and counts a reason.
- Done: compiles; identical bytes with the identity resolver.

### P4c: ICBs

- Owns: `icbs.rs`, and a new `icbs/tests.rs` if needed.
- `IcbBuffer` from the snapshot's `PoolBuffer` and the offset in it; `Entry` keeps the Arcs and ids of the pools it
  resolved; `get` rebuilds when an id is gone from the current snapshot; `Icbs::forget_pools(&[u64])`.
- Tests: an entry whose pool id vanished is rebuilt; `forget_pools` drops exactly the entries that name a pool (the
  id logic without Metal).
- Done: compiles.

### P4d: textures and copies

- Owns: `upload/mod.rs`, `upload/buffered.rs`, `upload/tests.rs`, `passes/copy.rs`, `passes/mod.rs`,
  `gpu/handles.rs` (the `copy_back_many` call only), `gpu/frame.rs` (the two `upload` calls only; P3 is done by then).
- `guest_offset(rec)` becomes `resolve(rec.address, rec.layout.size)` giving the pool buffer and offset; all calls pass
  the snapshot instead of `&Arc<GuestMemory>`.
- Tests: `upload/tests.rs` keeps passing through `PoolSnapshot::whole`; a texture across a pool edge skips with
  `Skip::Buffer`.
- Done: compiles.

### P5: the virtual base

- Goal: `GUEST_VA_BASE` becomes the fixed window; `guest` becomes `GuestSpace` everywhere in `nxkit-gpu`.
- Owns: every remaining `nxkit-gpu` file that names `GuestMemory` or `address_map()` for a CPU read (class 4 list:
  `guest.rs`, `queries.rs`, `overlap.rs`, `native.rs`, `native/offload.rs`, `native/ane_trace.rs`, `encode/diag.rs`,
  `gpu/handles.rs`, `gpu/mod.rs`, `backend/*`, `walk/mod.rs`, `pull/mod.rs`), `pools.rs` (whole mode uses the fixed
  base), `nxkit-host/src/frames.rs`, `seams.rs`, `run.rs`, `memory.rs` (pass `GuestSpace` and the whole buffer).
- `GuestSpace` has the same method names as `GuestMemory`'s CPU part, so most class 4 edits are a type name.
- Tests: `backend/api.rs` `gpu_base` is `GUEST_VA_BASE`; `queries/tests.rs` writes a counter through the virtual map.
- Done: compiles. First game run: whole mode, virtual addresses. Every miss is a tripwire error or a GPU fault in the
  log (`nxkit-metal: warning: frame ... failed`).

### P7: per-pool mode

- Owns: `pools.rs` (make buffers, pins, graveyard drain), `nxkit-host/src/memory.rs` (`PagePins` impl, whole-mode
  pin only), `run.rs` (no big buffer in pools mode), `frames.rs`, `nxkit-host/src/memreport.rs` (B.9; after the
  other agent's change has landed), `gpu/frame.rs` (graveyard drain at frame start), `icbs.rs` (call
  `forget_pools` on a new generation).
- `NXKIT_GPU_BUFFER` read once; default `whole`.
- Tests: `pools/tests.rs`: pin order with a fake backing that records events (pin, make, drop, unpin); graveyard drains
  once.
- Done: compiles; `NXKIT_GPU_BUFFER=pools` runs per-pool.

### P8: docs

- Owns: `docs/decisions.md` (a new R record that replaces R3: virtual addresses, one buffer per pool, measured
  numbers from P0), `docs/nxkit-gpu.md` (gmem delta, the `cbuf` binding text, the pool snapshot), `docs/nxkit-host.md`
  ("Guest memory", "What the GPU buffer costs"), `docs/nxkit-nvn.md` ("Guest memory and GPU addresses"),
  `docs/diagnostics.md` (`NXKIT_GPU_BUFFER`, `NXKIT_DIAG=pools`, the report fields), `docs/nxkit-hos.md` (pins).
- Done: the docs describe both modes and the default.

## D. Risks and open questions

1. **Wiring is per allocation, not per range** (the premise). Settled by P0 probe step 7: a resident buffer wires
   only its own pages (66.0 MiB for A, B and C of 1, 1 and 64 MiB over a 2048 MiB reservation). The pages of a
   dropped buffer stay wired until the next buffer is made (see B.4).
2. **Overlapping no-copy buffers.** Settled by the P0 test: Metal accepts a second buffer over the same page, both
   are coherent for the CPU and the GPU, also after one is dropped, and `gpuAddress` is 16 KiB aligned. Merging
   neighbours is not needed.
3. **Reads past a pool's end.** Today a vertex fetch past the end of a stream, or a uniform read past its size, lands
   in other guest memory. With per-pool buffers it can land in unmapped GPU memory and fail the command buffer.
   Settle: `NXKIT_DIAG=pull` already computes the vertex range of arrays and indexed draws; extend its check to the
   pool (`pull/model.rs`) and count draws whose range leaves the pool, in whole mode, before turning pools on. If
   there are any, pad each pool buffer by one page where the next page is guest memory.
4. **How much the pools cover.** If the game puts most of its heap into pools, the saving is the unused rest of the
   range, not all of it. A big pool that the game only partly uses is wired in full. Settle: `nxkit-mem pools`
   `pages`, `heap`, `largest` for OoT and BotW before P7.
5. **A pool in module data or bss.** File-mapped pages under a resident buffer become unreadable (measured
   2026-11-08). Pools outside the GPU range are refused today and stay refused. Settle: the existing error line; no
   game has shown one.
6. **Residency churn.** Settled by P0: a 64 KiB buffer takes 7.0 us to make and 1.6 us to drop, each with its
   commit, so 50 made and 25 dropped per second cost about 0.4 ms per second. A 64 MiB buffer takes 0.6 to 0.9 ms to
   make. The commit time in `PHASES[2]` still shows it in a real run.
7. **c0 tables.** The plan keeps them virtual (A, class 2v). Settle: an `nxb-xlate` test that translates a compute
   shader with a `ubo_ptr` binding and a mesh shader with a storage buffer, and checks that no `c0` value is cast to a
   `device` pointer in the MSL. If one is, that value must be real and the arithmetic against guest values must be
   reviewed.
8. **Pins and the kernel race.** A pin taken between the kernel's check and its remap leaves the GPU on old pages.
   It needs the game to register a pool over memory it is handing back. Settle: none needed; a debug check in
   `PoolSet::register` that the range is `NORMAL` RW in the kernel's map would catch it, if the host can ask.
9. **ICBs keeping finalized pools alive.** An ICB of a recording that the game never replays again keeps its pools
   wired until the candidates change. Settle: `retired` in the pools line; P7 forgets entries on every new
   generation.
10. **Window collision.** If Metal ever hands out an address inside `[0xC0_0000_0000, 1 << 40)`, the tripwire would
    refuse a real address. Settle: the creation assert (B.1); it has never been seen.
11. **The count of pools.** gmem holds 8192 entries. A game with more loses raw-pointer access to the rest, as today.

Simpler designs considered:

- **Fixed chunks** (one buffer per 2 MiB of the guest range that holds a pool): an O(1) table lookup, and chunk life
  times independent of the game's pools. But a pool larger than a chunk, or one across a chunk edge, cannot be bound
  as one Metal buffer, so chunks must grow into spans, which is the per-pool design with coarser edges and more wired
  memory. Not better.
- **A smaller single buffer** (smaller `reserve` and `alias`): no code, but it only moves the limit and fails a game
  whose peak grows. Rejected by the goal.
- **Metal-owned memory mapped into the guest** (make the pool's pages from a Metal buffer and remap them into the
  guest range): the pool already holds game data at registration, the kernel's map would change under the game, and
  the buffer is wired just the same. No gain.
- **One buffer that is not resident**: Metal 4 needs residency for every address a shader uses. Not possible.

What makes me doubt the direction: only risks 1 to 4. The code work is large but mechanical, and the whole-mode step
with the virtual base finds the missed sites before per-pool mode can lose data.

## What was not verified

- That `newBufferWithBytesNoCopy` accepts overlapping ranges, and how fast it and the residency commit are (P0).
- That the driver wires only the pages of each buffer and unwires them when the buffer is freed (P0).
- The share of the guest range that OoT's pools cover (the pools line, being added).
- That no translated shader dereferences a c0 table value directly (D.7).
- That Metal never hands out addresses in the chosen window (B.1 assert).
