# Changelog

All notable changes are in this file. The format follows Keep a Changelog. Versions follow Semantic Versioning.

## Unreleased

### Added

- The crate `nxkit-ipc` is the IPC server of the nxkit host. It parses HIPC and CMIF, keeps sessions and domains, serves `sm:`, counts every command and stops the game on an unknown service or command. It has no global lock.
- The crate `nxkit-svc-fs` is the `fs` service. It has the `nn::fs` functions of nxnative at SDK level (`exports()` and `big_stack_exports()`) and `fsp-srv` over IPC. The romfs is mapped, and the saves have the nxnative layout. `nxkit-rt` exports `nxkit_hle_find_big` for the functions that need the big host stack.
- The service `offline` moved from `nxkit-rt` into the crate `nxkit-svc-offline`.
- `nxb build` makes the app of a game with a title module without nxnative (R48 to R50). The stages are extract, romfs, rewrite, manifest, link, shaders and bundle. The app has `nxkit-host` as its program and `Contents/Resources/nxkit/` with `game.json`, `nxgame.dylib`, `romfs/`, `shaders/` and `pipelines.mtl4archive`. `nxb` writes `nxgame.dylib` as a Mach-O file with each module at its fixed address and signs it ad hoc. `--engine` has no use for such a game and stops the build with exit code 4. A game without a title module keeps the old path. Refer to `docs/build-botw.md`. The code was not compiled or run on game files when it was written.
- The title hook `standalone` of `nxtitle` carries the NVN function list, the shader facts, the run settings and the memory layout of a game. `nxgame-botw` fills it from nxnative.
- `nxb build` unpacks DLC into `aoc/<index>/` in the run folder and lists it in `game.json`.
- The crate `nxkit-host` is the main program of the nxkit host (`nxkit-host <run folder>`, macOS only). It reserves one range of guest memory that is also the no-copy GPU buffer, loads `nxgame.dylib` with `dlopen`, takes the slide of dyld and relocates the modules for it, or copies them when dyld cannot, fills the import slots from the service crates, runs the svc and TLS gates, the big-stack entry for service functions, the game threads and the AppKit window. `nxkit-host probe` measures the address and copy costs of R18. The contract with the build stage is `docs/nxkit-host.md`. The parts that other crates provide are seams in `crates/nxkit-host/src/seams.rs`.
- The crate `nxkit-rt` builds `libnxkit_rt.a` for the host, for `aarch64-apple-darwin` and `aarch64-apple-ios`. It exports `nxkit_init` and `nxkit_hle_find` (header `crates/nxkit-rt/include/nxkit_rt.h`). It implements no service yet, so every lookup gives NULL. Refer to `docs/plans/runtime-hooks.md`.
- `nxkit-rt` also builds `libnxkit_rt.dylib`, which the Mac host loads at run time. It answers the `offline` service, ported from nxnative. `nxb build --platform mac` puts `DM_NXKIT_SERVICES` and `DM_NXKIT_LIB` into the run command.
- `nxgame-botw` runs the `offline` service on nxkit.
- The crate `nxkit-svc-input` is the `input` service. Its exports answer the `nn::hid` calls that nxnative's `hle_hid.c` replaces (npad, six-axis, vibration, mouse, keyboard, touch screen, gesture, Nmouse, Palma). It reads `GCController`, `GCMotion`, `GCKeyboard` and `GCMouse` at each call and plays rumble with `CHHapticEngine`. The `hid` IPC service hands out the HID shared memory in the Horizon layout. The host calls `nxkit_svc_input::tick()` on each display tick, sets `SystemEventHooks` once and sends touch points through `set_touches` (R39, R40).
- The crate `nxkit-hos` holds the Horizon thread model of the nxkit host. One guest thread is one host thread. Guest locks, condition variables and address waits wait with `os_sync_wait_on_address` and have no global lock. It has handles, threads, events and `WaitSynchronization`. The QoS class of a thread follows its role, which a title supplies as a table. The core rules are off by default and opt-in. Per-thread counters are always on and `Kernel::dump_stats` prints them. `nxgame-botw` has a role table that is marked unverified. A waiter of a higher QoS class lends it to the owner of the lock. The handle table has 16 shards. `ThreadSpec.stack_size` sets the host stack.
- `nxkit-rt` exports `nxkit_kernel_init`, `nxkit_svc`, `nxkit_kernel_set_thread_name` and `nxkit_kernel_dump_stats` for the service `kernel`. The host gates call `nxkit_svc` with the svc number and registers x0 to x7.
- The crate `nxkit-metal` is the base of the Metal 4 backend of the nxkit host. It builds on macOS only. It has the Metal 4 queue and command allocators, guest memory as one no-copy buffer with the GPU address mapping, a pipeline cache with an optional binary archive, a drawable path, CPU and GPU time per command buffer, a block-linear deswizzle kernel and a data-driven pass encoding API. It also has full pipeline state, mipmapped, layered, 3D and cube textures with views, block-linear deswizzle and swizzle for them, sub-rectangle copy-back, a residency API, a barrier API, indirect command buffers and a `CAMetalDisplayLink` driver for 60 and 120 Hz. It binds Metal with a pinned commit of `objc2-metal` from GitHub, listed in `deps.toml`.
- The crate `nxkit-nvn` is the first part of the nxkit GPU. It is pure Rust and has no Metal code. It maps guest memory to GPU addresses, models textures (layout, block-linear reference, TIC and TSC headers, pools, a resource table with residency flags), records into the command memory of the game with a cache of recording summaries (R17), holds a frame until a present or a CPU wait, and infers render passes with load, store and memoryless choices. It also lists the NVN functions of nxnative and the ones it handles. It is not wired into the host. Refer to `docs/nxkit-nvn.md`.
- `nxkit-nvn` has the trait `Backend`, which the GPU backend implements. A queue keeps the state that its last submit left, as on the GPU. The doc comment of `tic.rs` records how each TIC and TSC field was checked. No bit position is checked against hardware.
- `nxkit-rt` has the module `nvn`, the NVN C ABI of the host (R22 to R24). It has the device, queue, sync, memory pool, buffer, window, event and program functions of nxnative, the GPU context, and the helpers for objects in guest memory. A name that nxnative knows and nxkit does not implement stops the game with its name. The service `gpu` exports `nvnBootstrapLoader`. The host installs the backend with `nxkit_rt::nvn::install`.
- `nxkit-rt` `nvn` also has the texture, sampler and state groups (R51, R52): the texture builder, texture, view and texel access, the texture and sampler pools with their TIC and TSC headers, the sampler builder and sampler, and the blend, color, channel mask, depth stencil, multisample, polygon, vertex attribute and vertex stream state objects. The `state` module has a reader for each state object for the command buffer.

- `nxkit-hos` models the process memory of Horizon over the host's reservation: `SetHeapSize`, `SetMemoryPermission`, `SetMemoryAttribute`, `MapMemory`, `UnmapMemory`, `QueryMemory`, `MapPhysicalMemory`, `UnmapPhysicalMemory`, shared and transfer memory, `GetInfo`, resource limits, `GetSystemTick`, `ExitProcess`, `Break`, `OutputDebugString`, `ReturnFromException`, `GetProcessId`, `GetThreadId` and the cache flush svcs. A thread has a thread-local slot with the IPC buffer. The kernel implements `nxkit_abi::kernel::Kernel`, so `ConnectToNamedPort`, the three request svcs and `CloseHandle` reach the sessions of a server. Refer to `docs/nxkit-hos.md`.
- `nxkit-hos` exports the SDK take-overs that nxnative has on its kernel side: `getentropy`, the csrng bytes, `getenv` and `StandardAllocator::Allocate`. `nxkit-rt` lists them under the service `kernel` and adds `nxkit_kernel_set_guest_sym`, `nxkit_kernel_os_init`, `nxkit_kernel_set_deterministic`, `nxkit_kernel_forced_names` and `nxkit_kernel_thread_tls`.
- `NxkitKernelConfig` has `main_thread_priority`, `main_thread_core`, `main_thread_stack_size`, `process_id`, `process_exit` and `memory`.

- The crate `nxkit-svc-video` is the `video` service of the nxkit host. It replaces `movie::Decoder` of the Nintendo SDK and decodes VP9 with VideoToolbox into Metal compatible `CVPixelBuffer`s. A `CVMetalTextureCache` turns them into textures with no copy. It also replaces the `nn::codec` video decoders of the Switch 2 SDK (101 entries, VP9 only, the other codecs answer Initialize with an error). The service has exports and no named services, and `big_stack_exports()` is empty. Its `Config` takes a guest symbol resolver, a guest thread runner and an optional texture sink. Refer to R43 and R44.

- The crate `nxkit-svc-audio` is the audio service of the nxkit host (R41, R42). It takes over the audio functions of the SDK as nxnative does, and keeps `audout:u`, `audren:u` and `IAudioDevice` as the IPC fallback. One AUHAL output unit mixes in its callback with vDSP and joins the `os_workgroup` of the device. The game hands blocks over through lock-free rings. `nxkit-rt` has the service under `Service::Audio`, a new lookup `nxkit_hle_find_big` for the functions that need a big host stack, and `nxkit_audio_init` for the host callbacks and settings. It is not compiled or tested yet.
- `nxkit-metal` has the present path of R19. Every tick of the 120 Hz display link drives the `Pacer`, which shows a game frame with `presentAtTime` or holds the last one. At most two frames are in flight (before: three). It adds the present plan policy, the render scale policy, `SpatialUpscaler` and `FrameInterpolator` over MetalFX (`objc2-metal-fx`, pinned in `deps.toml`), and `PacingStats` for present time, missed ticks and the frame interval histogram.
- The crate `nxkit-svc-pacing` is the `pacing` service. It exports `nn::vi::GetDisplayVsyncEvent`, clocks it from the display ticks, and owns the frame step state page and the variable step ported from nxnative. `nxkit-rt` lists its exports under `Service::Pacing`.
- `nxgame-botw` carries the frame step, the interval controller cuts and the render scale stubs of seedds/nxnative#76 as data in `rules/01007EF00011E000.toml`, with the evidence of each rule, and sets its `pacing()` hook. `tools/gen_botw_rules.py` makes the file.
- `nxkit-svc-pacing` ports the frame limit logic of nxnative. `StepMode::Load` is the load-driven step (`fstep_tick`: the cap, the hold in vsyncs, the extra vsync for a late frame, the thermal cap and the CPU watch). `Limits` is the mechanism of `hle_fpslock.c`: it writes the target into value rows, found offsets and a reflected class. The data comes from title and family modules through the new `nxtitle` hook `RuntimeHooks::limits`. `interval.rs` holds the target and present interval rules. `frame_presented`, `hold`, `present_vsyncs` and `deadline_vsyncs` are the calls of the host. `hle_fpslock.c` has no SDK export, so `exports()` has not changed.
- `nxtitle` has the hook `RuntimeHooks::pace_profile` and the type `PaceProfile` (`fps_target`, `fps_hold`, `present_snap`, `present_interval`). `nxgame-botw` sets it from the profile of nxnative.
- The crate `nxkit-gpu` is the frame encoder of the nxkit host. It builds on macOS only and has no `unsafe` code. It turns a held `nxkit-nvn` frame into Metal 4 work: pipelines keyed by the translated state, one indirect command buffer for each reused recording, bindless textures and samplers, barriers only where data flows, memoryless attachments, parallel encoding on a small job system, a render target scale for native 4K, and counters for the CPU and GPU time. `nxkit-metal` gained the formats, samplers, data buffers, compute indirect command buffers, ICB data commands and split render tables that it needs. It is not wired into the host. Refer to `docs/nxkit-gpu.md`.
- The crate `nxkit-svc-system` holds the system services as `Service` objects for the IPC server: `set`, `set:sys`, `apm`, `apm:p`, `time:u`, `time:a`, `time:s`, `acc:u0`, `acc:u1`, `appletOE`, `appletAE`, `pctl`, `pctl:a`, `pctl:s`, `pctl:r`, `nifm:u`, `friend:u`, `prepo:u`, `lm`, `psm`, `ssl`, `bcat:u` and `aoc:u`. Values follow nxnative and command IDs follow libnx and switchbrew. `nxkit-rt` lists it under `Service::System`. The host sets the focus and operation mode with `set_focus` and `set_operation_mode`.
- The crate `nxkit-svc-system` also exports the SDK functions that nxnative takes over (`exports()`, and `big_stack_exports()` for the two that need a big stack) and a `quit` function for the graceful exit. The host passes the functions that the exports need in `set_seams`. `Config` has the fields `previous_program`, `aoc_dir`, `font_dir`, `shared_font`, `shared_font_ext`, `shared_font_decode` and `sf_services`.
- `RuntimeHooks` has the hooks `shared_font_decode` and `sf_services`. The title module `nxgame-botw` answers the first, and the new title module `nxgame-acnh` (Animal Crossing: New Horizons, behind the feature `acnh`) answers the second.

### Changed

- `imports.json` has `addend` and `weak` for every undefined-symbol relocation, and `exports.json` lists only global, non-hidden symbols. The host adds the addend, stores 0 for a weak import with no provider and stops if the image and the list disagree on the number of slots. A relocation of a defined symbol binds to a service, then to the first export in load order.
- Each module in `game.json` has `low` and `span`, so the part below `base` of a far layout counts in the host's address map.
- The rules of Breath of the Wild live only in its title module. `data/rewrite-rules/01007EF00011E000.toml` and its rows in `snapshot.tsv` and `files.toml` are gone, and `tools/gen_rewrite_patches.py` skips games that have a title module.
- The main thread attaches with the priority, core and stack size of the NPDM instead of a guessed priority 44. Without them it does not attach.
- A QoS lend no longer reads a `pthread_t` of an owner that may have exited. The lend table lives in the owner's thread record (R31).
- `nxtitle::Settings` names no engine. `ue_texture_pool` is now `texture_pool_mb`, and `ue_fps_cvar` is now `fps_cvar`, with no default. `game.json` writes `fps_cvar` only when a title sets it. The settings of `nxgame-botw` are marked as nxnative's record, and R10, R17 and R19 replace some of them.
- `nxkit-rt` has the NVN command buffer group (`nvn::cmdbuf`) with the 145 `nvnCommandBuffer*` functions of nxnative. A recording call takes no lock, makes no lookup and allocates nothing. The write position lives in the guest `NVNcommandBuffer` (R54). The host installs the texture seam with `cmdbuf::set_seam` and the title settings with `nvn::set_command_settings`.
- `nxkit-nvn` has the extension record `Record::Ext` with `ExtCode`, `Setting`, `StateKind` and `DrawKind` (R53), the writer `Cursor` with `write_encoded` and `commit`, `FrontEnd::recorded`, and the list `NXNATIVE_EMPTY`.
- The title module `nxgame-mtf` for Mario Tennis Fever (`0400D4201C6EA000`), behind the `nxtitles` feature `mtf`. It states the need `vertex_stream_null_bind`.

### Changed

- `MAX_RECORD_BYTES` of `nxkit-nvn` is 120. The memory callback of a command buffer runs with no lock held.
- nxkit is licensed under GPL-3.0-only. Releases up to 0.9.2 stay under MIT. Code ported from Ryujinx and nxnative keeps its MIT notice in `THIRD_PARTY.md`.

## 0.9.2 - 2026-11-07

### Fixed

- `nxb build` names a version that the input lacks. Before, the rewrite stage stopped with an NRO collection error about a missing `extract/<version>/RomFS.img`. Now the build stops before the extraction. Where the NSP script derives a profile from the version it has (the `package` or a `shader_versions` entry of the profile is not in the input), the exit code is 4, so a launcher can run the script. `-p` with a version that the input lacks is an error (exit code 1), as in the script.

## 0.9.1 - 2026-11-07

### Added

- The title module `nxgame-botw` for The Legend of Zelda: Breath of the Wild (`01007EF00011E000`), behind the `nxtitles` feature `botw`.
- `nxtitle` splits the host into services (`Service`). `RuntimeHooks::runtime` lets a family or title move one service of its game from nxnative to nxkit. `Resolved::nxkit_services` lists them.
- `docs/plans/runtime-hooks.md` plans how needs reach the host and how the nxkit runtime replaces nxnative one service at a time.

### Changed

- `render_cond`, `tex_flush_upload` and `tex_dirty_upload` are needs of `nxgame-oot`, not of `nxfamily-agl`. Breath of the Wild runs on agl and sets none of them.

### Fixed

- The shell framework build links its own host and writes loader `FH` when the engine has the `DmShellGame` target (#11, @STRML). Before, it built the target `DmGame` and wrote loader `FR`, and the shell refused the framework. An engine without that target keeps `DmGame` and `FR`.
- The framework keeps `dm/env.txt` in that case. The shell reads the build-time defaults of the player from it, for example `DM_PROF_TEX_BC_CPU=1` for older devices.
- The check for the `DmShellGame` target no longer depends on the indent of `project-framework.yml`.
- The manifest key is `launcher`, as the engine's script writes it. Before, it was `app binary`.

## 0.9.0

### Changed

- The repository is now `v0lumehi/nxkit`. GitHub redirects the old `v0lumehi/nxbuild` URLs, including release downloads. The binary `nxb`, the crate names and the release file names stay the same.

### Added

- The title layer in `titles/`: the API `nxtitle` (families, title modules, hooks, evidence, the catalog), the registry `nxtitles` with one cargo feature per module, the family `nxfamily-agl` and the title module `nxgame-oot` for `0400486024DEC000`. Refer to `docs/titles.md`.
- `xtask layers` checks layer `T`: a family or title crate uses only `nxtitle` and L0, and only `nxtitles` uses a family or title crate.
- `tools/hmma-precision/run.sh` shows how the GPU of the Mac sums a half `simdgroup_matrix` product.
- The `nxb-xlatedev` tool `xlate-msl` writes the MSL of a list of shader keys.
- `docs/analysis/oot-open-issues.md` gives the cause of three defects of one title on Apple GPUs (upstream issues 1547, 1568 and 1326). The cause of 1326 is in the upstream runtime.

### Fixed

- A byte or half-word store to shared memory is one Metal store, not a compare-and-swap loop on its word (upstream `ffebfd59`). A task or mesh shader's primitive index bytes no longer retry.
  A call passes each argument as a variable, because the helper's parameters are references. Before, a literal argument gave MSL that did not compile.
- A pixel shader's depth output is read from register 4 times the number of enabled colour targets plus 1 (upstream `473c9026`). Before, it counted enabled components, which is wrong when a target has fewer than four.
- The stage memory of a mesh shader reserves the primitive index bytes for every block of 32 primitives (upstream `d365496d`). Before, the bytes were 4 plus 3 times `max_p`.
  Lanes 126 and 127 of a mesh with `max_p` 126 wrote past the array, into the memory of the next workgroup.
- A mesh shader reads its attributes as the guest lays them out (upstream `3ccb7da0`). All vertex blocks come before the primitive blocks, and each block has only the components of its own SPH.
  The point size is the first vertex component and becomes `[[point_size]]`. The topology and the index bytes of a primitive come from control +0x5f. The job carries `verts` and a `"psiz"` component.
- Mesh shaders get the outputs that their paired pixel shaders read and they do not write, as (0, 0, 0, 1). The split pairs a mesh key with its pixel key and the pipeline archive plan leaves mesh pairs out (upstream `5e344a87`).
- Mesh outputs are as wide as the pixel shaders that read them, so a large mesh fits the Metal limit of 32 KB (upstream `d777f0a3`). `Varying` has a new field `read`, the mask of the components that the pixel shader reads.
- The entry of the library `_bf` is `main0_f` and its result struct is `main0_out_f` (upstream `c8bfa161`). The host asks for `main0_f`. Not ported: the cut of `_bf` out of the one compile of `_b`. `nxb` still compiles `_bf` as a source of its own, so the build speed gain is not there. Every extra pixel entry already calls the one `dm_body_main0`.
- A short NDSR pack no longer stops the shader split (upstream `ff33639c`, issue 1296). The record-start score counts only complete 16-byte records. Before, a pack with a partial last record gave the error "NDSR records: a buffer that is not a multiple of 16 bytes". The split of every pack that was read before is unchanged.
- The pipeline archives hold a compute pipeline for each compute shader of the profile (the `C` rows of `pso_manifest.tsv`, then the compute rows of `shader_map.tsv`), compiled offline in groups of 16 apart from the render bases. Shaders with function constants are left out. `archive.tsv` gets a `compute` line. `nxb pso` takes `--shader-map`, `--manifest` and `--no-compute`. The host side (`cps_new` looks the pipeline up in the bundle) is not part of nxb. (upstream 30fb2136)
- The rewriter keeps the branches of an exclusive window in its copy (upstream `10fd9705`). A conditional branch to the store of its own window is no longer taken for a retry block, and a `b` in a window is relocated to the copy or to an exit stub.
  A window with a `b` may not leave with a live reservation. A direct branch from another function into the middle of such a window is refused, and so is a retry copy that branches into the middle of its window.
  Before, the first case stopped with "retry block overlaps a window" and the second case was copied without relocation.
- The `simdgroup_matrix` body of `HMMA.16816.F16` sums in FP32 and rounds D to half once, as Ampere does. Before, it summed in half. On an M2 Max a half product rounds after each step and gives infinity when a partial sum is larger than 65504.
  A GPU test on the M2 Max with random matrices gave the exact half result for 31 % of the values with the old body and for more than 99.9 % with the new body. The cost is about 1 %.
  This is the likely cause of the coloured speckles of upstream issue 1568 on M1 and M2. No game was run.
- `TRANSLATOR_ID` is `msl-2.16`.

## 0.8.0

### Added

- A passthrough geometry shader gets the lean geometry kernels `_bl0`, `_bl1` and `_bl3` (upstream `dmsh_geometry.lean`). They write only the viewport mask and the indices.
  Their `.json` has `geometry` with `stride` 1, `lean` true and `capture_reads`. When the full kernel does not fit the transform, the shader has no `_bl` and `notes` gives the reason.
- `TRANSLATOR_ID` is `msl-2.15`.
- The Unreal decode of `nxb build` reads the shader libraries of Unreal Engine 5.6 IoStore containers (`.utoc` and `.ucas`).
  Each library becomes a version 2 shader code archive in `<version>/ue_shaders`. The archive name comes from the `Manifest_*.txt` files.
  Stored, zlib and LZ4 blocks decode in nxb. Oodle blocks and shader groups decode with the game's own decoder in the Mac host (`DM_OODLE_SERVE`).
  An encrypted container opens with the AES-256 key that main holds for the paks. The reference script has no key for these containers.
  The tests use synthetic containers and a fake decoder. No real IoStore game was run.
- `nxb build --platform ios` builds the iPhone app of a Vulkan or OpenGL game. The stage `Vulkan shaders` (or `GL shaders`) writes `<work>/shader/ios/vk` (or `gl`), and the IPA has it in `msl/vk` (or `msl/gl`). The NVN split and chain do not run. Unit tests only, no real game IPA and no device test.
- The JIT capture is ported. An iOS build of a game whose main imports `nn::jit` boots the game in the Mac host and builds `jit/jit.img` with `nxb_rewrite::jit`, the port of `jit_capture.py`. The build no longer stops when `jit.img` is missing. `--platform mac` sets `DM_JIT_CAPTURE` for such a game.

### Fixed

- The split reads the export chunks of IoStore containers of TOC version 4 and later. Before, it found none in these containers.
- A patch container `<name>_P.utoc` mounts after the game's containers, like `<name>_0_P.utoc`.
- The split skips the global shader cache of a pak only when the IoStore containers in its folder hold the cooked packages. This is the rule of the reference.
- A damaged IoStore TOC gives an error. Before, a TOC of 16 bytes made the split panic, and a directory index with a loop did not end.

### Known gaps

- The known gaps of 0.7.0 apply, except the IoStore shader libraries, the lean geometry kernels, the JIT capture and the iPhone app of a Vulkan or OpenGL game.

## 0.7.0

### Added

- Vulkan and OpenGL games build. The new crate `nxb-layer` translates their shaders with naga (SPIR-V) and the `glslang` crate (GLSL).
  The Mac shader stage writes `shader/vk/vk_shaders.{metallib,tsv}` or `shader/gl/gl_shaders.*` in the format of the host's `vk_shaders.py` and `gl_shaders.py`.
  The shader split and the pipeline archive are skipped for these games. The Mac app ships `msl/vk` or `msl/gl`.
  The stage also reads the `vk_capture` or `gl_capture` folder of the capture boot, so shaders the game makes at run time are in the library.
  On Hades, 74 of 74 shaders translate and compile.
- `nxb build` boots the game in the Mac host after the rewrite. The host runs headless with a time limit, and its output goes to `<work>/logs`.
  The boots are the NRO capture, the heap check and the region check of a provisional profile, the GLSLC capture, the Vulkan and GL captures and the Unreal decode.
  A step whose output is complete does not boot again.
- The split decodes Oodle and LZ5 streams with the game's own decoder in the Mac host (`DM_OODLE_SERVE`). `nxb build` starts it when the profile names Oodle packs or legacy Unreal paks.
- The split reads Trinity packs, Frostbite superbundles, the NROs of the NRO capture, the outputs of the GLSLC capture and the decoded Unreal shader archives.
- The Unreal decode opens pak indexes encrypted with AES-256. The key comes from main. LZ4 shaders decode in nxb. Oodle shaders decode in one boot of the Mac host (`DM_OODLE_JOB`).
- `nxb-nro` finds NROs, reads NRR version 1 and `.nrolz` files, and writes `missing.tsv`.

### Fixed

- The shader split finds the shaders of NVN2 games that it missed.
  It reads the NVN2 shaders of agl `BAHS` files, the task and mesh shaders of `BNSH` programs, and nn::ndve `NDVE` files.
  An `NDSR` pack may hold data entries of 8 bytes between its shaders. The split steps over them.
  On one NVN2 title the split now finds 42,854 shaders, the same keys as the profile's map. Before, it found 35,732.
- The shader step reads each record through the split's own `shader_map.tsv`.
  Before, it took the record indices from the profile's map. When the two maps differed, a shader was built from another shader's record.
  A profile key that the split did not find fails with "the split did not find it" in `_b_build.json`.
- A gather with four offsets kept its coordinates in a scalar `float`. Metal rejected the shader when the coordinates had two or three components.
- A size query for a component the texture does not have, such as the depth of a 2D texture, failed with "size component out of range". It now returns 1.
  A size query on a 1D texture or a texture buffer returns the width for every component.
- The translator emits `ATOMG.CAS` of NVN2 shaders as a compare and swap. Before, the instruction was unknown.
  `ATOMG` and `RED` with `INC` or `DEC` run as a compare and swap loop with the wrap of the hardware.
  A memory descriptor in the uniform register of `LDG`, `STG`, `ATOMG` and `RED` is no longer added to the address.
- The Metal compiler takes up to 1,024 nested brackets (`-fbracket-depth=1024`). Before, a deep expression failed with "bracket nesting level exceeded maximum of 256".
- The split no longer reads the words of an NVN2 control section in a `BAHS` file as a program entry.
- A raw global access first checks the storage buffer it chose last. A loop over one buffer no longer searches all buffers on each access.
- HMMA chains of compute shaders keep the accumulator and A in the SIMD-group between calls (upstream `bd9f422d`).
  A that an `LDSM` read is loaded from shared memory where the `LDSM` read it. Upstream measured the Ocarina of Time movie network at 11.4 ms instead of 14.8 ms.
- An HMMA call outside every chain no longer overwrites the accumulator that a chain holds in slot 0 (upstream `13b432b4`, issue 1537). Upstream saw dotted rows in Ocarina of Time from it.
- An `LDSM` sits between two SIMD-group barriers (upstream `16776913`). Without them, an `LDSM` could read rows before other lanes stored them. Upstream saw dotted streaks in the Ocarina of Time movie networks and white blotches through the game's upscaler.
- `ATOMS.INC` and `ATOMS.DEC` run as a compare and swap loop (upstream `1a410e6e`). Before, they gave 0 and did not touch shared memory.
  `ATOMS.CAS` gives the old value. Only `CAST` and `CAST.SPIN` give 1 or 0.
- Five Ampere ALU instructions compute what the hardware computes (upstream `b97f0dc6`). Upstream found them by differential tests of single instructions.
  `IADD3` sets its first carry predicate for sum >= 2^32 and its second for sum >= 2 * 2^32. Before, a carry of 2 lost 2^32 in a 64-bit add.
  `PLOP3` computes both results before it writes the first.
  `POPC` and `FLO` take bit 63 as a bitwise not only in the register, uniform register and constant forms. In the immediate form it is the immediate's top bit.
  `LEA.HI.SX32` takes the sign extension of A as the high word.
  `LEA` with a negated B adds ~B with `.X` and ~B + 1 without it. The + 1 counts in the carry out.
- Half pairs read their operands as the hardware does (upstream `9dc81953`). A constant operand applies its swizzle. Before, `HMUL2 R6, R19.H0_H0, c[0x0][0x168].H0_H0` gave the high lane the other half of the constant.
  Swizzle value 1 is `.F32`, the 32-bit float in both lanes. `HSETP2` and `HSET2` with `.FTZ` compare a subnormal half as zero.
- `I2F` from a 32-bit integer to a 32-bit float and `F2F.F16.F32` apply `.RM`, `.RP` and `.RZ` (upstream `645d3029`). Before, both rounded to nearest.
  The 32-bit integer division of the compiler starts with `I2F.U32.RP`. Rounded to nearest, a value above 2^24 came out one ulp off in about half the cases.
- `MUFU.SIN` and `MUFU.COS` keep the fraction of a large number of turns (upstream `b5948342`). The lowering is sin or cos of (t - rint(t)) * 2pi.
  Before, t * 2pi lost the fraction, and Metal's fast sin and cos gave 0 past 2^20 turns. Upstream saw the SSAO of Donkey Kong Bananza lose its ambient occlusion in the lower right.
- The rewriter takes a compare as a jump table's bound only when a branch on its flags guards the table load (upstream `e49afb1e`).
  `b.hi` out of the dispatch gives N + 1 entries and `b.hs` gives N. `b.ls` and `b.lo` into the dispatch count the same way when the instruction before their target is a `b`, `br` or `ret`.
  A compare with no such branch only sets the flags, and the walk goes on to the check before it. With none the site is refused.
  Before, `cmp w18,#4; b.hi exit; cmp w18,#1; ldrsw ...` read 2 entries of a table of 5. `cmp #5; b.hs` read 6.
- The rewriter's walk for a jump table's base passes a `mov` of a constant that would put the table in page zero (upstream `9671a581`, issue 1532).
  Horizon never maps page zero, so no path of a working game carries that value to the table load. Any other write still ends the walk.
  Cuphead 1.3.7 `main+0x59dc90` holds the constant 1 in the base register of another loop (`mov w12,#1`). Before, its jump table was not found.
- The Mac app holds the `c1_full.tsv` and `cb_extent.tsv` of its own shader build, as the IPA does. Before, it held the tables of the game profile.
  Those tables name the stamp of another build, so the host ignored them. Each c1 then got a buffer of its own and whole uniform bindings were copied.
  `nxb build --platform mac` gives the host the same tables in `<work>/run/tables`. `nxb-macapp` takes them with `--guard-tables`.
- `TRANSLATOR_ID` is `msl-2.14`.

### Known gaps

- The iOS app build of Vulkan and OpenGL games.
- Standalone `nxb extract` and `nxb split` have no host. They still stop with exit code 4 for Oodle and LZ5 formats.
- The JIT capture is not ported. `nxb build --platform ios` stops for a game that imports `nn::jit` and has no `jit/jit.img`.
- Unreal Engine 5.6 IoStore shader libraries are not read. The Unreal decode stops with an error.
- No boot and no host decoder was tested with a real game. The tests use synthetic data.

## 0.6.6

### Changed

- The raw tier of the own front end also makes the iOS libraries of `nxb build`. Before, Apple's `metal` and `metallib` made all iOS libraries.
  When the raw tier rejects a shader, Apple's compiler makes the iOS library.
  The iOS library key gets the `frontend` field, so raw and Apple libraries do not share a store entry.
- No GPU test and no device test covers the iOS raw libraries.
  Of a sample of 1,000 shaders from two titles, 997 built and Apple's `air-validate` accepted all 997 for iOS. The 3 others were rejects.
- The run note of the shader step counts the iOS raw libraries and fallbacks.
- The docs describe the current code. Claims about features that do not exist are removed.
  The chain version V, the translator identity, the archive batches and the security gaps now match the code.

### Added

- `NXB_OWN_RAW_IOS=0` turns the raw tier off for the iOS libraries only. `NXB_OWN_RAW` and `NXB_OWN_FRONT` also apply to iOS.

## 0.6.5

### Changed

- The shader step needs less CPU time and memory.
  On the reference title the standalone `nxb shaders build` took 327 s and 2,222 s of CPU time with 0.6.4.
  The `air-bc` changes below bring it to 170 s and 1,115 s. All changes of 0.6.5 bring the CPU time to 879 s.
  Peak memory went from 1.5 GB to 1.2 GB.
- `air-bc` runs `mem2reg` in one pass over the definition blocks, with an undo log instead of a map for each block.
  It also uses an Fx hasher, a structural key for CSE and a cache for TBAA nodes.
  The writer finds constants and metadata through a hash index. Dead code removal uses a work list.
  The output stays byte-identical.
- Fewer shaders go to Apple's `metal`. On the reference title Apple makes 35 libraries. With 0.6.4 it made 1,079 of 23,601.
  On a second title 14,780 of 14,796 shader sources build in process and pass `air-validate`. Before, 10,276 did.
- The extract stage shows byte progress for a NUP and a dump, as it does for a package.
- On a virtual machine without the `vm.page` sysctls, the machine load reads memory from `host_statistics64`.

### Added

- The own front end compiles texture pools. A pool is an argument buffer with an `array<texture2d<float>, N>` or `array<sampler, N>` member.
- The own front end compiles the translator's helper `dm_pool_mark`, with one instantiation for each pool type.
  Its relaxed device atomics become `air.atomic.global.load.i32` and `air.atomic.global.or.u.i32`.
  Apple's IR wraps arrays and atomics in struct types. Thus pool libraries pass `air-validate`, but they are not byte-identical to Apple's libraries.
- The own front end compiles `reinterpret_cast` from pointer to pointer and from integer to pointer.
- The own front end accepts `mulhi`, `simd_shuffle`, `simd_any` and `simd_all`.
  It also accepts the `front_facing` stage-in field and the `thread_index_in_simdgroup` parameter.
  `calculate_clamped_lod` and `calculate_unclamped_lod` work on `texture2d_array` too.
  For these builtins the IR has the same intrinsics, attributes and metadata as the IR of Apple's front end.
  Apple's front end writes a `freeze` of the data operand of `simd_shuffle`. The own front end does the same, and `air-bc` writes `freeze`.
- `air_front::FORMAT` is 8.
- `airc rawcheck` writes the reason of a reject or a failed build (`error`) and the SHA-256 of each library (`lib_sha256`).

### Fixed

- A full shader build for iOS wrote no stamp to the iOS library. The framework step then rejected the output with "their stamp: none".
  The build now writes the game ID and profile digest to the Mac and the iOS library.
  A rebuild removes old iOS outputs. An interrupted or key-limited rebuild leaves no iOS stamp.
  The iOS packager takes the guard tables from the work folder of the build, not from the game profile. (#10)
- The raw path failed with "unknown %v1" for a function with code after its last `return`.
  `mem2reg` removed the variable but kept its loads and stores in the block that no path reaches.
  These blocks now lose their accesses to promoted variables, and their loads read `undef`.
  The other raw libraries stay byte-identical.
- A call of a user function got the attributes of `dfdx` and `dfdy` (`convergent nounwind willreturn`) when the shader also used one of them.
  The two attribute groups had the same number. The call has `convergent nobuiltin "no-builtins"`, as in Apple's IR.
  On the reference title 2 libraries changed for this reason.

## 0.6.4

### Changed

- The pipeline archive starts compiler runs by the load of the Mac. 3 runs always go when the memory budget holds them. More start while CPU and memory use stay below 90 %, up to one run for each core and what the memory budget holds.
  Before, 3 runs went while the shader step ran, and more only after it.
- The compiler runs of the archive have nice 10. The shader step keeps the cores that it needs.
- The default of `--threads` comes from the cores of the Mac, `round(P + 0.4 × E)`. Before, it was 10 on every Mac.
  An M2 Max still gets 10. An M1 gets 6, and an M4 Max with 12 P cores gets 14.
  `nxb shaders build --jobs` has the same default instead of a fixed 12.
- The archive starts at most as many compiler runs as the memory budget holds, at least one.
  Before, 3 runs always started. On a Mac with 8 GB the budget holds one run of about 2 GiB.
- The new option `--load-limit PERCENT` sets the limit for CPU and memory. The default is 90.
- The archive line of the summary says how many compiler runs went at once.
- `nxb-sched::Headroom` admits runs by load. `nxb-sys` reads the CPU ticks, the memory in use (as Activity Monitor's "Memory Used") and the memory pressure.

## 0.6.3

### Fixed

- `nxb build` stopped in the Mac shader step with "own front end: .../data/own-front: No such file or directory" in every release binary.
  The default verdicts folder is a path on the build machine, and the step read it as a verdict file.
- Without an own-identical allow list for this front end and toolchain, the own front end stays on and only the raw path runs, as the README says.
  Before, a missing list turned off all own tiers, and Apple's front end made every library. A verdict file given with `--own-front <file>` must still exist.
- Measured on one title (16,268 shaders, cold store, M2 Max): the shader step took 5.2 minutes with the raw path and 10.3 minutes without it.
  The shader step and the pipeline archive together took 7.3 minutes instead of 11. The pipeline archive is 7 % larger.
  The game looked the same in a check by eye. This is not a GPU test.
- The vertex variants step their loops with `+= 1` instead of `++`. The own front end rejects `++` and `--`, so the raw path sent these shaders to Apple.
  `TRANSLATOR_ID` is `msl-2.3`, so the store translates the shaders again.
- The translator writes the helper functions with every callee before its callers and without prototypes. The own front end rejects prototypes.
  Prototypes stay only when the calls form a cycle.
- The own front end accepts `#pragma clang fp contract(off)` at file scope and at the start of a block. It writes no `llvm.fmuladd` where the pragma is in effect, as clang does.
  On 2 shaders with the pragma, the IR after `air-opt -O1` had the same counts of `fmuladd`, `fadd` and `fmul` as Apple's front end. `air_front::FORMAT` is 7.
- A scan of all 31,433 translated libraries of one title found 11,729 rejects by the own front end: 10,148 with `++`, 1,016 with prototypes and 867 with the pragma.
  63 rejects remain (`front_facing`, `point_coord`, a `threadgroup` value and `texture2d_ms`).

## 0.6.2

### Changed

- The rewrite step follows the engine `88c7e03f`. `nxb rewrite` no longer stops with "patch_nso.py of the engine differs from the version this rewriter was checked against".
  The pins have 13 files. `ue_ring.py` is new, and `patch_nso.py` and `nso_match.py` have new hashes.
- The finder `ue_ring` in `nxb rewrite`. A main module of Unreal Engine 4 with the ring of 3 copies for dynamic buffers gets 4 copies, as `dm/tools/ue_ring.py` does.
  The output is byte-identical to the script on 11 synthetic main modules (`crates/nxb-rewrite/tests/ring.rs`).
- The matcher reads the word `0xe7ffdefe` as `brk #0`, and the fingerprint still covers the word itself.
  It checks each rule like `nso_match.validate` and accepts a replacement branch that stays inside the matched function.
- `data/rewrite-rules` is made again from the engine `88c7e03f` (149 files). It has one new patch, two new game profiles and one changed `game.tsv`.
- The output is byte-identical to the script of the engine `88c7e03f` on 6 real modules (`main`, `rtld` and `sdk` of the base package and the update of one title).

### Fixed

- The extract stage of a package build reports the bytes written with `--progress json`. A launcher shows a bar with the time left. Before, it only showed that the stage was running.
- The extract stage is named after the versions it extracts, for example `extract v0 + v327680`, as the script names it. NX2Apple showed the bare name `extract` as "%@ extrahieren".
- `nxb_extract::Request` has a `progress` field with a byte counter (`nxb_extract::Counter`).

## 0.6.1

### Fixed

- `nxb build` reads the engine folder from `NXB_ENGINE` when `--engine` is not given. NX2Apple passes the engine this way. Before, the build stopped with "--engine <engine folder> is required".

## 0.6.0

### Changed (read this first)

- One Rust translator writes MSL directly (D20). The shader step calls `nxb_xlate::msl::translate` in the process for each key.
  Each library of the translation becomes `<key><suffix>.metal`, a `.metallib`, and a `.json` when it has a reflection.
  The translator fills the reflection. Nothing parses MSL text.
- The translation store key uses the translator identity (the chain version V, which covers the sources of `nxb-xlate`).
  Entries of the C# translator and `spirv-cross` are not used.
- A vertex shader waits for its pixel shaders and gets their varyings from the translator (`StageInputs`).
  It no longer reads `_b.metal` files.
- The `shaders done` event has the fields `xlate_hits` and `xlate_refused`. The fields `xlate_starts`, `xlate_restarts` and `xlate_fallbacks` are gone.
- The output is not byte-identical to the output of the reference build scripts. No game ran with it yet. Unit tests and Metal compile tests on built samples cover it.

### Removed

- The C# translator bridges: the `Translator` trait, the translator host, the process bridge and the Rust bridge in `nxb-tool`.
- The tools `spirv-cross` and `dotnet`. `nxb self-check` does not look for them.
- The options `--xlate`, `--xlate-host`, `--xlate-rust`, `--dotnet-root` and `--spirv-cross`, and the variables `NXB_XLATE_RUST` and `NXB_XLATE_HOST`.
- The command `nxb shaders recompile`. It depended on the MSL text passes.
- The SPIR-V generator: the crate `nxb-spvgen` and the module `nxb-xlate::codegen` (the shader definitions in `codegen::props` move to `nxb-xlate::props`). With it go the `.spv` and `res.json` outputs of the driver, the SPIR-V lowering of one vertex shader, the `<key>.iface.json` writer and the `xlate-diff` tool.
- The MSL text passes of `nxb-shader` (`msl`, `reflect`, `variants` and the side unit builder). The job writer, the chain version, the guards and the side library cut stay.

### Added

- Transform feedback in the MSL back end. A vertex job with `tf` gets the entry `main0_tf` in `_b`, and a geometry job gets it in `_bgv`. The `.json` has `entry_tf` and `tf_strides`. `Job.tf` holds the buffer layout of the job.
- `nxb build --platform mac` (the development build) and `nxb build --program N` for N more than 0. They no longer give exit code 4.
  `--platform mac` ends with the command that runs the game on the Mac. The pipeline prebuild, which starts the host, is not run.
  The other programs of a multi-program profile are built for `mac` and `mac-app`. The first program is in `programs.tsv` (`mac`) or in the app (`mac-app`).
- `nxb extract --program <title ID>` picks the Program NCA of one program of a multi-program application.
- The finder `ue_region` in `nxb rewrite`. A main module with `SwitchVirtualAddressSpaceSize` no longer gives exit code 4.
  The output is byte-identical to `dm/tools/ue_region.py` on 13 synthetic main modules (`crates/nxb-rewrite/tests/region.rs`).
- The census of the rewrite sites (`dm/tools/insn_census.py`) as `nxb_rewrite::census`. The survey report of `nxb build` and `--census` of `nxb profile guess` have it now.
  The output is identical to the script on 3 synthetic modules. The `--sites` file of the script is not ported.
- The source checks of `nxb build` read the files of `RomFS.img` (Unreal paks with IoStore tables, Frostbite, PKZ, GFPAK, Unreal 3, SPIR-V, GLSL, Trinity and the NRO imports),
  as `profile_guess.build_sources` does. The counts are identical to the script on a synthetic RomFS (`crates/nxb-build/tests/sources.rs`).
- The split reads zip members compressed with bzip2 or lzma. The decoders are `nxb-split::bzip2` (new) and `nxb-split::unity::lzma`.
- The split reads BND3 archives whose entry names are Shift JIS with double-byte characters. The decoder is `nxb-split::sjis` (new). Its table is made by `tools/gen_sjis_table.py`.
- The split reads the CRA archive family (`nxb-split::cra`, new). The file tables come from the NROs of `DM_NRO_DIR`, and each NRO is scanned like main. New `jobs::plan_with` takes the NROs. It was checked with a synthetic fixture made by `tools/gen_cra_fixture.py`.
- The extract reads a sparse base program together with its update NSP (`nxb-extract::sparse`, new). The base alone still stops with exit code 4. The reader follows the Atmosphere sparse storage and was checked with a synthetic fixture.

### Fixed

- The profile header follows the current engine `game.py`. It has the keys `fps_max` (`DM_GAME_FPS_MAX`) and `draw_barriers` (`DM_GAME_DRAW_BARRIERS_GAME`) now.
  The key `tex_bc_cpu` is gone. The headers of all profiles, the multi-program header and the app settings files are identical to the engine scripts again.

## 0.5.1

### Changed (read this first)

- **The Rust translator is off by default.** The entries of 0.4.0 and 0.5.0 said that it is on by default. That is not correct for this release.
  The Rust translator does not follow the current engine translator, for example its Maxwell denormal flush.
  Its output can thus differ from the output of the C# translator on the current engine.
  `--xlate-rust on` (for `nxb shaders build`) or `NXB_XLATE_RUST=1` (also for `nxb build`) turns it on.
  In the default mode, the translator host does the work, and the process bridge is the fallback. Both are C# programs, thus the .NET runtime is necessary.
  The Rust translator was byte-identical to the C# translator only on the engine version that it was ported from. Nobody compared it again with the current engine.
- **The chain value in each `_b.json` file is the computed value.** In 0.5.0, the vertex shader files had the constant `0ad559cdbb28`.
  The folder name had the computed value, and the two values were not equal. As a result, the pipeline archive found no base library.
  In a full Mac run with the 0.5.0 code, the archive step gave an error. The build packed the app without an archive.
  The computed value uses the engine files that `dmsh_chain.py` names.
- **`nxb rewrite` follows the current engine.** The script `patch_nso.py` changed in 551 lines after 0.5.0. This release ports the changes.
  The rules in `data/rewrite-rules` come from the current engine (147 rule files). The pins give the two hashes in the message.
  `nxb build` stops at the rewrite step when the engine files differ from the pinned files. It gives exit code 1 and the message. `nxb rewrite` gives exit code 4.
  The finder `ue_region` was not ported in this release.
- `nxb build` has more inputs, more options and fixed exit codes (the next section).

### Added

- Port of two changes of the MSL and the reflection step: the point size and the kinds of bindless handles.
- Rewrite step: the order of lock acquires (`ldaxr`), the Trinity barrier, `changed.tsv` as the sixth output and the observation gates at each instruction that can move.
  Also `BACK_MIN`, the recognizers of jump tables, the proof of tail calls and the finders `ue_cvar`, `ue_malloc`, `fps_target` and `snowdrop_pools`.
- The split reads NSO segments with ZBIC compression. The decoder is in `nxb-codec::zbic`.
- `nxb build`:
  - With `--progress json`, standard output has NDJSON events only.
  - The exit codes are 0, 1, 2, 3, 4, 6 and 130 (130 for SIGINT and SIGTERM).
  - The Mac host stage makes the font file. The shader stage reports `total` from its first progress event.
  - New inputs: NSZ, XCZ, XCI, merged packages (`--app`), `.nup` (`--nup`), dump folders (`--dump`) and DLC (`--aoc`).
  - New options: `--env-file`, `--motion`, `--seen-shaders`, `--allow-other-metal`, `--mac-sign`, `--romfs`, `--keep-extracted` and `--show-fps`.
  - The iOS options are connected to the iOS stages: `--team-id`, `--framework`, `--shell-framework`, `--prepare-xcode` and `--xcode-workspace`.
  - `--gpu` has a default: the GPU architecture of this Mac. `--gpu none` skips the pipeline archive.
    If the archive gives an error, the build writes a `[RED]` line and packs the app without an archive. The exit code is 0.
  - Provisional profiles. A game without a registered profile gets a survey and a provisional profile.
    The options are `--provisional`, `--survey-only`, `--no-heap-check` and `--profile-out`.
    A game that has a heap check, a region check or a shader capture gives exit code 4. The message names `--no-heap-check`.
- The command `nxb profile` with `guess`, `show`, `check` and `manifest`.

### Checked

All results are **measured**.

- Libraries and `_b.json` of a full Wonder run (C# translator, `NXB_OWN_FRONT=0`) against a Python build of the same current engine:
  0 of 41,883 libraries differ in a sample. 5 shaders are stubs that the Python run itself left.
- Rewrite: 111 of 111 modules are byte-identical to the current `patch_nso.py` (9 real modules of 3 titles, 4 synthetic main modules and 98 NRO modules).
- Profile files: 1,115 of 1,115 tables of 145 profiles are identical. The guess reports are identical in 11 of 11 cases.
  The survey reports and provisional profiles are identical in 6 of 6 cases. The checks of registered profiles are identical in 10 of 10 cases.
- The survey of the Switch 2 package, the profile, the rewrite (3 modules), the split (35,732 shaders) and the Mac host stage passed with `--no-heap-check`.
- A run that was stopped with SIGTERM after 4 s ended with exit code 130.
- The check script of the repository passes.

### Not checked

- **Nobody ran the shader step, the pipeline archive and the app stage on the Switch 2 package.** The checks above stop at the Mac host stage.
  Thus the shader step for a NUP package is not tested.
- The new inputs (XCI, XCZ, merged packages, dump folders and DLC) were checked on synthetic fixtures only. The NSZ block mode was not checked on real data.
- The iOS options and `nxb build --platform ios` were not run on a device. Each stage was checked with a real `xcodebuild`, but not in one run.
- `--mac-sign` was not run with a real identity.
- The complete `nxb build` was not run again after the fix of the chain value. A cold full run was not timed.
- The rewrite step was not checked again with the current script on TotK, BotW and the Smash main module (the images are not extracted).
  The finders `ue_cvar`, `ue_malloc`, `fps_target` and `snowdrop_pools` have no real positive sample.
- The exit-code-4 paths of the unified commands have no test.

### Not ported (open items)

- The Rust translator must follow the current engine translator before it can be the default again.
- The paths that go to the C# translator (geometry, tessellation, vertex shaders as compute, transform feedback, task and mesh shaders, `SharedStoreSmallIntCas`) stay open. Refer to the README.
- The census of the rewrite sites, and `--program N` for N more than 0.
- The heap check, the region check and the shader captures of the survey. They start the game.
- DLC shader content is not split. The shader step does not write cache entries at this time.

## 0.5.0

### Changed (read this first)

- **At this time, there is one program: `nxb`.** The release ZIP file holds `nxb`, the folder `rewrite-rules`, `LICENSE` and `THIRD_PARTY.md` and nothing else.
  The separate program names are gone: `nxb-extract`, `nxb-split`, `nxb-ios`, `nxb-macapp`, `nxb-ipa`, `nxb-pack`, `nxb-pso`, `nxb-archive`, `shaderbuild` and `airc`.
  There are no links and no other names. The program does not use its own file name to select a command.
  **This breaks a caller that runs a program by its old name.** Use the command instead. The command line after the command name is the same as before,
  and the exit codes are the same.

  | old program | new command |
  |---|---|
  | `nxb-extract` | `nxb extract` |
  | `nxb-split` | `nxb split` |
  | `nxb-ios` | `nxb ios` |
  | `nxb-macapp` | `nxb app` |
  | `nxb-ipa` | `nxb ipa` |
  | `nxb-pack` | `nxb sign` |
  | `nxb-pso` | `nxb pso` |
  | `nxb-archive` | `nxb archive` |
  | `shaderbuild` | `nxb shaders` |
  | `airc` | `nxb airc` |

- The usage text of `nxb shaders` and its error lines name `nxb shaders`. They do not name `shaderbuild` any more.
- Some error lines still start with an old program name, for example `nxb-extract:`, `nxb-split:`, `nxb-archive:`, `nxb-macapp:`, `nxb-ios:` and `airc:`.
  Only the text is old. The exit codes stay the same. A script must not read these prefixes.
- If you downloaded the ZIP file with a browser, run `xattr -dr com.apple.quarantine <folder>` once on the unpacked folder.
  A download with `gh release download` or `curl` has no quarantine flag.

### Added

- The command `nxb airc`. It is the own front end compiler (`emit`, `bc`, `check`, `build`, `rank`, `deny`, `repack`, `time`).
  The crate `airc` has a library entry point, `airc::main`. The layer rule lets `nxb-cli` use `airc` (ADR W23).
- The commands `nxb help` and `nxb <command> --help`. `nxb help` shows the commands in groups. `nxb version` shows the version.
- The commands `nxb prune` (the same as `nxb cache prune`), `nxb regressions` and `nxb self-check`.
  `nxb self-check` finds the external tools and shows their versions. It returns 1 if a required tool is missing.
- The release script checks the ZIP file: it holds `nxb`, the rules and the two text files, it holds no link and no second program,
  `nxb` has a valid signature, and the checksum matches.

### Checked

- The test `stage_commands` of `nxb-cli` runs the program on small inputs. It checks the exit codes for a missing argument or a missing file
  of `extract`, `split`, `ios`, `archive`, `pso`, `ipa`, `app` and `sign`, the IPA writer on one text file, and `nxb airc emit` on one kernel.
- The check script of the repository (format, clippy, tests, documentation, layer and size rules) passes.

### Not checked

- Exit code 4 (unsupported input) of the unified commands was not run again with the new command names. The code is the same as before, but no test covers it.
- The new command names were not used with real game data. The earlier real-data results in this file and in the README are for the old program names.
- The exit codes are the same as before only by design and in the small tests above. No full comparison exists.

## 0.4.0

### Changed (read this first)

- **The raw container tier is now on for each Mac shader that the own front end accepts.** In 0.3.0 it was on only for shaders on an allow list.
  The tier makes the library itself and does not start an Apple tool. If the front end rejects a shader, the shader goes to Apple's compiler.
  The shader also goes to Apple's compiler when the library does not pass a check in the process.
  **The raw libraries are not byte-identical to Apple's libraries.** A GPU test was done for a part of the corpus only.
  It covered 3,625 of 23,752 candidates (about 15 %). In that test, 3,375 shaders gave the same output as Apple's library and 220 gave a different output.
  Nobody ran a GPU test for the other shaders, thus the rate of different output for them is not known.
  A smoke test of the GPU harness on 8 shaders gave 7 equal and 1 different (the largest error was 166 ulp). This is not a sample and gives no rate.
  To turn the tier off, set `NXB_OWN_RAW=0`. To turn off all own tiers, set `NXB_OWN_FRONT=0` or use `--own-front off`.
  `NXB_OWN_RAW=list` gives the old behavior (only the shaders of a raw allow list, which is not in this repository, thus the tier stays off unless you give a list).
  The cache key of a raw library has the field `own-raw@<front-end id>` for each shader. Entries from the 0.3.0 list mode are not reused.
- The exit code for an unsupported input is now 4 for `nxb extract`, `nxb-extract`, `nxb-split`, `nxb rewrite`, `nxb nro`, `nxb-ios` and `nxb build`. Exit code 3 means damaged data.
  The 0.3.0 text said that `nxb-ios` returned 3. The signer binary `nxb-pack` still returns 3 for an unsupported input.
- The translator host is the default when its binary is next to the translator (`--xlate-host off` turns it off).
  The Rust translator is on by default (`--xlate-rust off` or `NXB_XLATE_RUST=0` turns it off). **Version 0.5.1 reverses this: it is off by default.**
- Events have two new fields, `code` and `severity`. The old fields do not change, and `t` stays last.

### Added

- The code rewrite step in Rust (`nxb rewrite`, crates `nxb-a64` and `nxb-rewrite`). It has the command line of `patch_nso.py`. The extract stage of `nxb build` uses it.
  It reads NSO and NRO modules, including modules with ZBIC compression. The decoder `nxb-a64` follows Capstone 5 for the fields that the rewriter uses.
- The rewrite rules are data in `data/rewrite-rules` (117 rule files, `snapshot.tsv` and the licence notice of the engine that they come from).
  The release ZIP file has the folder `rewrite-rules` next to the binaries. `nxb rewrite` finds it there. `nxb rewrite-rules` makes the files from a snapshot.
- NRO collection in Rust (`nxb nro`, crate `nxb-nro`). It has the command line of `nro_collect.py`.
- Translator host as the default (`xlatehost` in `nxb-stages`). One host process does the work of all batches. The counters for starts, restarts and fallbacks are in the run report.
- Rust translator (crates `nxb-xlate`, `nxb-maxwell`, `nxb-spvgen`). It translates in the process, for Maxwell and for Ampere machine code (Switch 2).
  A key that it cannot do goes to the C# translator, and the reason is counted. The translator also writes `<key>.iface.json` (the `ShaderInterface`) next to `<key>.res.json`.
- `nxb-extract` and `nxb-split`: zstd dictionaries, many more readers (Unity bundles and serialized files, archive families with ranges, Needle, Sumo, Unreal Engine and more),
  and the options `--allow-damaged`, `--manifest` and `--aoc`. A format that is not ported stops with exit code 4 and a named message.
- iOS: games with more than one program and framework builds.
- Also for iOS: the button-icon font, the header of multi-program profiles, the own signer for the iOS executable and the reuse of the previous IPA.
  `nxb build --platform ios` runs the iOS flow with the usual events.
- `nxb cache export`, `import`, `prune` and `list`: the cache of a title as one export file with a checksum.
- Manifest-first order for `nxb build`: `--shader-manifest <file>` builds the listed shaders first. `--partial` builds only these shaders and writes `_b_partial.json`.
- The crate `nxb-profile` (the profile of a game as typed data with a TOML reader and writer).

### Checked

- Rewrite: byte-identical to `patch_nso.py` on 25 of 25 real modules (all five tables, `.patched.bin` and the fixups). The modules include one Switch 2 title,
  modules with ZBIC compression, modules with an NRO table and modules with frame step stubs. The 98 NRO modules of one title are also identical (98 of 98).
  With the rules from `data/rewrite-rules`, 120 of 130 earlier cases ran again and all were identical. 14 cases cannot run again, because their input files are deleted (their rules are unchanged).
- NRO collection: `nro.tsv` and 98 of 98 modules are byte-identical to `nro_collect.py` on one real image.
- Rust translator: SPIR-V and `res.json` are byte-identical to the C# translator for all keys of three titles, first-attempt jobs only
  (16,268 of 16,268, 19,592 of 19,592 and 28,669 of 28,669), and for 42,299 of 42,331 keys of one Switch 2 title (Ampere).
  Of the 32 other keys, 17 fail in the C# translator also and 15 go to the C# path (`SharedStoreSmallIntCas`). The corpus has no job for task and mesh shaders.
- Translator host: 802 of 802 results equal to the process bridge.
- Extract and split: one real update image with a 17.6 GB archive gave all package files and tables byte-identical to the reference split. Real zstd dictionaries: byte-identical to `zstd -D`.
  The readers for the other formats were compared with the reference functions on synthetic files.
- iOS: 142 of 142 profiles byte-identical in the profile port. The framework flow, the multi-program flow and the IPA reuse were compared with the reference script on synthetic fixtures
  (the framework IPA also with a real `xcodebuild` run: 47 of 47 entries equal, the executable, the framework binary and the font file equal in part).

### Only synthetic or only partly checked

- Unity readers, archive families without a local sample, layouts of very large text segments (100, 125.3 and 185.6 MB), three frame step builds, one build with registered cuts,
  framework builds and multi-program builds: only synthetic files or fixtures. The rules for real code of those builds are not tested.
- Switch 2 packages with an update and DLC. Ampere translation was checked on one title.
- `nxb build --platform ios` was not run end to end. The wiring is checked up to the first check of the inputs.
- `nxb-ios shell --build` was not run.
- `nxb cache` makes and reads export files. The shader step does not yet write cache entries for it.
- A full `nxb build` in one run with the new defaults was not timed.

### Measured and estimated

- Rewrite (release build, machine not quiet): 3.4 s for a 29 MB main module against 63 s for the script, and 8.9 s against 121 s for a larger one. Synthetic large modules: 1 to 3.5 s against 183 to 672 s.
- NRO collection: 3.5 s against 4.5 s. The archive table dominates both times, so there is no gain in speed.
- Translator host: 1.0 to 1.7 s against 6.3 to 8.4 s for 802 jobs (one start instead of 19). In a subset run of a shader step, the wall time was 20.2 s against 20.7 s, because the Metal compile dominates.
  The change of the .NET starts from 667 to about 1 in a full build is an **estimate**. Nobody measured it on a full build.
- Rust translator: in a subset run (160 keys, load average about 10) the wall time was 17 s against 16 s with the C# translator. The Metal compile dominates.
- Raw tier on all shaders: in a subset run (160 keys, 261 shaders), 363 library compiles gave 363 raw libraries with no fallback, and all 699 output libraries load in Metal.
  The wall time was 6.5 s against 14.4 s with the tier off. This is not a benchmark. The gain on a full cold build is **not measured**.

### Limits

- Not ported in the Rust translator (the key goes to the C# translator): geometry, tessellation, transform feedback, vertex shaders as compute, the interlock function match,
  `SharedStoreSmallIntCas` and the task and mesh jobs. No corpus has the first four, thus they are not ported blind.
- Not ported: the CRA archive family, and each format that needs Oodle (these stop with exit code 4). Also not ported: zip members in bzip2 or lzma, and encrypted Unreal indexes.
- The rewrite port stops with exit code 4 when the engine files differ from the files that the rules come from. It needs the same engine version.
- `nxb split`, `nxb app`, `nxb ipa`, `nxb pso` and `nxb sign` print "not implemented yet". Use `nxb-split`, `nxb-macapp`, `nxb-ipa`, `nxb-pso` and `nxb-pack`.
- The own-identical allow list is not in this repository. The raw path does not use a list by default.
- Mac only for the build tools. The raw tier is not available for iOS.

## 0.3.0

### Added

- iOS stages in `nxb-build::ios` and a new binary `nxb-ios`. It does the work of the reference iOS scripts with the same command lines
  and environment: `nxb-ios ipa [<out.ipa> | --prepare-xcode]` and `nxb-ios shell --prepare-xcode | --build`.
  `xcodegen`, `xcodebuild`, `codesign` and the other Apple tools stay external. Exit code 3 means unsupported: the caller runs the reference script.
- Stage overlap in `nxb build`. The Mac host starts at the same time as the extract. The companion app, the icon and the settings file
  start when the extract and the host are done, beside the shader step. The stream of the pipeline archive gets more workers
  (one for each slot, if the memory is sufficient) when the shader step stops. The events are the same as in a run in series, except the times.
- Raw container tier `own-raw` in the shader step (Mac only). The own front end writes the bitcode and the library container itself
  for shaders on a second allow list. No Apple tool and no process is necessary for these shaders.
  `NXB_OWN_RAW=0` turns the tier off. `NXB_OWN_RAW=<file>` selects a different list.
- The front end has an optimizer for the raw path in `air-bc` (`mem2reg`, removal of dead code, block-local CSE) and the commands `airc raw`, `rawcmp` and `rawcheck`.
- The release ZIP file holds `nxb-ios` in addition to `nxb`, `nxb-extract`, `nxb-split` and `airc`.

### Checked

- iOS stages, each against the reference script on real or synthetic inputs. The manifest, resource, embed and build-setting generators
  and the Xcode workspace writer: byte-identical. The shell project: all files equal, except the font file (equal in size only,
  because the font tool writes a time stamp). The IPA flow on one title: 75 of 75 entries equal in name, method, sizes, CRC and attributes.
  Two entries are equal only in part. In the executable, only `__LINKEDIT` differs (debug map paths and signature), and the bytes before it are identical.
  The font file is equal in size only. In this run each side used its own `xcodebuild`.
- Overlap: on one title, with extract, split and shaders reused, the event streams of the old and the new binary are identical (except the times).
  58,211 of 58,211 paths are present in both outputs. All files are byte-identical, except the two rebuilt Mach-O files (`LC_UUID` and debug map) and their two lines in `CodeResources`.
- Raw tier: the allow list has 3,365 shaders. Each one gave the same GPU output as Apple's library on 12 test vectors.
  The test covered 3,625 of 23,752 candidates (3,375 the same, 220 different, 29 not deterministic, 1 not built).
  The raw libraries are not byte-identical to Apple's libraries. They are different by design.
  On a sample of 40 shaders, the tier made 44 raw libraries with no fallback. The 61 other libraries were byte-identical to the reference.
  With the tier off, all 105 libraries were byte-identical to the reference.

### Measured and estimated

- Raw tier, measured on a small sample on a loaded machine: median compile CPU of 97.5 ms for each library, against 109 ms without the tier (about 11 % less).
  This is not the full corpus and not a full build. The estimate of 4 to 6 minutes less on a cold build is **not** measured.
- Overlap, measured on one run with reused extract, split and shaders: 133.0 s against 136.2 s. The change is small, because the host and the app stages
  are the critical path there. The gain on a cold build is an **estimate** (the host build of 22 to 30 s and the app parts leave the critical path).
  Nobody timed a cold build with overlap.

### Limits

- `nxb build` still takes only the Mac target. `nxb build` refuses `--platform` for iOS. The iOS stages run through `nxb-ios` only.
- `nxb-ios` returns exit code 3 for games with more than one program and for framework builds.
- The default name of the shell app is `NxbShell`. The reference script has a different default name. Set `DM_SHELL_NAME` to get the same name.
- Not run: `nxb-ios shell --build`, and an IPA with a pipeline archive. The IPA is always packed in full (no reuse of the previous IPA).
  The `codesign` of the host signs the executable. The own signer does not.
- The allow lists for the own front end and for the raw tier are not in this repository. Without a list, the tier is off.
  The raw list covers about 14 % of the corpus. The raw tier is not available for iOS (there is no GPU test for iOS libraries).
- After a change of the front end, make the raw list again and change the front-end id. If you do not, the cache keys stay the same.
- Not ported: code rewrite, button-icon font, header of multi-program profiles, zstd dictionaries.

## 0.2.0

### Added

- `nxb-extract` reads game cards (`.xci`), compressed packages (`.nsz`, `.xcz`) and dump folders (`--dump`).
  The zstd decoder streams the data and has an output limit. `--app <id>` selects one application in a merged package.
- Mac stages in `nxb build`: the Mac host (`make` and `clang`), the companion app, the app settings file and the app icon.
  The reference build scripts are the oracle.

### Checked

- XCI, merged packages and dump folders: synthetic fixtures only. The output is byte-identical to the reference extractors.
  No real sample was available.
- NSZ: one real file. The decoded NCA is identical to the output of the reference decompressor. The NCZ block mode was not checked on real data.
- App icon and companion app: byte-identical on one title.
- App settings file: 425 of 425 outputs are byte-identical for 142 profiles. One multi-program header is not ported.
- Mac host: the reuse key and the decoder inputs are byte-identical. The binary is not byte-identical.
  The reference script also gives a different binary on each run (`LC_UUID` and the debug map). The code part is identical.

### Limits

- `nxb build` end to end with the new stages was not run on a real title.
- Not ported: the button-icon font, the header of multi-program profiles, zstd dictionaries, code rewrite.

## 0.1.1

### Added

- Reads Switch 2 packages (`.nup`) without keys. Same output layout as the reference script.
  Use `nxb extract --nup <game.nup>` or `nxb-extract` with the same arguments.
- Checked on one real package and on synthetic containers, including damaged ones.
- Not checked on real data: packages with base game, update and DLC, and the control data.

## 0.1.0 (first public release)

### Added

- Foundation and runtime crates: ids, hashes, errors, system calls, events, process runner, store, scheduler, parallel I/O.
- `shaderbuild`: the shader step in Rust (`build`, `recompile`, `guards`). On the reference title, 74,421 of 74,422 output files
  are byte-identical to the reference build.
- `nxb-extract` and `nxb-split`: NSP extract and shader split. Compared on three titles.
- `nxb-pack`, `nxb-macapp`, `nxb-ipa`: Mac app stage, ad-hoc signer, IPA writer. The signer matches `codesign` on the tested bundles.
- `nxb-pso`, `nxb-archive`: plan and merge of the pipeline archive.
- `nxb`: the orchestrator that runs the ported stages in one process graph.
- Own front end in layer F: `msl-lex`, `msl-parse`, `msl-sema`, `air-emit`, `air-bc`, `air-lib`, `air-front`, `airc`, `mslcheck`.
  It is on only for shaders on an allow list. A switch turns it off.
- Robustness tests for the NSP and NCA parsers and the zstd decoder.
- Release workflow that builds, signs (ad-hoc) and publishes the binaries for macOS on Apple silicon.

### Limits

- Mac only.
- Not ported: code rewrite, host app build, app binary build, app settings file and icon, XCI and NSZ input, geometry and mesh shaders.
- Verified on one reference title. The shader step for iOS was compared on a sample only.
- The pipeline archive step is slower than the reference scripts on the reference machine (273 s against 257 s).
- The reference scripts remain the fallback for every stage that is not ported.
