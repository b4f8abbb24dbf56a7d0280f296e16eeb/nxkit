# Decisions

This document records the architecture decisions and their causes. Part 1 has the workspace decisions W1 to W23.
They apply where they are different from the two design proposals. Part 2 has the design decisions of the shader step and of the other stages. Part 3 has the runtime decisions R1 to R16.

## Principles

All stages are in Rust, and the reference build scripts are only the test reference. A port gives byte-identical output.
The own front end is the one exception. The archive back end stays Apple's tool. The workspace layout sets names, layers
and shared APIs. The designs set the behavior.

## Part 1: workspace decisions

| # | decision | cause |
|---|---|---|
| W1 | One prefix `nxb-` for all crates. The program `nxb` comes from one crate. | One name space for one workspace. |
| W2 | `nxb-model` (identifiers, digests) and `nxb-error` (classes, reason strings) are two crates in L0 and in V. | The other stages use identifiers and errors without the shader ABI constants. |
| W3 | Own SHA-256 in `nxb-model`. Tests compare it with FIPS vectors and Python `hashlib`. | No network, no vendored code, one implementation for all systems. |
| W4 | Superseded by D20. The `Translator` trait was in `nxb-tool` (L2). | The trait returns `ToolError`. Process and server bridges are its implementations. |
| W5 | Events are in their own crate `nxb-obs`. | The scheduler, the tool layer and all stages write events. |
| W6 | Store format 1 is the own content-addressed store. A compatible tree reader is an adapter. Format 2 has segment files. | This prevents two meanings of "v1". |
| W7 | Four layers L0 to L3 with an explicit list of sideways edges. | One rule set and one checker. |
| W8 | `nxb-stages` holds the shader step. `nxb-cli` holds the program `nxb`. The other stages are L3 crates (W19). | Fewer crates. |
| W9 | The NSO parser is part of `nxb-rewrite`. The format modules start in `nxb-split`. | Fewer crates. The names stay reserved. |
| W10 | The ad-hoc signer started as a standalone crate. It is part of `nxb-pack` (L3) at this time. | Parallel work with its own file set. |
| W11 | Single-flight uses continuations (`claim` with `on_ready`), plus `get_or_run`, which waits. | A worker that waits is an idle core. |
| W12 | Scheduler queues are `Mutex<VecDeque>` with a priority injector. | Standard library only. Tasks take milliseconds, thus the lock cost is small. |
| W13 | `rust-version` in the workspace, no `rust-toolchain.toml` at this time. | The build machine has no rustup. |
| W14 | Withdrawn. A checkout compiles into `./target`. `CARGO_TARGET_DIR` and `CARGO_BUILD_JOBS` change this. | |
| W15 | `Budget` calculates tokens from the P and E core counts and the factor e. | The budget follows the metric (PCE). e is 0.4 until a measurement replaces it. |
| W16 | The crates `nxb-model`, `nxb-error`, `nxb-maxwell`, `nxb-pyjson`, `nxb-shader`, `nxb-air` and `nxb-xlate` are in V. They have `in_v = true`. They use only crates in V. V itself is a hash of engine files (refer to [early-cutoff.md](early-cutoff.md)). The other stages have their own format versions. | `nxb-error` holds reason strings that go into `_b_build.json`. |
| W17 | `nxb-model` uses `nxb-sys` for the ARMv8 SHA-256 block function, with a scalar fallback. | Measured: 1,841 MB/s against 225 MB/s on 1 GiB. The intrinsics are `unsafe`, and only `nxb-sys` has `unsafe` code. The digests are the same. |
| W18 | Shader front ends are a `FrontEnd` trait in `nxb-tool`: `AppleFrontEnd`, `OwnFrontEnd`, `WithFallback`. The verdict data is data of the shader domain. The default uses the allow list (refer to D18). | The front-end id keeps the cache entries apart. |
| W19 | The stage crates with I/O (`nxb-extract`, `nxb-pack`, `nxb-archive`) and the orchestrator `nxb-build` are L3 crates with listed sideways edges. | Each stage crate has one owner. |
| W20 | The front-end crates are a separate layer F with their own names. | They are standalone and use only the standard library. Only `nxb-tool` can use `air-front`. Since W23, `nxb-cli` can also use `airc`. |
| W21 | Early cutoff: each stage output has a key from the stage version and the content hash of its inputs. Only the translator key contains V. A V change translates all shaders again. It compiles only the shaders with a changed MSL. A V change does not delete data. | A change of one line in the chain must not compile all shaders of all games again. Measured in 0.1.0: a V change alone took 47 s with 0 compiles (refer to [early-cutoff.md](early-cutoff.md)). |
| W22 | Superseded by D20. The shader step had three ways to translate a key: the Rust translator, the translator host and the process bridge. Since 0.5.1, the Rust translator is off by default. Without it, the step uses the host first, then the process bridge. With it on, the step uses it first. Each fallback is counted by reason. | The Rust translator does not follow the current engine translator (for example the Maxwell denormal flush). Thus its output can differ from the C# output. It also does not cover geometry, tessellation, transform feedback, vertex shaders as compute and some rare cases. The C# ways give the same bytes. This reverses the decision of 0.4.0 to turn it on by default. The decision stays reversed until the changes are ported and compared again. |
| W23 | There is one program, `nxb`. Each stage is a command. The stages have no separate programs and no dispatch by `argv[0]`. The front end compiler is the command `nxb airc`. `nxb-cli` can use `airc` (a layer F entry point). | One file to sign, ship and update. A caller names the command, not a file. |

## Part 2: design decisions

### Shader step

| # | decision | cause |
|---|---|---|
| D1 | V is a hash of the engine files that `dm/gfx/tools/dmsh_chain.py` lists in `FILES`, plus the translator options. | A runtime fix must not make libraries invalid. A domain change must change V. |
| D2 | Binary provenance in a lock file. BIN_ID in the reuse key of the build. This is the design. The lock file check is not in the code at this time. | This separates the code that made the bytes from the binary that ran. |
| D3 | The own front end is not byte-identical. Image correctness validates it. | Unoptimized IR gave the same library in only 2 of 300 samples (measured). |
| D4 | Three gates: `gpudiff` with class tolerances, coverage, scene comparison. | Each gate finds a different error type. |
| D5 | Fallback to the Apple front end for each shader. `NXB_OWN_FRONT=0` sets the Apple front end for all shaders. | An incorrect shader costs one fallback, not a release. |
| D6 | First back-end path: unoptimized IR into Apple's back half. | This keeps Apple's AIR passes. The back half took 57 ms median (measured). |
| D7 | Front-end id in TOOL_FP and in the Metal cache identity. | Apple and own libraries do not mix. |
| D8 | `SDKROOT` for each target and a fixed environment allow list. | A direct call without `SDKROOT` gives different bytes (measured). |
| D9 | Superseded by D20, which removes the MSL text passes. The shim for Python regular-expression semantics is not built (`nxb_shader::re` is empty). | 19 back-references and look-arounds. A verbatim port is the safest path to parity. |
| D10 | Error classes and a circuit breaker. | A full disk must not become thousands of shader failures and an incorrect reuse key. |
| D11 | Manifests with SHA-256, quarantine, dirty marker. | This finds damage and keeps the evidence. |
| D12 | Process groups and division of bad batches. | One bad shader costs log2(batch) runs. |
| D13 | Memory budget from the memory size and the peak memory of the previous build. | Games with 130,000 shaders and Macs with 8 GB. |
| D14 | CPU use in PCE. | E cores are slower. A core count hides a P-core shortage at the end. |
| D15 | Drift samples. | They find nondeterminism, damage and toolchain changes at approximately 0.5 % cost. |
| D16 | Typed task graph in memory. Each external tool is behind a `Tool` adapter. The first adapter is a child process. | A tool changes to a persistent worker only when a benchmark shows a better time and parity stays. |
| D17 | No own optimizer and no own `metallib` writer. | No pass subset in the test gave Apple's machine code (0 of 15). |
| D18 | The own front end uses the allow list by default: only shaders with the verdict `own-identical` in the shipped verdict file. | With a deny list, a shader that is not in the corpus goes through the own path without a check. |
| D19 | Since 0.4.0, the raw path is on by default for each Mac shader that the own front end accepts. It has a check in the process and a fallback to Apple. `NXB_OWN_RAW=0` turns it off. It supersedes D18 for the raw path only. | The path is faster and needs no Apple tool. The cost is that its output is not byte-identical to Apple's output, and a GPU test covered only about 15 % of the corpus. D17 stays for the identical path. |
| D20 | One Rust translator writes MSL directly. `nxb-xlate::msl` emits each library from the structured program and fills a typed `Reflection` for `_b.json`. The C# translator, the SPIR-V generator, `spirv-cross` and the MSL text passes are removed. The host contract stays as it is: argument buffer IDs, `main0` and its extra entries, the library suffixes and the `_b.json` fields. | One code path for all stages, no .NET and no external translator binary. Facts come from the emitter and not from parsing MSL text. The contract changes later, one step at a time and only after a profile of the host shows a gain. Output is not byte-identical to the C# path. |
| D21 | Vulkan and OpenGL games get their Metal shaders from `nxb-layer`. naga reads SPIR-V and writes MSL. The `glslang` crate turns GLSL into SPIR-V first. The output has the format of `dm/gfx/tools/vk_shaders.py` and `gl_shaders.py`: `vk_shaders.metallib` and `vk_shaders.tsv` (or `gl_shaders.*`), function `vk_<sha256[:16]>`, the same slot numbers and the same table rows. | D20 removed `spirv-cross`, and the reference scripts need it. naga and `glslang` build with Cargo and need no external binary. The MSL text differs from the `spirv-cross` text. The host reads only the function names and the table, so the text does not have to be equal. |

### Other stages

| # | decision | cause |
|---|---|---|
| E1 | Rust extract, one process, all versions in one pass, shared block cache, parallel `pwrite`. | The C# path reads, decrypts and examines the base two times. |
| E2 | Own AArch64 decoder `nxb-a64`. | The rewriter uses only register and memory operands. A C library is then the only native build dependency. |
| E3 | Own ad-hoc signer. `codesign` is the reference and the fallback. | `codesign` reads 4.7 GB again. The own signer uses the known digests. |
| E4 | The split operates on the data of the extract. | The stages read each file only one time. |
| E5 | One hash for each byte, at the producer. | No byte gets a second hash. |
| E6 | Archive batches as tasks under a memory budget, with the same merge sequence. Each batch runs `applegpu-nt -j 1` and `air-pack`. The number of runs follows the machine load and the memory budget. If the two tools are not beside `metal-tt`, each batch runs `metal-tt`. | Measured: 207 s against 257 s cold, and the loader probe found all 18,300 bases. |
| E7 | One `nxb build` with one task graph. Stage commands first, as a migration step. | One graph lets the extract feed the split and the shader step feed the archive. |
| E8 | Own zstd decoder in `nxb-split`, standard library only. | NSZ, XCZ and `.zs` files use zstd. The workspace has no external crates. |
| E9 | No own GPU code generator. | Closed plugin, private format and ISA (refer to [research/backend-survey.md](research/backend-survey.md)). |

## Part 3: runtime decisions

These decisions are for the nxkit host, which runs a game without nxnative. The plan is [plans/nxkit-host.md](plans/nxkit-host.md).

| # | decision | cause |
|---|---|---|
| R1 | The shader translator of the host is `nxb-xlate`. | It already writes the Mac shader libraries of every build (D20). The host reads the same reflection as the build. |
| R2 | The rewrite rules are nxkit's own. The core and the title module hold them. | `nxb-rewrite` already holds its rules as data. A rule for one game stays in the module of that game. |
| R3 | Guest memory is one no-copy `MTLBuffer`. The GPU address the game sees is the `gpuAddress` of that buffer plus the offset of the host address. A shader reads guest memory through a `device` pointer made from that address. There is no translation table. | Measured on 2026-11-07 on an M2 Max with macOS 27.2: no-copy buffers of 4 and 8 GiB work, and `maxBufferLength` is 18.72 GiB. An MSL 4.1 shader read and wrote guest memory 3 GiB into a 4 GiB buffer through a `ulong` cast. A table costs a lookup on every address. |
| R4 | Render targets live only in Metal. They go back to guest memory only when the game reads those bytes. | Apple GPUs keep attachments in tile memory during a render pass. Breath of the Wild loads and stores about 800 MB a frame on nxnative. |
| R5 | `nxkit-metal` binds Metal in Rust with `objc2-metal`, at a pinned commit of `main` until a release has the Xcode 27 headers (madsmtm/objc2#892). The `unsafe` calls stay inside `nxkit-metal`. | Metal is the only native GPU API on the Mac. The crate is Zlib, Apache-2.0 or MIT, which GPL-3.0 can take. Checked on 2026-11-07: `main` has the Xcode 27.1 headers, and crates.io has 0.3.2 from 2025-10-04. The cost of one Metal call is the same for every binding. The number of calls sets the speed. |
| R6 | Each guest thread is one macOS thread. The structure of the game's threads does not change. | The game's own synchronisation assumes its threads. The M2 Max has 8 performance and 4 efficiency cores, more than the 3 cores a Switch game gets. |
| R7 | There is no global lock. A guest lock waits on its own word in guest memory with `os_sync_wait_on_address`. | nxnative runs every kernel call under one mutex `G` in `dm/host/hle_thread.c`, with 49 lock sites. A wait on the word itself shares no lock. The call exists since macOS 14.4. |
| R8 | A thread's QoS class follows its role, not its Switch priority. The title module names the roles. Render and audio run at user-interactive. Loading and streaming may run on the efficiency cores. | A Switch priority orders threads on one core. It says nothing about the type of core. nxnative runs every thread at user-interactive by default, because `DM_QOS_MAP` is off. |
| R9 | The core rules of Horizon (a thread runs before every lower priority thread of its core) are copied only where a game needs them. That is a fact of the family or the title. | nxnative copies them for every game by polling with 50 µs sleeps (`dm_yield_to_core_peers`). It added them for a few games. |
| R10 | nxkit's own work runs like a native game: a job system on the free cores, `os_workgroup` for the frame, and the GPU encoding of frame N while the game builds frame N+1. | The game's threads leave cores free. Encoding next to the game keeps the GPU busy without slowing the game. |
| R11 | A patch of a game's thread structure comes only if the game stays slow on the CPU after R6 to R10. | A patch changes the game and needs its own proof for each game. |
| R12 | `nxkit-host` owns the window. It uses AppKit and a `CAMetalLayer`. | This is the native path of a Mac game. |
| R13 | Saves have the same layout as on nxnative. If this costs speed, the layout changes. | A save moves between the two hosts while both are in use. |
| R14 | A service answers at the level of the SDK functions. The loader writes the address of the nxkit function into the import table of the game's module. Other calls reach an IPC server in nxkit. | A call is then a plain branch. An IPC request costs a message write and a parse on each call. |
| R15 | Nothing falls back in silence. A command the IPC server does not know stops the game with its name, service and command ID. | A silent fallback hides a missing service until a crash far from the cause. |
| R16 | Measurement is part of the host from the start: CPU and GPU time of every frame, and the count of every IPC command. | Step H3 is done only when GPU time is at or below nxnative. That needs numbers from the first run. |
