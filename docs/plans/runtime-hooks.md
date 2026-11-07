# Runtime hooks and the nxkit runtime

| | |
|---|---|
| Part A, runtime hooks and Ocarina of Time | ComicBit |
| Part B, nxkit runtime and Breath of the Wild | v0lumehi |
| Status | planned |

## Goal

- A title module decides how the host runs its game. Its needs become the host's per-game settings.
- nxkit replaces nxnative one service at a time. nxnative runs every service of a game until its module moves one.
- Work on one game lands in nxkit: in its title module, in its family or in the nxkit runtime.

## Who does what

| item | result | owner | needs |
|---|---|---|---|
| A1 | key table for needs | ComicBit | |
| A2 | needs reach `game_profile.h` | ComicBit | A1 |
| A3 | drift check against `game.tsv` | ComicBit | A1 |
| A4 | upscaler and pacing as keys | ComicBit | A1 |
| A5 | `nxb title` and the launcher | ComicBit | A2 |
| A6 | Ocarina of Time module | ComicBit | A2 |
| A7 | `TitleFacts.libraries` is filled | ComicBit | |
| B1 | `nxkit-rt` static library, built without a service | v0lumehi | |
| B2 | the host asks nxkit first | v0lumehi | A2, B1 |
| B3 | nxb links the library | v0lumehi | B1 |
| B4 | first services in nxkit | v0lumehi | B2, B3 |
| B5 | Breath of the Wild module | v0lumehi | A2 |

Each reviews the other's pull requests in `titles/nxtitle`. The interface between A and B is the key table (A1) and the service macro (B2).

## How the host works

These facts were checked on 2026-11-07 against nxnative `f25fa956` and nxkit `main`.

### Per-game settings

- Each game has `dm/games/<title id>/game.tsv`.
- `dm/games/game.py header` writes `dm/host/gen/<title id>/game_profile.h` with `DM_GAME_*` macros. The defaults are in `header_one`.
- The settings are fixed when the host is compiled. A few have overrides through environment variables, for example `DM_FPS_TARGET` and `DM_SCREEN_ASPECT`.
- `nxb` runs `make gen/<title id>/game_profile.h` in the engine and takes the host's compile line from `make -n` (`crates/nxb-build/src/host/machost.rs`).
- `crates/nxb-build/src/host/header.rs` is a Rust port of `header_one`.

### HLE functions

- The host registers its functions by their Switch mangled names in the Mach-O section `__DATA,swhle` (`dm/host/hle.h`, macro `HLE`).
- `hle_find` in `dm/host/hle.c` looks a name up in that section. With a duplicated name, the first entry wins.
- `HLE_FORCE` entries in `__DATA,swforce` replace a function even when the SDK defines it.
- Kernel calls do not go through names. The rewriter turns each `svc` into a gate, and `gate_dispatch` calls `svc_dispatch`.
- Switch code does not extend arguments smaller than 32 bits, and it passes variadic arguments in registers. A guest thread's stack can be 8 KB.

### Rewriter patches

`nxb` applies each game's `patches.py` from rule files in `data/rewrite-rules`. These files are generated from the engine's patches. `nxb` refuses to rewrite when `DM_GAMES_DIR` is set or an engine patch file changed (`crates/nxb-build/src/rewrite.rs`).

## Part A: runtime hooks (ComicBit)

### A1 Every need names a known key and value

`nxtitle` has a table of every `game.tsv` key a need may set, with its allowed values and its default from `game.py header_one`. A test fails when a family or title states a need with an unknown key or value.

### A2 The host's `game_profile.h` holds the needs

- A need fills a key that `game.tsv` leaves out.
- A need that contradicts `game.tsv` stops the build. The message names the key, both values and the module.
- `nxb build` prints which keys came from which module.

Open question: how do the needs reach the header? `DM_GAMES_DIR` cannot be used, because the rewrite step refuses it. One option is to write the header with `header.rs` instead of `make`. The other is a patch that lets `game.py` read an extra file.

### A3 Modules and `game.tsv` do not drift apart

A command compares the needs of every module with `game.tsv` at the engine pin. It lists each key that differs. It runs when the engine pin changes.

### A4 Upscaler and pacing have keys

The answers of `upscaler` and `pacing` become `game.tsv` keys, for example `fps_target` and `present_interval`.

Open questions:

- Which key does `Upscaler::MetalFx` set?
- Does `Pacing::Fixed` replace `fps_target`, or does it set it?

### A5 The launcher shows the module

- `nxb title <package> --json` prints the title id, the module, the family, the needs and the services nxkit runs.
- The launcher (`ExperimentalBuilder.swift` and `BuildStore.swift` in NX2Apple) reads it and shows the module in the build view.

Open question: is this a new command, or part of `nxb profile show`?

### A6 Ocarina of Time is answered by its module

- The module answers the open questions in `docs/titles.md`: pacing, the upscaler and which agl facts hold for Tears of the Kingdom.
- The seed list (`pso_manifest.tsv`) and the game's rewriter patches come from the module's `seeds` and `rewrite_rules`.

### A7 Families can be detected

`TitleFacts.libraries` holds the library names of the game's modules. A family's `detect` finds a game without a title module.

Open question: where do the names come from? The NSO's module name, its symbol table or the build IDs?

## Part B: nxkit runtime (v0lumehi)

### B1 nxkit builds a static library for the host

- The crate `nxkit-rt` builds `libnxkit_rt.a` for `aarch64-apple-darwin` and `aarch64-apple-ios`.
- It exports `nxkit_init(services)` and `nxkit_hle_find(name)` with the C ABI.
- It is the only crate allowed to use `unsafe`. The workspace forbids it everywhere else.
- Its functions follow the Switch calling rules above. Arguments under 32 bits are taken as `u32`. No function is variadic. Functions with large stacks switch to a host stack.
- The nxkit release ships the library next to `nxb`.
- `crates/nxkit-rt/include/nxkit_rt.h` is the header for the host. A test checks it against the exports and against `Service::ALL`.
- `nxkit_init` is thread-safe. The last call wins. Bits that name no service are ignored.
- A function is found only by the exact bytes of its Switch mangled name. A name of a disabled service gives NULL.
- The lookup table is empty. No service is implemented.
- Verified on 2026-11-07: `nm -gU libnxkit_rt.a` lists `_nxkit_init` and `_nxkit_hle_find` for `aarch64-apple-darwin` and `aarch64-apple-ios`. The iOS build needs the rustup toolchain, not a Homebrew `rustc`.

### B2 The host asks nxkit first

- An engine patch in NX2Apple's patch queue changes `hle_find`. It asks `nxkit_hle_find` first and its own section second.
- `game_profile.h` has `DM_GAME_NXKIT_SERVICES`, a bit mask in `Service::ALL` order. The host passes it to `nxkit_init`.
- `nxkit_hle_find` returns functions only for the services in the mask. With an empty mask the host runs as before.
- On the Mac the host does not link nxkit. It loads `libnxkit_rt.dylib` with `dlopen` when the mask is not empty. The path is `DM_NXKIT_LIB`, or the file next to the host. `DM_NXKIT_SERVICES` (a list of service names) replaces the mask for a test.
- A missing library or symbol writes one line to the log, and every service stays on nxnative.

Open question: should the patch go upstream to nxnative instead of the patch queue?

### B3 nxb links the library

On the Mac, `nxb build --platform mac` does not link the library. The run command it prints sets `DM_NXKIT_SERVICES` and `DM_NXKIT_LIB` (`NXB_NXKIT_RT`, or `libnxkit_rt.dylib` next to `nxb`). The host stage's key does not include the library, so a changed library needs no new stage. iOS links `libnxkit_rt.a`, because iOS cannot load unsigned code at run time.

### B4 Services move one at a time

| service | host files | addressed by |
|---|---|---|
| `offline` | `hle_net.c`, `hle_offline.c` | name |
| `fs` | `hle_fs.c`, `save_sync.c` | name |
| `input` | `hle_hid.c`, `hle_touch.c`, `hle_haptics.c` | name |
| `audio` | `hle_audio.c`, `hle_audren.c`, `hle_codec.c` | name |
| `system` | `hle_oe.c`, `hle_account.c`, `hle_time.c`, `hle_misc.c`, `hle_swkbd.c`, `hle_web.c` | name |
| `video` | `hle_movie*.c`, `hle_codec_video.c` | name |
| `pacing` | `hle_fpslock.c` | called on every present |
| `kernel` | `hle_svc.c`, `hle_thread.c` | gate dispatch |
| `gpu` | `dm/gfx/nvn`, `dm/gfx/gl`, `dm/gfx/vk` | NVN, GL and Vulkan entry points |

- `offline` is first. It is small and only answers "unavailable", so it proves the seam.
- Breath of the Wild is the pilot game. Its module moves a service with `RuntimeHooks::runtime` once a run of the game shows it works.
- A service may move with `Unverified` evidence while its code is written. The run of the fixed route at the end of part B turns it into `Recorded`, or moves the service back to nxnative.

Open questions:

- `pacing`, `kernel` and `gpu` are not found by name. What seam does each need?
- How does a test compare an nxkit service with the nxnative one?

### B5 Breath of the Wild is answered by its module

- The `FRAME_STEP` stubs, the interval controller cuts and the render scale stubs of `patches.py` come from the module's `rewrite_rules`.
- The integer output shaders of `shader_facts_manual.tsv` come from the module.
- Optimisations for the game go into the module, into the family `agl` when they hold for the engine, or into an nxkit service.

Part B continues in [nxkit-host.md](nxkit-host.md): Breath of the Wild on nxkit alone.

## Rules

- Every answer is a `Fact` with its evidence. A fact that was not checked is `Unverified` and says what would check it.
- nxnative is the default for every service of every game.
- A fix that helps more than one game goes into the core or the family, not into a title module.
