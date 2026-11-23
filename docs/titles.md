# Title layer

Four layers decide how one game is built and run.

- The core is the rest of the workspace. It knows no game, no engine and no graphics API of a game.
- A graphics family holds what is true for every game on one graphics API: `nvn` (NVN and NVN2), `vulkan` or `opengl`.
- An engine family holds what is true for every game of one engine, for example `agl` (Nintendo EPD's engine on sead,
  agl and gsys) or `unity`.
- A title module holds what is true for one game only.

Every title module names its engine family (`Title::engine`) and its graphics API (`Title::graphics`), also a game on
an engine no other game shares. The graphics family is the one of the API. So an engine or API fact always has its
place, and nobody has to remember to make one. A family depends on no other family and on no title. A title depends on
no family.

The API is the crate `nxtitle`. The crate `nxtitles` lists the built-in families and title modules.

## Folders

```
titles/
  nxtitle/              the API: traits, hooks, evidence, the catalog
  nxtitles/             the registry, one cargo feature per module
  engines/<id>/         crate nxengine-<id>, an engine family
  graphics/<id>/        crate nxgraphics-<id>, a graphics family (the id is the API's, `GraphicsApi::id`)
  games/<title id>/     crate nxgame-<short name>, with the game's profile.toml
```

## Who answers a question

Families and a title answer questions through hooks. `None` means no opinion.

- A single answer comes from the title first, then the engine family, then the graphics family, then the core default.
- A list adds up the other way round: the graphics family's entries, then the engine family's, then the title's.
- The rewrite rules of a family have the family's id as their scope. `nxb build` gives the rewriter the title's file and
  the families' files, and the rewriter searches the families' scopes after the title's (`Options::layers`).

| hook | trait | answer |
|---|---|---|
| `standalone` | `BuildHooks` | what `nxb build` needs to make the game: the version, the modules with their build IDs, the NVN function list, the shader split settings, the run settings and memory layout of the game (`game.json` keys `settings` and `memory`) and a romfs marker. A title that answers it builds without `--engine` |
| `shader_role` | `BuildHooks` | what a shader does for the frame, for example the game's own upscaler |
| `pipeline_seeds` | `BuildHooks` | the pipeline seeds (`pipelines.seeds`), which the build archives and ships, see [pipeline-stages.md](pipeline-stages.md#pipeline-seeds-of-a-title) |
| `rewrite_rules` | `BuildHooks` | rewriter rules as TOML text |
| `resource_patches` | `BuildHooks` | edits of one member of a game archive that `nxb build` applies to the app's RomFS, by a build option (`--resource-patch <option>=<value>`) or the patch's default. Lists of all layers add up |
| `console_vars` | `BuildHooks` | console variables (`ConsoleVar`, a name and a value) that the build sets. Lists of all layers add up, the title's after the family's, and a later value of a name wins |
| `console_var_files` | `BuildHooks` | turns the variables into new or changed RomFS files, for an engine that reads them from its files. The first layer that returns `Some` wins. It reads the game's files through `RomfsRead` (list a directory, size of a file, a range of a file) |
| `needs` | `RuntimeHooks` | host behaviours the game depends on, with the setting. `nxb build` writes the need `vertex_stream_null_bind` with the setting `ignore` as the key of that name in `settings` of `game.json`. |
| `upscaler` | `RuntimeHooks` | keep the game's upscaler, render at the output size, or use MetalFX |
| `pacing` | `RuntimeHooks` | the game, the display or a fixed rate paces the frames |
| `pace_profile` | `RuntimeHooks` | the keys that steer the frame step and the presents |
| `limits` | `RuntimeHooks` | where the game's own frame limit sits, so that the host can write the target into it |
| `temporal_view` | `RuntimeHooks` | the engine's layout for MetalFX temporal upscaling: the byte offsets of `ClipToPrevClip`, the jitter and the view size in the view uniform buffer, the velocity code (`scale`, `bias`, `y_up`, `zero_means_camera`) and `depth_reversed`. A family answers it only for an engine version whose layout it knows. Otherwise the title answers it |
| `temporal_draws` | `RuntimeHooks` | the draws of the game that MetalFX temporal upscaling replaces: the upscale draw (fragment key, colour slot), the source draw (fragment key, depth slot, velocity slot, stage and slot of the view uniform buffer) and the pass-through draws (fragment key, slot). `nxb build` writes `settings.temporal_upscale` of `game.json` (R72) only when `temporal_view` and `temporal_draws` are both answered |
| `runtime` | `RuntimeHooks` | whether the nxkit runtime runs one service of the game |

A role names what a shader does. The core decides what to do with it. A title module does not change the core's code path for a shader.

### Console variables through a pak (family `unreal`)

A Shipping build of an Unreal game ignores `-ExecCmds`. The family sets console variables with an extra pak instead. `nxb build` reads the paks in `*/Content/Paks` of the RomFS (pak versions 3 and 4, zlib or uncompressed, never an encrypted index). It takes `Config/DefaultDeviceProfiles.ini` of the project from the pak with the highest priority and sets `+CVars=<name>=<value>` in the section `[<Platform> DeviceProfile]`. A line of that name is replaced, otherwise a line is added. The platform is the one in the pak file names `<Project>-<Platform>[_<n>][_P].pak`. The build fails when the section is missing.

The copy goes into a new pak `<Project>-<Platform>_<n+1>_P.pak` of version 4 with one uncompressed entry. The engine reads a file from the pak that was mounted last. A `_P` pak ranks over a plain one, and a higher number `_<n>_P` over a lower one. A `_P` pak without a number counts as 0. `n` is the highest patch number, or 0 when there is no patch pak.

The build writes the pak into the overlay folder beside the extract and adds it to the app's RomFS. It logs `console vars: <names> in <pak path>`. A title without console variables builds as before.

## Which engine runs a game

The host is split into services (`Service`): `fs`, `input`, `audio`, `video`, `system`, `offline`, `pacing`, `kernel` and `gpu`. A family or title answers `runtime` for a service with `Runtime::Nxkit`, and the nxkit runtime runs that service for the game. `Resolved::nxkit_services` lists them.

The plan for the nxkit runtime and for applying needs is [plans/runtime-hooks.md](plans/runtime-hooks.md).

## Evidence

Every answer is a `Fact`, a value with its `Evidence`.

- `Tested` names a test in this workspace.
- `Recorded` names a record outside the workspace, for example a game README at a commit, and the day it was read.
- `Unverified` says why the fact is believed and what would check it.

The core can refuse an unverified answer, for example in a release build.

## Engine detection

A game with a title module has its engine family from `Title::engine`. A game without one gets it by detection
(`Catalog::detect_engine`), and the build says so in the log and in `game.json` (`generic`, `engine`).

1. A family names its markers (`Family::markers`): a byte string in the read-only data of the modules (`Rodata`), or a
   file in the RomFS (`RomfsPath`, a folder when the pattern ends with `/`). The id of a marker is a library name.
2. The core scans once for the markers of all families, in the modules and in the RomFS file names
   (`nxb-build`, `engine_scan`). The hits are the `libraries` of `TitleFacts`. The rewriter's finder of the engine
   console variables also names the library `UnrealEngine`.
3. `detect` of each family decides from the libraries. A family that only lists markers matches when one of them hit.
   Exactly one match is the answer. When several match, the family that `refines` another settles it. Else there is no
   answer and the build lists every candidate with its markers. Nothing is guessed. Nobody looked, or no marker hit:
   no engine family, and only the graphics family applies.
4. The generic path gives the manifest the frame limit and the render size arguments of the family. The host prints one
   line at its start. The family's console variables (the pak) are not made for such a game yet.

| Family | Markers | Evidence | Status |
|---|---|---|---|
| `unreal` | the rewriter found an engine console variable in `main` (`UE_CVAR:`), the folder `Content/Paks/` in the RomFS | the finder runs on recorded games, the folder is the engine's layout | in use |
| `unity` | `Data/Managed/Metadata/global-metadata.dat`, `Data/globalgamemanagers`, `Data/data.unity3d` in the RomFS, `il2cpp_init` in a module | the engine's public file layout, not read from a game here | unverified |

A family that gets rules or hooks gets its markers in the same commit.

The other families have no marker. No trace of their engine is known that does not need a game to find it, and a guessed
marker gives a wrong answer. Their `detect` stays `Unknown`. A family gets markers when a game of it shows a reliable
trace. The test `engine_markers_are_unique_and_each_family_finds_itself` of `nxtitles` keeps the markers apart. The
`#[ignore]` test `the_detected_engine_is_the_declared_one` of `nxb-build` compares the detection with `Title::engine`
over game dumps (`NXB_PARITY_ENGINE`, one folder per title with `exefs/` and `romfs/`) and prints a confusion list.

## Rules for a module

Each fact has one place. Use this table to find it.

| The fact is true for | It goes into | Example |
|---|---|---|
| every game | the core (`crates/`) | the NVN layer of the host, the shader split, the SDK-wide rewrite rules |
| every game on one graphics API | the graphics family (`titles/graphics/<api>`) | Vulkan: the host supplies the SDK's loader and driver |
| every game on one engine | the engine family (`titles/engines/<id>`) | Unreal Engine 4 and 5: the frame limit `t.MaxFPS` |
| one version of an engine | the engine family, if the fact matches only that version, else the title module | Unity 2018: the `ThreadedStreamBuffer` rule, matched by fingerprint |
| one game | the title module (`titles/games/<title id>`) | the build IDs, the memory layout, the byte cuts of one build |

1. A title module does not work around a core bug. A fix that helps more than one game goes into the core.
2. A fact about engine code goes into the engine family, also when only one game showed it. Its evidence names that
   game. Do not keep it in the title module to move it later.
3. A fact about a graphics API goes into the graphics family. A fact about the content of one game stays in the title
   module.
4. Every title module names its engine family (`Title::engine`) and its graphics API (`Title::graphics`). An engine
   that only one game uses also gets a family. The tests of `nxtitles` fail when a title names a family that does not
   exist, or when no title names a family.
5. The core knows no game, no engine and no API of a game. Core code does not test a title id, a family id or an
   engine name. It asks the hooks of `nxtitle`.
6. A title names its families by id. It does not depend on a family crate. A family does not depend on another family
   or on a title, and it does not name a title. Without a family, the title works with the next layer.
7. Only `nxtitles` depends on family and title crates. The core depends on `nxtitle` and `nxtitles`. `cargo run -p
   xtask -- layers` checks rules 6 and 7.
8. The committed part of a game's profile (design 2.11) is `profile.toml` in the title module's folder. Keys that a
   capability check decides are not in it. A family or title states them as needs.

## Adding a game

1. Make `titles/games/<title id>/` with a crate `nxgame-<short name>` of layer `T`.
2. Implement `Title`, `BuildHooks` and `RuntimeHooks`. `Title::engine` names the engine family, `Title::graphics` the
   API. Leave every hook without an answer at its default.
3. Add `profile.toml` with `title_id` and `[declared] title`. A test parses it with `nxb-profile` and expects no problems.
4. If the engine has no family yet, make one (below).
5. Run `python3 tools/gen_title_registry.py .`: it writes `[workspace.dependencies]`, the features of
   `titles/nxtitles/Cargo.toml` and the lists of `catalog()` from the folders.

A new family follows the same steps in `titles/engines/<id>/` (crate `nxengine-<id>`, feature `engine-<id>`) or
`titles/graphics/<id>/` (crate `nxgraphics-<id>`, feature `graphics-<id>`) with `Family` in place of `Title` and
`Family::layer` set. The static is `ENGINE` or `GRAPHICS`. A test of `nxtitles` checks that every title finds both of
its families and that every family is named by a title.

## The Legend of Zelda: Ocarina of Time (`0400486024DEC000`)

The Nintendo Switch 2 game on NVN2, family `agl`. The records are the game notes at a commit, with the day they were read.

- The family marks the `agl_ndsr_vr` networks as the game's upscaler. The archive path that identifies them is unverified.
- The title states `draw_barriers`, `mem_map_limit`, `render_cond`, `tex_flush_upload` and `tex_dirty_upload`. The last three come from agl's depth prepass and the terrain maps the CPU writes. They are title facts, because Breath of the Wild sets none of them.
- The game builds with `nxb build --nup <game>.nup`. The title module gives update v131072, the modules `main`, `subsdk0` and `sdk` with their build IDs, the shader groups, the marker `Pack/ZsDic.pack.zs` and the run settings recorded at `37f076a5`. The run settings are the memory split, `sys_resource`, `mem_map_limit`, `fps_target 120` with `fps_max 120`, `pad_layout full` and the graphics keys. For `fps_target`, the record says 60, but 120 is not checked in the game yet.
- The NVN function list is not a hand list. The title module gives an empty list. The build takes the `nvn…` names in the read-only data of the modules: 774 names in v131072 (738 in `main`, 36 more in `sdk`). That is what the game can fetch through the NVN loader, more than one route calls.
- `fps_target 60` and `120` need the frame ratio stub (`MATCH_FRAME_STEP`). It is the one rule of `rules/0400486024DEC000.toml`, a `frame-step` rule for main build `e0f3ccfaf50065ae4ea01eb5aeb13b1a`. At `main+0x6e6cc4` (`fdiv s8,s1,s0`) the ratio becomes the host's step × base rate / 60. The step is in vsyncs of 1/60 s. At 60 it is the whole step (state page `+0x20`). At 120 it is the variable step (`+0x50`, 1/30 s units) times 2, so half the ratio per 120 Hz tick. The game steps its time by that ratio. Thus the pacing is the display's. The dynamic resolution stub of the same file is a separate change.
- The run settings for 60 fps (2026-11-23, M2 Max, the Kokiri Forest autosave, built-in ProMotion screen): `fps_target 60` (120 stays the launcher's choice through `fps_max`), `overlap`, `rec_lock false` and `pace_profile steady`. Together they gave 50 to 52 fps in six runs; any one of them alone stayed at 8 to 17 fps. Without them the CPU and the GPU wait for each other several times a frame, the GPU is busy about a third of the time and macOS keeps it at its lowest clock. With `work`, a late frame raised the hold to 2 or 3 vsyncs and the GPU clock fell further. The GPU takes about 16.8 ms a frame there, so some frames still take two vsyncs. Not checked: whether the foliage blotches that once came with the overlap are back (the picture was overexposed in every run, with and without it), and full screen (one run stayed at 11 fps).
- The `.nup` of v131072 is one standalone update: 186,587 RomFS files, 26.2 GB. 146,000 files (23.2 GB) are stored raw and 40,600 (3.0 GB) as one zstd frame each. The build writes the RomFS as a folder in one pass with the shader split (no `RomFS.img`), and links the files into the app.

### Fixed resolution

`nxb build ... --resource-patch resolution-scale=<scale>` pins the dynamic-resolution scale, with `0 < scale <= 1`.
Without the option, or with `resolution-scale=stock`, the game keeps its dynamic resolution. The finding is
ComicBit's (v0lumehi/nxkit#22): in v131072 the two resolution palettes share ten `f32` words in the ModelSceneExtensionParam
of `Pack/Bootup.Nin_Ounce_NVN2.pack.zs`, a SARC compressed with the dictionary `pack.zsdic` of `Pack/ZsDic.pack.zs`.
The build decodes the archive, checks the SHA-256 of the archive, the dictionary and the member, writes the scale into
the ten words and stores the SARC again as a zstd frame of raw blocks. No compressor and no native library are needed.
The patched package is `<work>/resources/<package>`, a clone of `extract/<package>` in which only that archive is new.
An archive of another version is left as it is, with a warning, and the build goes on. A fixed scale does not promise
a frame rate. A higher scale is more GPU work.

Open questions:

- Should the host keep `agl_ndsr_vr`, render at the output size, or use MetalFX?
- Which facts of the family hold for Tears of the Kingdom and other agl games?

## The Legend of Zelda: Breath of the Wild (`01007EF00011E000`)

The Nintendo Switch game on NVN, update 1.9.0 (v1114112), family `agl`. The records are the game notes at a commit, with the day they were read.

- The title states `fps_target 60`, `present_snap`, `pso_mtl4` and `screen_aspect`.
- `fps_target 60` needs the frame-step stubs. They are the 168 steps of `rules/01007EF00011E000.toml` in the title module, which `rewrite_rules()` gives to the build.  These steps have the variable step, the render pool, the heap growth and the output size. The screen shape, layout and FOV stubs (about 2900 more steps) are not part of the file. They use the same sites and state page offsets as the variable step. Thus they need their own state page layout and host support first. Until then, `screen_aspect` does nothing.
- The settings are recorded values. R19 sets the target to 120 fps through the same frame step stubs, so `fps_target 60` is a record and not a cap. R10 and R17 replace `present_snap`, `enc_thread`, `core_preempt`, `draw_barriers_game` and the `tex_*` keys. The host reader reads no `settings` object yet.
- `Settings` names no engine. An engine with a texture pool or a frame rate variable uses `texture_pool_mb` and `fps_cvar`.
- The game is the pilot for the nxkit runtime.

## Super Mario Bros. Wonder (`010015100B514000`)

The Nintendo Switch game on NVN, update 1.2.1 (v327680), SDK 16.2.12, family `agl`. The records are the game notes in
the engine tree at `b91cb749`, read 2026-11-12.

- The game builds with `nxb build "<folder with the base and the v327680 NSP>"`. The title module gives the modules
  `main` and `sdk` with their build IDs, the shader groups of `game.tsv` (`SecredUber`, `agl`, `effect`, `layout`), the
  marker `System/HeapSize/Product.Nin_NX_NVN.HeapDef.byml.zs` (in v0 and in v327680) and the run settings of
  `game.tsv`: the 5 GB guest layout (2.875 GB alias region, 1.625 GB heap region) and the defaults of `game.py` for
  every other key. The display version is 1.2.1.
- The NVN function list is not a hand list: the build takes the `nvn…` names in the read-only data of the modules. The
  README counts 534 names the loader fetches and 236 the game calls.
- `rules/010015100B514000.toml` has the build row and the engine's two SDK-wide rules for `sdk`: the node area of the
  AddressRegionManager 62 MB in place of 1 GB, and regions of 4 MB and more aligned to 2 MB. Both match sdk build
  `e4bd58ca…` once. The game maps 1.75 GB of virtual address memory into the alias region, and its layout was measured
  with them. The game has no code patch of its own.
- The title states no needs. `game.tsv` sets no `render_cond`, `tex_*`, `pad_layout` or pacing key. `main` imports
  `ScalableFontEngineHelper::Decode`, so the shared font is the BFTTF form (`shared_font_decode`).
- The engine played the game with a shader table of v0 and v327680 together (`shader_versions v0 v327680`). The build
  splits only the shaders of v327680.

## Hades (`0100535012974000`)

The Nintendo Switch game on **Vulkan**, update 1.0.38233 (v589824), SDK 9.3.0, Supergiant Games' engine on The Forge
renderer. The records are the game notes in the engine tree at `034e91a8`, read 2026-11-21.

- `BuildHooks::graphics_api` is `Vulkan`, so `game.json` says `"graphics": "vulkan"` and the host installs the Vulkan
  layer (`nxkit-vk`, `nxkit-rt::vk`, `nxkit-vk-gpu`, R64) and no NVN layer. `main` holds 126 `vk*` names (imports and
  names it fetches through `vkGet*ProcAddr`). Each one has an export.
- The modules are `main` and `sdk`. `subsdk0` (the Vulkan loader) and `subsdk1` (NVIDIA's driver) are replaced by the
  host's layer, so the build row of the rules names neither.
- The shaders are the 74 SPIR-V files of `Content/EffectsForgeSwitch` (`romfs_marker` is `Default.vert`). The build
  translates them with `nxb-spirv` into `shaders/vk_shaders.metallib` and its table `vk_shaders.tsv` (D21). The table
  equals the engine's row for row. The game has no NVN shaders, no shader split and no pipeline archive.
- The rules are the build row and the engine's SDK-wide rules for `sdk`: both match this sdk. The node area is at
  `0xd2240`, implementation 2 of 8. The 2 MB alignment is at `0xd2434`, implementation 1 of 4.
- `main` imports 39 `EOS_*` functions (Epic Online Services, from an NRO through `nn::ro`). The host refuses an NRR that
  registers only online SDKs. Thus the game never loads the module. The names are `optional_imports` and stay 0.
- The memory partition is the engine's (Link's Awakening's, not measured for this game), `pad_layout full`.
- Checked on 2026-11-21 (MacBook Pro, a fresh `nxb build` of the folder with both packages): the game shows the title
  screen and plays a room of Tartarus at about 60 presents a second, 0 skipped draws. The layer logs one gap: texel
  buffer views (`R32_UINT`), which nothing on screen needed in that run.

## The games of the engine tree

The engine tree at `034e91a8` (read 2026-11-12) has 163 game folders. Each game it plays has a title module now: 149 new
modules, and Animal Crossing: New Horizons and Mario Tennis Fever got their build data. The modules were made with a
script from each folder's `game.tsv`, `patches.py` and `shader_facts_manual.tsv`, the games table of
`dm/games/README.md`, `dm/tools/sdk_patches.py` and `dm/tools/engine_patches.py`, and then read through. Every module
has the same parts:

- `standalone`: the version of `package`, the modules of its `build` row in the load order of `modules` with their build
  IDs, `shader_groups`, `shader_note`, `romfs_marker` and the run settings. A setting is the value of `game.tsv`, or the
  default of `game.py` for a key it does not set. The display version is the one of the package's build row (Nintendo
  Switch Sports Resort's row has `?`, so it has none). The upscaler keys are the `agl_ndsr.pck` compute shaders of the
  folder's `shader_map.tsv`, as `game.py` finds them. An Unreal Engine 4 or 5 game gets `fps_cvar` (`ue_fps_cvar`, else
  `t.MaxFPS`). Keys without a field (`depth_mode`, `zip_store`, `ipa`, `app_category`, `pso_gpu`, `render_base`) are not
  taken.
- The NVN function list is empty: the build reads the names from the modules.
- `rules/<title id>.toml`: the package's build row, the byte cuts (`CUTS`) and capabilities of `patches.py` for the
  builds of that row, the byte cuts of `sdk_patches.py` for an sdk build of the row, the function-matched rules of
  `patches.py` (`MATCH_CUTS`), the engine's SDK-wide rules for `sdk` (node area 62 MB, regions aligned to 2 MB, one
  `[[rule]]` per reviewed implementation). This gives 12 rules in all, and 15 in the modules of the second import below. The engine-wide
  rule of Unity 2018 from `engine_patches.py` is in the engine family `unity`, not in the title files. Each rule names its source in `evidence`. A frame step stub (`FRAME_STEP`, `MATCH_FRAME_STEP`)
  and code stubs (`STUBS`) are not byte cuts and are left out. A game whose `fps_target` needs the left-out stub gets
  `fps_target 0`, as the engine gives a build without its stub.
- All nine services run on nxkit (unverified: no game ran on nxkit yet).
- `shared_font_decode`: `false` on SDK 10 and older (the engine's rule), the value of `shared_font`, or `false` where the
  README says that `main` does not import `ScalableFontEngineHelper::Decode`. On SDK 11 and later without a record the
  module gives no answer, and the core default (the TTF itself) holds.
- Every module names its engine family and its graphics API. The engine is the one of the games table's engine column:
  one family per engine (77 in all), also for an engine that only one game uses, and `agl` when the column names sead
  and agl. Unity, Unreal Engine 4 and 5, PlatinumGames' engine and Unreal Engine 3 are each one family, whatever the
  version. Unreal Engine 3 is a family of its own (`unreal3`), not `unreal`. Where the column names no engine, the family
  is named after the developer (`basso`, `typemoon`, `nicalis`). The graphics family is `nvn` (NVN and NVN2), `vulkan`
  or `opengl`, after the column `Graphics`.
- The family `unreal` holds Unreal Engine 4 and 5. Its fact is the frame limit: a `LimitRule` row on the console
  variable `t.MaxFPS`, found by the rewriter by its registered name in any build (`UE_CVAR:t.MaxFPS` in the fixups of
  `main`). The object holds at `+0x10` the pointer to the value, a float for the game thread and one for the render
  thread (`dm/host/hle_fpslock.c` and `dm/host/hle_uecvar.c` at `034e91a8`, read 2026-11-22). Every Unreal Engine 4 and
  5 title names the family. Only OCTOPATH TRAVELER was run at the written limit. For the others the limit is the
  family's fact, unchecked on that game.
- The family `unreal` also names the program arguments of the render size, `-ResX={w} -ResY={h}` (R71). The engine's
  Switch platform code reads them in `nninitStartup`, 1280x720 when absent.
- Unreal Engine 4 on NVN calls `nvnDeviceSetDepthMode(NEAR_IS_ZERO)` (seen in OCTOPATH TRAVELER, 4.18). The depth
  mode is a capability, not a profile key: the host follows the call, and the build learns it from the recorded pipeline
  list. It is a candidate fact for the family `unreal` once a run of a second Unreal title shows it.
- OCTOPATH TRAVELER sets `spatial_audio` in its run settings. CRIWARE ADX2 mixes its buses into a six-channel AudioOut
  in the SDK's order (L R C LFE Ls Rs, `criNcVoice_InterleavePcmFloat32toInt16`). Thus the game gains from 5.1 played as
  spatial audio. The setting is the default of the launch option `NXKIT_SPATIAL_AUDIO`. The player's choice wins.
- `shader_facts.tsv` is the folder's `shader_facts_manual.tsv`, where it has one.

### The second import (`131bd013`)

The engine tree at `131bd013` (read 2026-11-22) has 215 game folders, 52 more than at `034e91a8`. 56 folders got a module
now: the 51 new ones that hold a `game.tsv`, and the five that were left out before for their graphics API (Digimon
Story Cyber Sleuth, Well Dweller, Balatro, Super Mario Galaxy and Galaxy 2 are now modules of the graphics families
`opengl` and `vulkan`). They are made as above, with three differences:

- The SDK-wide rules are those of `sdk_patches.py` at `131bd013`: 15 rules, with the older VammManager's 64 MB node area
  and 32 MB alignment (SDK 3.5.1 and 4.4.0) and the node area of SDK 21.4.
- An OpenGL game builds no app: `nxb build` stops with a note that nxkit has no OpenGL layer. Super Mario Galaxy, Galaxy
  2, Minecraft and Re:ZERO are on Vulkan, and the engine added parts to its Vulkan layer for them that nxkit's does not
  have. Each module names them.
- The pipeline seeds of the 19 new titles that have recorded rows in `pso_manifest.tsv` were imported by `nxb-seeds
  import --ref 131bd013 --only <title id>`. The other titles have none in the engine tree: its `pso_manifest.tsv` is
  empty until someone records the game on the Mac.

The tests of each module parse its profile and its rules with `nxb-rewrite` and count the rules. The rules of Pokémon
Scarlet were run through the rewriter on the extracted v0: both SDK-wide rules match its sdk once. `nxb build
--dry-run` was run on Pokémon Scarlet (v0, the module's version). The update v786432 in the folder is not used.

Notes: "frame step stub" means that `rules` leave a frame step stub out, "`fps_target` 0" that the module sets 0 for it,
"code stubs" that `STUBS` are left out, "`snowdrop_pools`" that the build places Snowdrop's pools with the defaults
(the game's are 2048, 512 and 512 MB), and "NROs (unchecked)" that the game loads NRO modules through `nn::ro`.

| Title ID | Game | Feature | Engine | Graphics | Version | Rules | Notes |
|---|---|---|---|---|---|---|---|
| `0100000000010000` | Super Mario Odyssey | `smo` | `agl` | `nvn` | 1.4.1 (v393216) | 12 | - |
| `0100000011D90000` | Pokémon Brilliant Diamond | `pkbd` | `unity` | `nvn` | 1.3.0 (v393216) | 12 | - |
| `010005501E68C000` | Ace Attorney Investigations Collection | `aaic` | `unity` | `nvn` | 1.0.0.2 (v131072) | 12 | - |
| `010012101468C000` | Metroid Prime Remastered | `mpr` | `retro` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010013C00E930000` | Hollow Knight: Silksong | `silksong` | `unity` | `nvn` | 1.0.30000 (v655360) | 12 | - |
| `010014C011146000` | Subnautica: Below Zero | `snbz` | `unity` | `nvn` | 1.23.55063 (v655360) | 12 | - |
| `010014E00DB56000` | Digimon Story Cyber Sleuth: Complete Edition | `digimoncs` | `mvgl` | `opengl` | 1.0.3 (v196608) | 15 | no OpenGL layer |
| `010015100B514000` | Super Mario Bros. Wonder | `smbw` | `agl` | `nvn` | 1.2.1 (v327680) | 2 | own section above |
| `0100152000022000` | Mario Kart 8 Deluxe | `mk8d` | `agl` | `nvn` | 4.0.1 (v1507328) | 12 | - |
| `010016100A186000` | SteamWorld Heist: Ultimate Edition | `swheist` | `steamworld` | `nvn` | 2.2.1 (v65536) | 12 | - |
| `0100199023B2E000` | Atelier Ryza: Ever Darkness & the Secret Hideout DX | `ryza` | `ktgl` | `nvn` | 1.2.0 (v65536) | 12 | - |
| `010019A01E2F2000` | Metroid Prime 4: Beyond | `mp4` | `retro` | `nvn` | 1.1.0 (v131072) | 12 | - |
| `01001B300B9BE000` | Diablo III: Eternal Collection | `diablo3` | `blizzard` | `nvn` | 2.7.7.92380 (v1441792) | 12 | - |
| `01001C8016B4E000` | FIFA 23 Legacy Edition | `fifa23` | `ea` | `nvn` | 1.0.3 (v196608) | 15 | - |
| `01001F5010DFA000` | Pokémon Legends: Arceus | `pkarceus` | `gamefreak` | `nvn` | 1.1.1 (v262144) | 13 | - |
| `010020400E1C2000` | Raging Loop | `ragingloop` | `unity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010020D01AD24000` | ANIMAL WELL | `animalwell` | `basso` | `nvn` | 1.0.10 (v655360) | 12 | - |
| `0100217023F6C000` | Well Dweller | `welldweller` | `gamemaker` | `opengl` | 1.0.0 (v0) | 15 | no OpenGL layer |
| `010026300BA4A000` | Slay the Spire | `slaythespire` | `monogame` | `opengl` | 1.0 (v0) | 15 | no OpenGL layer |
| `010027400CDC6000` | Divinity: Original Sin 2 - Definitive Edition | `dos2` | `larian` | `nvn` | 1.0.12 (v786432) | 15 | - |
| `0100276009872000` | ŌKAMI HD | `okami` | `clover` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010028600EBDA000` | Super Mario 3D World + Bowser's Fury | `sm3dwbf` | `agl` | `nvn` | 1.2.2 (v262144) | 12 | - |
| `010029B0118E8000` | Need for Speed: Hot Pursuit Remastered | `nfshp` | `criterion` | `nvn` | 1.0.3 (v196608) | 13 | - |
| `010029B018432000` | Ghost Trick: Phantom Detective | `ghosttrick` | `re-engine` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010029D00E740000` | DOOM 3 | `doom3` | `idtech4` | `opengl` | 1.0.5 (v327680) | 15 | no OpenGL layer |
| `01002B00111A2000` | Hyrule Warriors: Age of Calamity | `hwaoc` | `ktgl` | `nvn` | 1.3.0 (v327680) | 12 | frame step stub, `fps_target` 0 |
| `01002B30028F6000` | Celeste | `celeste` | `monogame` | `nvn` | 1.4.1.1 (v589824) | 12 | - |
| `01002CD00A51C000` | Baba Is You | `baba` | `fusion` | `nvn` | 1.0.12 (v786432) | 12 | - |
| `01002D4007AE0000` | Mega Man Legacy Collection | `mmlc` | `digital-eclipse` | `opengl` | 1.0.3 (v196608) | 15 | no OpenGL layer |
| `01002DA013484000` | The Legend of Zelda: Skyward Sword HD | `sshd` | `nintendo-wii` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `01002E7016C46000` | Cult of the Lamb | `cotl` | `unity` | `nvn` | 1.5.28 (v1966080) | 12 | - |
| `0100300012F2A000` | Advance Wars 1+2: Re-Boot Camp | `aw12` | `unity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `0100317013770000` | Mario + Rabbids Sparks of Hope | `mrsoh` | `snowdrop` | `nvn` | 1.6.2225577 (v393216) | 13 | - |
| `010031F019294000` | Super Monkey Ball Banana Rumble | `smbbr` | `unity` | `nvn` | 2.20 (v458752) | 12 | - |
| `010032E00E6E2000` | Return of the Obra Dinn | `obradinn` | `unity` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100346017304000` | Taiko Risshiden V DX | `trvdx` | `ktgl` | `nvn` | 1.1.1 (v131072) | 15 | - |
| `010035F022078000` | DRAGON BALL: Sparking! ZERO | `dbspz` | `unreal` | `nvn` | 2.0.76 (v327680) | 12 | - |
| `010036E00FB20000` | The Great Ace Attorney Chronicles | `tgaac` | `mt-framework` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010038600B27E000` | Bastion | `bastion` | `monogame` | `nvn` | 1.0.0 (v0) | 12 | - |
| `010038B015560000` | FINAL FANTASY TACTICS - The Ivalice Chronicles | `fftic` | `square-enix` | `nvn` | 1.4.0 (v393216) | 14 | - |
| `01003E601E324000` | DRAGON QUEST III HD-2D Remake | `dq3hd` | `unreal` | `nvn` | 1.1.2 (v262144) | 12 | - |
| `01003FC01670C000` | 13 Sentinels: Aegis Rim | `sentinels` | `vanillaware` | `nvn` | 1.10 (v65536) | 12 | - |
| `010040801A4C8000` | Blasphemous 2 | `blasphemous2` | `unity` | `nvn` | 1.0.8 (v524288) | 12 | - |
| `010042000A986000` | DRAGON QUEST BUILDERS 2 | `dqb2` | `ktgl` | `nvn` | 1.7.3d (v393216) | 12 | - |
| `01004200189F4000` | Factorio | `factorio` | `wube` | `opengl` | 2.0.76 (v2031616) | 15 | no OpenGL layer |
| `0100429011144000` | Subnautica | `subnautica` | `unity` | `nvn` | 1.23.83358 (v458752) | 12 | - |
| `010042D00D900000` | LEGO Star Wars: The Skywalker Saga | `legosw` | `traveller` | `nvn` | 1.10.0 (v589824) | 15 | - |
| `010044500C182000` | Sid Meier's Civilization VI | `civ6` | `firaxis` | `nvn` | 1.2.19 (v1638400) | 15 | - |
| `0100453019AA8000` | Xenoblade Chronicles X: Definitive Edition | `xcx` | `monolith` | `nvn` | 1.0.1 (v65536) | 22 | code stubs |
| `010045C014650000` | 13 Sentinels: Aegis Rim, Japanese release | `sentinelsjp` | `vanillaware` | `nvn` | 1.00 (v0) | 12 | - |
| `01004680124E6000` | Bloodstained: Curse of the Moon 2 | `bscotm2` | `inti` | `nvn` | 1.3.2 (v327680) | 15 | - |
| `01004A4010FEA000` | Bayonetta 3 | `bayo3` | `platinum` | `nvn` | 1.2.0 (v131072) | 12 | - |
| `01004AB00A260000` | DARK SOULS: REMASTERED | `dsr` | `fromsoftware` | `nvn` | 1.0.3 (v196608) | 14 | - |
| `01004B800AF5A000` | Bloodstained: Curse of the Moon | `bscotm` | `inti` | `nvn` | 1.1.1 (v131072) | 15 | - |
| `01004D300C5AE000` | Kirby and the Forgotten Land | `kirbyfl` | `hal` | `nvn` | 1.1.0 (v65536) | 12 | - |
| `010050F00BC1A000` | Resident Evil | `re1` | `mt-framework` | `nvn` | 1.0.0 (v0) | 15 | - |
| `010051701FB46000` | Monster Train 2 | `mt2` | `unity` | `nvn` | 1.1.1 (v131072) | 12 | - |
| `010051C0134F8000` | DRAGON BALL Z: KAKAROT + A NEW POWER AWAKENS SET | `dbzk` | `unreal` | `nvn` | 1.70 (v1048576) | 15 | - |
| `01005270232F2000` | Octopath Traveler 0 | `octo0` | `unreal` | `nvn` | 1.0.5 (v327680) | 12 | - |
| `0100535012974000` | Hades | `hades` | `supergiant` | `vulkan` | 1.0.38233 (v589824) | 12 | own section above |
| `010054B01AD92000` | Unicorn Overlord, Asian release | `unicornasia` | `vanillaware` | `nvn` | 1.05 (v262144) | 12 | - |
| `010055D009F78000` | Fire Emblem: Three Houses | `fe3h` | `ktgl` | `nvn` | 1.2.0 (v327680) | 19 | - |
| `010056B015FE8000` | NieR:Automata The End of YoRHa Edition | `nier` | `platinum` | `nvn` | 1.0.3 (v131072) | 21 | - |
| `010057A01E4D4000` | TSUKIHIME A piece of blue glass moon | `tsukihime` | `typemoon` | `nvn` | 1.0.2 (v131072) | 15 | - |
| `010057D006492000` | OCTOPATH TRAVELER | `octo` | `unreal` | `nvn` | 1.0.5 (v327680) | 12 | - |
| `01005CA01580E000` | Persona 5 Royal | `p5r` | `gfd` | `nvn` | 1.0.2 (v65536) | 12 | - |
| `01005FF002E2A000` | Rayman Legends: Definitive Edition | `raymanl` | `ubiart` | `nvn` | 1.2 (v131072) | 15 | - |
| `01006000040C2000` | Yoshi's Crafted World | `yoshicw` | `unreal` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100606026F12000` | FINAL FANTASY RESONANCE | `ffres` | `unreal` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `010061D00DB74000` | Ori and the Blind Forest: Definitive Edition | `oribf` | `unity` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `0100620012D6E000` | Big Brain Academy: Brain vs. Brain | `bba` | `bezel` | `nvn` | 1.2.0 (v131072) | 12 | - |
| `010062B01525C000` | Persona 4 Golden | `p4g` | `atlus` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `0100633007D48000` | Hollow Knight | `hollowknight` | `unity` | `nvn` | 1.5.12620 (v589824) | 12 | - |
| `010063301BD50000` | SUPER ROBOT WARS Y | `srwy` | `unity` | `nvn` | 1.4.1 (v720896) | 12 | - |
| `0100646009FBE000` | Dead Cells | `deadcells` | `heaps` | `nvn` | 1.25.0 (v3014656) | 12 | - |
| `010064800F66A000` | Borderlands: Game of the Year Edition | `borderlands` | `unreal3` | `nvn` | 1.0.2 (v131072) | 15 | - |
| `010067300059A000` | Mario + Rabbids Kingdom Battle | `mrkb` | `snowdrop` | `nvn` | 1.9.589692 (v589824) | 15 | `snowdrop_pools` |
| `010069401ADB8000` | Unicorn Overlord | `unicorn` | `vanillaware` | `nvn` | 1.05 (v262144) | 12 | - |
| `0100698009C6E000` | Blasphemous | `blasphemous` | `unity` | `nvn` | 1.0.8 (v524288) | 12 | - |
| `010069C01AB82000` | SHIN MEGAMI TENSEI V: Vengeance | `smt5v` | `unreal` | `nvn` | 1.0.3 (v196608) | 12 | - |
| `01006A800016E000` | Super Smash Bros. Ultimate | `ssbu` | `sora` | `nvn` | 13.0.5 (v2031616) | 24 | NROs (unchecked) |
| `01006BB00C6F0000` | The Legend of Zelda: Link's Awakening | `zla` | `grezzo` | `nvn` | 1.1.0 (v131072) | 13 | - |
| `01006C300E9F0000` | DRAGON QUEST XI S: Echoes of an Elusive Age – Definitive Edition | `dq11s` | `unreal` | `nvn` | 1.0.4 (v262144) | 12 | - |
| `01006D0017F7A000` | Mario & Luigi: Brothership | `mlbs` | `unreal` | `nvn` | 1.0.0 (v0) | 12 | - |
| `01006F8002326000` | Animal Crossing: New Horizons | `acnh` | `agl` | `nvn` | 3.0.3 (v2228224) | 12 | own section above |
| `01006FE013472000` | Mario Party Superstars | `mps` | `bezel` | `nvn` | 1.1.1 (v131072) | 12 | NROs (unchecked) |
| `010070E022546000` | Fate/hollow ataraxia REMASTERED | `fateha` | `fuzz` | `opengl` | 1.0.2 b723 (v131072) | 15 | no OpenGL layer |
| `0100713016CCC000` | AI: THE SOMNIUM FILES - nirvanA Initiative | `aisni` | `unity` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `0100726014352000` | Diablo II: Resurrected | `d2r` | `blizzard-d2r` | `nvn` | 1.0.34.0 (v2228224) | 15 | - |
| `01007300020FA000` | ASTRAL CHAIN | `astral` | `platinum` | `nvn` | 1.0.1 (v65536) | 12 | frame step stub, `fps_target` 0 |
| `010074F013262000` | Xenoblade Chronicles 3 | `xc3` | `monolith` | `nvn` | 2.2.1 (v720896) | 18 | - |
| `010076F0049A2000` | Bayonetta | `bayo` | `platinum` | `nvn` | 1.1 (v65536) | 12 | - |
| `0100777022394000` | Cozy Land | `cozyland` | `unity` | `nvn` | 1.0.0 (v0) | 15 | - |
| `01007820196A6000` | Red Dead Redemption | `rdr` | `rage` | `nvn` | 1.0.7 (v458752) | 12 | - |
| `01007960049A0000` | Bayonetta 2 | `bayo2` | `platinum` | `nvn` | 1.2 (v131072) | 12 | - |
| `01007A2027548000` | Trails in the Sky 2nd Chapter | `trailssky2` | `falcom` | `nvn` | 1.0.4 (v393216) | 15 | - |
| `01007A3009184000` | Princess Peach: Showtime! | `peach` | `unreal` | `nvn` | 1.0.0 (v0) | 12 | - |
| `01007BB017812000` | Portal | `portal` | `source` | `nvn` | 1.0.3 (v196608) | 12 | NROs (unchecked) |
| `01007E3006DDA000` | Kirby Star Allies | `kirbysa` | `hal` | `nvn` | 4.0.0a (v393216) | 12 | - |
| `01007EF00011E000` | The Legend of Zelda: Breath of the Wild | `botw` | `agl` | `nvn` | 1.9.0 (v1114112) | 8 | own section above |
| `0100801011C3E000` | Persona 5 Strikers | `p5s` | `ktgl` | `nvn` | 1.0.0 (v0) | 16 | - |
| `0100830004FB6000` | L.A. Noire | `lanoire` | `team-bondi` | `nvn` | 1.2 (v131072) | 15 | - |
| `010086A022444000` | BALL x PIT | `ballxpit` | `unity` | `nvn` | 1.293 (v393216) | 12 | - |
| `010087701B092000` | Persona 5 Tactica | `p5t` | `unity` | `nvn` | 1.1.0 (v262144) | 15 | - |
| `010089B00D09C000` | AI: THE SOMNIUM FILES | `ais` | `unity` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `010089C01E9CA000` | SteamWorld Heist II | `swheist2` | `steamworld` | `nvn` | 1.5.1 (v262144) | 12 | - |
| `01008B900DC0A000` | FINAL FANTASY VIII Remastered | `ff8` | `baseengine` | `opengl` | 1.0.1_5 (v196608) | 15 | no OpenGL layer |
| `01008BC024FDE000` | STEINS;GATE RE:BOOT | `steinsgate` | `m2` | `nvn` | 1.0.3 (v131072) | 12 | - |
| `01008C0016544000` | Sea of Stars | `seaofstars` | `unity` | `nvn` | 2.0.59001 (v786432) | 12 | - |
| `01008CF01BAAC000` | The Legend of Zelda: Echoes of Wisdom | `zeow` | `grezzo` | `nvn` | 1.1.0 (v196608) | 12 | - |
| `01008D300C50C000` | Super Nintendo Entertainment System – Nintendo Classics | `snesnc` | `snes-online` | `nvn` | 6.0.0 (v1966080) | 15 | - |
| `01008D7016438000` | 13 Sentinels: Aegis Rim, Asian release | `sentinelsasia` | `vanillaware` | `nvn` | 1.00 (v0) | 12 | - |
| `01008D9022462000` | The Rogue Prince of Persia | `rpop` | `unity` | `nvn` | 1.1.0 (v262144) | 12 | - |
| `01008DB008C2C000` | Pokémon Shield | `pkshield` | `trinity` | `nvn` | 1.3.2 (v458752) | 12 | - |
| `01008DD013200000` | Ori and the Will of the Wisps | `oriwotw` | `unity` | `nvn` | 1.2.1 (v196608) | 12 | code stubs |
| `01008F2005154000` | South Park: The Fractured But Whole | `spfbw` | `snowdrop` | `nvn` | 1.05 (v327680) | 16 | - |
| `01008F4014A74000` | UnMetal | `unmetal` | `arcadix` | `opengl` | 1.2.0 (v262144) | 15 | no OpenGL layer |
| `01008F6008C5E000` | Pokémon Violet | `pkviolet` | `trinity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `01008F701C074000` | Sonic Superstars | `sonicss` | `unity` | `nvn` | 1.1.8 (v327680) | 12 | - |
| `010092B0091D0000` | Team Sonic Racing | `tsr` | `sumo` | `nvn` | 1.0.3 (v131072) | 12 | - |
| `01009320084A4000` | SteamWorld Dig | `swdig` | `steamworld` | `nvn` | 1.12.0 (v65536) | 12 | - |
| `010093801237C000` | Metroid Dread | `dread` | `mercurysteam` | `nvn` | 2.1.0 (v327680) | 12 | - |
| `010095300B6A4000` | South Park: The Stick of Truth | `spsot` | `onyx` | `nvn` | 1.01 (v65536) | 16 | - |
| `0100965017338000` | Super Mario Party Jamboree | `smpj` | `bezel` | `nvn` | 2.3.0 (v458752) | 12 | NROs (unchecked) |
| `010097F018538000` | DAVE THE DIVER | `davediver` | `unity` | `nvn` | v1.0.6.1267 (v3080192) | 12 | - |
| `010099C022B96000` | Super Mario Galaxy | `smg` | `hagi` | `vulkan` | 1.3.1 (v327680) | 15 | Vulkan layer parts missing |
| `01009BC00C38A000` | The Binding of Isaac: Rebirth | `isaac` | `nicalis` | `nvn` | 1.0 (v0) | 12 | - |
| `01009BF0072D4000` | Captain Toad: Treasure Tracker | `toad` | `agl` | `nvn` | 1.4.2 (v393216) | 12 | - |
| `01009D901BC56000` | Donkey Kong Country Returns HD | `dkcrhd` | `unity` | `nvn` | 1.1.0 (v65536) | 12 | - |
| `0100A00019DE0000` | Hades II | `hades2` | `supergiant` | `nvn` | 0.0.126424 (v0) | 12 | frame step stub |
| `0100A0800E9C4000` | The Witcher 3: Wild Hunt — Complete Edition | `witcher3` | `redengine` | `nvn` | 4.04b (v458752) | 15 | - |
| `0100A3501946E000` | OCTOPATH TRAVELER II | `octo2` | `unreal` | `nvn` | 1.1.1 (v131072) | 12 | - |
| `0100A3900C3E2000` | Paper Mario: The Origami King | `pmok` | `intsys` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100A3D008C5C000` | Pokémon Scarlet | `pkscarlet` | `trinity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `0100A5C00D162000` | Cuphead | `cuphead` | `unity` | `nvn` | 1.3.7 (v655360) | 12 | - |
| `0100A6301214E000` | Fire Emblem Engage | `feengage` | `unity` | `nvn` | 2.0.0 (v327680) | 12 | - |
| `0100A7C01B792000` | Minecraft Dungeons II | `mcd2` | `unreal` | `nvn` | 1.1.3.0 (v131072) | 12 | - |
| `0100A83022A22000` | Absolum | `absolum` | `fna` | `nvn` | 1.1.2 (v458752) | 12 | - |
| `0100A8E016236000` | Kirby's Dream Buffet | `kirbydb` | `hal` | `nvn` | 1.0.0 (v0) | 15 | code stubs |
| `0100A9D01C446000` | DRAGON QUEST VII Reimagined | `dq7r` | `unreal` | `nvn` | 1.2.0 (v131072) | 12 | - |
| `0100AA001415E000` | FINAL FANTASY VI | `ff6` | `unity` | `nvn` | 1.2.1_5 (v458752) | 15 | - |
| `0100AA201415C000` | FINAL FANTASY V | `ff5` | `unity` | `nvn` | 1.2.0_5 (v393216) | 15 | - |
| `0100ABD01785C000` | Portal 2 | `portal2` | `source` | `nvn` | 1.0.3 (v196608) | 12 | NROs (unchecked) |
| `0100ABF008968000` | Pokémon Sword | `pksword` | `trinity` | `nvn` | 1.3.2 (v458752) | 12 | - |
| `0100ACD0163D0000` | Batman: Arkham Knight | `batmanak` | `unreal3` | `nvn` | 1.0.5 (v327680) | 20 | - |
| `0100AE00096EA000` | Hyrule Warriors: Definitive Edition | `hwde` | `ktgl` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100B04011742000` | Monster Hunter Rise | `mhrise` | `re-engine` | `nvn` | 16.0.2 (v2228224) | 12 | - |
| `0100B7C00933A000` | Pikmin 4 | `pikmin4` | `unreal` | `nvn` | 1.1.1 (v262144) | 12 | - |
| `0100B8A026F14000` | FINAL FANTASY RESONANCE, Asian release | `ffresasia` | `unreal` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100B8E016F76000` | NieR:Automata The End of YoRHa Edition, Asian release | `nierasia` | `platinum` | `nvn` | 1.0.3 (v131072) | 21 | - |
| `0100B99019412000` | Mario vs. Donkey Kong | `mvdk` | `agl` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100BA4026E94000` | Castlevania: Belmont's Curse | `cvbc` | `unity` | `nvn` | 1.1.0 (v65536) | 12 | - |
| `0100BC0018138000` | Super Mario RPG | `smrpg` | `unity` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100BC300CB48000` | FINAL FANTASY X/X-2 HD Remaster | `ffx` | `phyre` | `nvn` | 1.0.0 (v0) | 12 | NROs (unchecked) |
| `0100BF00112C0000` | Catherine: Full Body | `catherine` | `gfd` | `nvn` | 1.0.1a (v131072) | 12 | - |
| `0100BF500207C000` | Bloodstained: Ritual of the Night | `bsrotn` | `unreal` | `nvn` | 1.60 (v851968) | 15 | - |
| `0100C1F0051B6000` | Donkey Kong Country: Tropical Freeze | `dkctf` | `retro` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `0100C2A01A730000` | ROMANCE OF THE THREE KINGDOMS 8 REMAKE | `rotk8` | `ktgl` | `nvn` | 1.1.3 (v851968) | 12 | - |
| `0100C3601518C000` | Sid Meier's Civilization VII | `civ7` | `firaxis` | `nvn` | 1.5.0.1 (v1638400) | 15 | - |
| `0100C49025D3E000` | EA SPORTS FC 27 | `fc27` | `frostbite` | `nvn` | 1.8d.310c (v393216) | 12 | NROs (unchecked) |
| `0100C510166F0000` | Chained Echoes | `chainedechoes` | `unity` | `nvn` | 1.1.1 (v131072) | 15 | - |
| `0100C9C00E25C000` | Mario Golf: Super Rush | `mgsr` | `camelot` | `nvn` | 4.0.0 (v262144) | 15 | - |
| `0100CA9002322000` | SteamWorld Dig 2 | `swdig2` | `steamworld` | `nvn` | 4.1.1 (v196608) | 12 | - |
| `0100CAA026246000` | Warhammer Survivors | `whsurvivors` | `unity` | `nvn` | 1.0.0 (v0) | 15 | - |
| `0100CC80140F8000` | TRIANGLE STRATEGY | `tristrat` | `unreal` | `nvn` | 1.1.1 (v262144) | 12 | - |
| `0100CD701434C000` | Twelve Minutes | `twelvemin` | `unity` | `nvn` | 1.0.1 (v0) | 15 | - |
| `0100CD801CE5E000` | Balatro | `balatro` | `love` | `opengl` | 1.1.3 (v851968) | 15 | no OpenGL layer |
| `0100CE4010AAC000` | FINAL FANTASY CRYSTAL CHRONICLES Remastered Edition | `ffcc` | `unity` | `nvn` | 1.0.3 (v196608) | 12 | - |
| `0100D440269E8000` | Farming Simulator 26: Nintendo Switch Edition | `fs26` | `giants` | `nvn` | 1.0.1.0 (v65536) | 15 | - |
| `0100D4900E82C000` | Metro 2033 Redux | `metro2033` | `4a` | `nvn` | 1.0.0 (v0) | 15 | - |
| `0100D5501857E000` | Sam & Max: The Devil's Playhouse | `sammax3` | `telltale` | `nvn` | 1.8.0 (v524288) | 15 | - |
| `0100D5B026844000` | Silver Pines | `silverpines` | `unity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `0100D71004694000` | Minecraft for Nintendo Switch | `minecraft` | `renderdragon` | `vulkan` | 1.26.62 (v10747904) | 15 | Vulkan layer parts missing |
| `0100D7C01F254000` | DRAGON QUEST I & II HD-2D Remake | `dq12hd` | `unreal` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `0100D800117F4000` | Re:ZERO -Starting Life in Another World- The Prophecy of the Throne | `rezero` | `the-forge` | `vulkan` | 1.0.2 (v131072) | 15 | Vulkan layer parts missing |
| `0100D9F01D474000` | Rhythm Heaven Groove | `rhgroove` | `nintendo-2d` | `nvn` | 1.0.0 (v0) | 12 | - |
| `0100DA801624E000` | TUNIC | `tunic` | `unity` | `nvn` | 1.0.3 (v196608) | 12 | - |
| `0100DBF01000A000` | Burnout Paradise Remastered | `burnout` | `criterion` | `nvn` | 1.0.0 (v0) | 15 | - |
| `0100DCA0064A6000` | Luigi's Mansion 3 | `lm3` | `next-level` | `nvn` | 1.4.0 (v327680) | 12 | - |
| `0100E44016C22000` | Patrick's Parabox | `parabox` | `unity` | `nvn` | 1.0.1 (v65536) | 12 | - |
| `0100E65002BB8000` | Stardew Valley | `stardew` | `monogame` | `nvn` | 1.6.15.13 (v1310720) | 12 | - |
| `0100E95004038000` | Xenoblade Chronicles 2 | `xc2` | `monolith` | `nvn` | 2.1.0 (v983040) | 12 | - |
| `0100EB100AB42000` | FINAL FANTASY XII THE ZODIAC AGE | `ff12` | `phyre` | `nvn` | 1.1.0 (v65536) | 15 | - |
| `0100EC9010258000` | Streets of Rage 4 | `sor4` | `monogame` | `nvn` | 1.0.9 (v458752) | 12 | - |
| `0100ECD018EBE000` | Paper Mario: The Thousand-Year Door | `pmttyd` | `intsys` | `nvn` | 1.0.1 (v65536) | 13 | frame step stub, `fps_target` 0 |
| `0100ED7012DB2000` | ROMANCE OF THE THREE KINGDOMS XIV: Diplomacy and Strategy Expansion Pack | `rotk14` | `ktgl` | `nvn` | 1.0.10 (v458752) | 12 | - |
| `0100F28011892000` | Overcooked! All You Can Eat | `overcooked` | `unity` | `nvn` | 1.0.12 (v786432) | 15 | - |
| `0100F2C0115B6000` | The Legend of Zelda: Tears of the Kingdom | `totk` | `agl` | `nvn` | 1.4.3 (v720896) | 12 | frame step stub, `fps_target` 0, NROs (unchecked) |
| `0100F43008C44000` | Pokémon Legends: Z-A | `pkza` | `trinity` | `nvn` | 2.0.2 (v393216) | 12 | - |
| `0100F5D01DA3A000` | Brain Memory Training Academy | `bmta` | `unity` | `nvn` | 1.0.0 (v0) | 12 | - |
| `0100FD8022DAA000` | Super Mario Galaxy 2 | `smg2` | `hagi` | `vulkan` | 1.4.0 (v393216) | 15 | Vulkan layer parts missing |
| `0100FF500E34A000` | Xenoblade Chronicles: Definitive Edition | `xcde` | `monolith` | `nvn` | 1.1.2 (v196608) | 12 | - |
| `040015100B514001` | Super Mario Bros. Wonder – Nintendo Switch 2 Edition + Meetup in Bellabel Park | `smbw2` | `agl` | `nvn` | 1.2.1 (v0) | 15 | - |
| `0400245021D2C000` | Yoshi and the Mysterious Book | `yoshimb` | `unreal` | `nvn` | 1.0.2 (v131072) | 12 | - |
| `040032B023CA4000` | Orbitals | `orbitals` | `unreal` | `nvn` | 1.1.2.0 (v327680) | 15 | - |
| `0400380020056000` | Splatoon Raiders | `splatraiders` | `agl` | `nvn` | v65536 | 12 | - |
| `04003F40232DE000` | Hyrule Warriors: Age of Imprisonment | `hwaoi` | `ktgl` | `nvn` | v196608 | 12 | - |
| `040041E0212AE000` | Fast Fusion | `fastfusion` | `shinen` | `nvn` | v262144 | 15 | - |
| `0400486024DEC000` | The Legend of Zelda: Ocarina of Time | `oot` | `agl` | `nvn` | v131072 | 1 | own section above |
| `0400569027BF4000` | Resident Evil 4 | `re4` | `re-engine` | `nvn` | v131072 | 15 | - |
| `040057C02159A000` | Nintendo Switch 2 Welcome Tour | `welcometour` | `bezel` | `nvn` | ? (v262144) | 15 | - |
| `040074A01BF12000` | Donkey Kong Bananza | `dkbananza` | `agl` | `nvn` | v458752 | 12 | - |
| `040077201BE50000` | Kirby Air Riders | `kirbyar` | `phx` | `nvn` | 1.3.3 (v458752) | 12 | - |
| `04007EF01F8AE000` | Fire Emblem Fortune's Weave | `fefw` | `intsys` | `nvn` | v131072 | 12 | - |
| `04008E001F098000` | Nintendo Switch Sports Resort | `nssr` | `agl` | `nvn` | v65536 | 12 | - |
| `0400921025B32000` | Sonic Frontiers Definitive Edition | `sonicfd` | `hedgehog` | `nvn` | 1.0.0 (v65536) | 15 | - |
| `0400A3A01EE4C000` | Pokémon Pokopia | `pkpokopia` | `ktgl` | `nvn` | v655360 | 12 | - |
| `0400C3F00006E000` | Mario Kart World | `mkw` | `agl` | `nvn` | v0 | 12 | frame step stub, `fps_target` 0 |
| `0400C76024710000` | PRAGMATA | `pragmata` | `re-engine` | `nvn` | v196608 | 12 | - |
| `0400D4201C6EA000` | Mario Tennis Fever | `mtf` | `camelot` | `nvn` | v327680 | 12 | own section above |
| `0400E88025B1A000` | Star Fox | `starfox` | `vrender` | `nvn` | v131072 | 12 | - |

The folders without a module:

- `010049900F546000` Super Mario 3D All-Stars: an application of several programs (a launcher, Super Mario 64, Super
  Mario Sunshine, and Galaxy, which is 32-bit ARM). nxb build makes one program, and Super Mario 64 runs its code through nn::jit,
  which nxkit stops.
- `010049900F546001` Super Mario 3D All-Stars: Super Mario 64: a program of Super Mario 3D All-Stars (program_of), not a
  game of its own. It runs its code through nn::jit, which nxkit stops.
- `010049900F546002` Super Mario 3D All-Stars: Super Mario Sunshine: a program of Super Mario 3D All-Stars (program_of),
  not a game of its own.
- `010037C022390000` Hitman: Absolution: the folder holds only `feral_defaults.tsv` for the packer, no `game.tsv`.
