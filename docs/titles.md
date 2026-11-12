# Title layer

Three layers decide how one game is built and run.

- The core is the rest of the workspace. It knows no game and no engine.
- An engine family holds what is true for every game of one engine. The family `agl` is Nintendo EPD's engine on sead, agl and gsys.
- A title module holds what is true for one game only.

The API is the crate `nxtitle`. The crate `nxtitles` lists the built-in families and title modules.

## Folders

```
titles/
  nxtitle/              the API: traits, hooks, evidence, the catalog
  nxtitles/             the registry, one cargo feature per module
  families/<id>/        crate nxfamily-<id>
  games/<title id>/     crate nxgame-<short name>, with the game's profile.toml
```

## Who answers a question

A family and a title answer questions through hooks. `None` means no opinion.

- A single answer comes from the title first, then the family, then the core default.
- A list adds up. The family's entries come first, then the title's.

| hook | trait | answer |
|---|---|---|
| `standalone` | `BuildHooks` | what `nxb build` needs to make the game: the version, the modules with their build IDs, the NVN function list, the shader split settings, the run settings and memory layout of the game (`game.json` keys `settings` and `memory`) and a romfs marker. A title that answers it builds without `--engine` |
| `shader_role` | `BuildHooks` | what a shader does for the frame, for example the game's own upscaler |
| `seeds` | `BuildHooks` | shaders to prepare before the first frame |
| `rewrite_rules` | `BuildHooks` | rewriter rules as TOML text |
| `resource_patches` | `BuildHooks` | edits of one member of a game archive that `nxb build` applies to the app's RomFS, by a build option (`--resource-patch <option>=<value>`) or the patch's default. Lists of all layers add up |
| `needs` | `RuntimeHooks` | host behaviours the game depends on, with the setting. `nxb build` writes the need `vertex_stream_null_bind` with the setting `ignore` as the key of that name in `settings` of `game.json`. |
| `upscaler` | `RuntimeHooks` | keep the game's upscaler, render at the output size, or use MetalFX |
| `pacing` | `RuntimeHooks` | the game, the display or a fixed rate paces the frames |
| `pace_profile` | `RuntimeHooks` | the keys that steer the frame step and the presents |
| `limits` | `RuntimeHooks` | where the game's own frame limit sits, so that the host can write the target into it |
| `runtime` | `RuntimeHooks` | whether the nxkit runtime runs one service of the game |

A role names what a shader does. The core decides what to do with it. A title module does not change the core's code path for a shader.

## Which engine runs a game

The host is split into services (`Service`): `fs`, `input`, `audio`, `video`, `system`, `offline`, `pacing`, `kernel` and `gpu`. A family or title answers `runtime` for a service with `Runtime::Nxkit`, and the nxkit runtime runs that service for the game. `Resolved::nxkit_services` lists them.

The plan for the nxkit runtime and for applying needs is [plans/runtime-hooks.md](plans/runtime-hooks.md).

## Evidence

Every answer is a `Fact`, a value with its `Evidence`.

- `Tested` names a test in this workspace.
- `Recorded` names a record outside the workspace, for example a game README at a commit, and the day it was read.
- `Unverified` says why the fact is believed and what would check it.

The core can refuse an unverified answer, for example in a release build.

## Rules for a module

1. A title module does not work around a core bug. A fix that helps more than one game goes into the core.
2. A fact about engine code goes into the family, even when one game showed it. Its evidence names that game.
3. A fact about one game's content stays in the title module.
4. A title names its family by id. It does not depend on the family crate. Without the family, the title still works with the core defaults.
5. Only `nxtitles` depends on family and title crates. The core depends on `nxtitle` and `nxtitles`. `cargo run -p xtask -- layers` checks this.
6. The committed part of a game's profile (design 2.11) is `profile.toml` in the title module's folder. Keys that a capability check decides are not in it. A family or title states them as needs.

## Adding a game

1. Make `titles/games/<title id>/` with a crate `nxgame-<short name>` of layer `T`.
2. Implement `Title`, `BuildHooks` and `RuntimeHooks`. Leave every hook without an answer at its default.
3. Add `profile.toml` with `title_id` and `[declared] title`. A test parses it with `nxb-profile` and expects no problems.
4. Add the crate to `[workspace.dependencies]` and add a feature for it in `titles/nxtitles/Cargo.toml` and `catalog()`.

A new family follows the same steps in `titles/families/<id>/` with `Family` in place of `Title`.

## The Legend of Zelda: Ocarina of Time (`0400486024DEC000`)

The Nintendo Switch 2 game on NVN2, family `agl`. The records are the game notes at a commit, with the day they were read.

- The family marks the `agl_ndsr_vr` networks as the game's upscaler. The archive path that identifies them is unverified.
- The title states `draw_barriers`, `mem_map_limit`, `render_cond`, `tex_flush_upload` and `tex_dirty_upload`. The last three come from agl's depth prepass and the terrain maps the CPU writes. They are title facts, because Breath of the Wild sets none of them.
- The game builds with `nxb build --nup <game>.nup`. The title module gives update v131072, the modules `main`, `subsdk0` and `sdk` with their build IDs, the shader groups, the marker `Pack/ZsDic.pack.zs` and the run settings recorded at `37f076a5` (memory split, `sys_resource`, `mem_map_limit`, `fps_target 120` with `fps_max 120` (the record says 60; 120 is not checked in the game yet), `pad_layout full`, the graphics keys).
- The NVN function list is not a hand list. The title module gives an empty list, and the build takes the `nvn…` names in the read-only data of the modules: 774 names in v131072 (738 in `main`, 36 more in `sdk`). That is what the game can fetch through the NVN loader, more than one route calls.
- `fps_target 60` and `120` need the frame ratio stub (`MATCH_FRAME_STEP`). It is the one rule of `rules/0400486024DEC000.toml`, a `frame-step` rule for main build `e0f3ccfaf50065ae4ea01eb5aeb13b1a`: at `main+0x6e6cc4` (`fdiv s8,s1,s0`) the ratio becomes the host's step × base rate / 60, with the step in vsyncs of 1/60 s: the whole step (state page `+0x20`) at 60, and at 120 the variable step (`+0x50`, 1/30 s units) times 2, so half the ratio per 120 Hz tick. The game steps its time by that ratio, so the pacing is the display's. The dynamic resolution stub of the same file is a separate change.
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
- `fps_target 60` needs the frame-step stubs. They are the 168 steps of `rules/01007EF00011E000.toml` in the title module, which `rewrite_rules()` gives to the build.  These steps have the variable step, the render pool, the heap growth and the output size. The screen shape, layout and FOV stubs (about 2900 more steps) are not part of the file. They use the same sites and state page offsets as the variable step, so they need their own state page layout and host support first, and `screen_aspect` does nothing until then.
- The settings are recorded values. R19 sets the target to 120 fps through the same frame step stubs, so `fps_target 60` is a record and not a cap. R10 and R17 replace `present_snap`, `enc_thread`, `core_preempt`, `draw_barriers_game` and the `tex_*` keys. The host reader reads no `settings` object yet.
- `Settings` names no engine. An engine with a texture pool or a frame rate variable uses `texture_pool_mb` and `fps_cvar`. Two earlier games use these values. No Unreal family exists in nxkit yet, so those two games are not taken along.
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
  `patches.py` (`MATCH_CUTS`), the engine's SDK-wide rules for `sdk` (node area 62 MB, regions aligned to 2 MB; one
  `[[rule]]` per reviewed implementation, so 12 in all) and, for a Unity 2018 game, the engine-wide rule of
  `engine_patches.py`. Each rule names its source in `evidence`. A frame step stub (`FRAME_STEP`, `MATCH_FRAME_STEP`)
  and code stubs (`STUBS`) are not byte cuts and are left out. A game whose `fps_target` needs the left-out stub gets
  `fps_target 0`, as the engine gives a build without its stub.
- All nine services run on nxkit (unverified: no game ran on nxkit yet).
- `shared_font_decode`: `false` on SDK 10 and older (the engine's rule), the value of `shared_font`, or `false` where the
  README says that `main` does not import `ScalableFontEngineHelper::Decode`. On SDK 11 and later without a record the
  module gives no answer, and the core default (the TTF itself) holds.
- The family is `agl` when the engine column names sead and agl. No other engine has a family in nxkit: a family holds
  facts that every game of an engine shares, and the engine tree records none for the other engines in a form that a
  hook takes.
- `shader_facts.tsv` is the folder's `shader_facts_manual.tsv`, where it has one.

The tests of each module parse its profile and its rules with `nxb-rewrite` and count the rules. The rules of Pokémon
Scarlet were run through the rewriter on the extracted v0: both SDK-wide rules match its sdk once. `nxb build
--dry-run` was run on Pokémon Scarlet (v0, the module's version; the update v786432 in the folder is not used).

Notes: "frame step stub" means that `rules` leave a frame step stub out, "`fps_target` 0" that the module sets 0 for it,
"code stubs" that `STUBS` are left out, "`snowdrop_pools`" that the build places Snowdrop's pools with the defaults
(the game's are 2048, 512 and 512 MB), and "NROs (unchecked)" that the game loads NRO modules through `nn::ro`.

| Title ID | Game | Feature | Family | Version | Rules | Notes |
|---|---|---|---|---|---|---|
| `0100000000010000` | Super Mario Odyssey | `smo` | agl | 1.4.1 (v393216) | 12 | - |
| `0100000011D90000` | Pokémon Brilliant Diamond | `pkbd` | - | 1.3.0 (v393216) | 12 | - |
| `010005501E68C000` | Ace Attorney Investigations Collection | `aaic` | - | 1.0.0.2 (v131072) | 12 | - |
| `010012101468C000` | Metroid Prime Remastered | `mpr` | - | 1.0.0 (v0) | 12 | - |
| `010013C00E930000` | Hollow Knight: Silksong | `silksong` | - | 1.0.30000 (v655360) | 12 | - |
| `010014C011146000` | Subnautica: Below Zero | `snbz` | - | 1.23.55063 (v655360) | 12 | - |
| `0100152000022000` | Mario Kart 8 Deluxe | `mk8d` | agl | 4.0.1 (v1507328) | 12 | - |
| `010016100A186000` | SteamWorld Heist: Ultimate Edition | `swheist` | - | 2.2.1 (v65536) | 12 | - |
| `0100199023B2E000` | Atelier Ryza: Ever Darkness & the Secret Hideout DX | `ryza` | - | 1.2.0 (v65536) | 12 | - |
| `010019A01E2F2000` | Metroid Prime 4: Beyond | `mp4` | - | 1.1.0 (v131072) | 12 | - |
| `01001B300B9BE000` | Diablo III: Eternal Collection | `diablo3` | - | 2.7.7.92380 (v1441792) | 12 | - |
| `01001F5010DFA000` | Pokémon Legends: Arceus | `pkarceus` | - | 1.1.1 (v262144) | 13 | - |
| `010020400E1C2000` | Raging Loop | `ragingloop` | - | 1.0.0 (v0) | 13 | - |
| `010020D01AD24000` | ANIMAL WELL | `animalwell` | - | 1.0.10 (v655360) | 12 | - |
| `0100276009872000` | ŌKAMI HD | `okami` | - | 1.0.0 (v0) | 12 | - |
| `010028600EBDA000` | Super Mario 3D World + Bowser's Fury | `sm3dwbf` | agl | 1.2.2 (v262144) | 12 | - |
| `010029B0118E8000` | Need for Speed: Hot Pursuit Remastered | `nfshp` | - | 1.0.3 (v196608) | 13 | - |
| `010029B018432000` | Ghost Trick: Phantom Detective | `ghosttrick` | - | 1.0.0 (v0) | 12 | - |
| `01002B00111A2000` | Hyrule Warriors: Age of Calamity | `hwaoc` | - | 1.3.0 (v327680) | 12 | frame step stub, `fps_target` 0 |
| `01002B30028F6000` | Celeste | `celeste` | - | 1.4.1.1 (v589824) | 12 | - |
| `01002CD00A51C000` | Baba Is You | `baba` | - | 1.0.12 (v786432) | 12 | - |
| `01002DA013484000` | The Legend of Zelda: Skyward Sword HD | `sshd` | - | 1.0.1 (v65536) | 12 | - |
| `01002E7016C46000` | Cult of the Lamb | `cotl` | - | 1.5.28 (v1966080) | 12 | - |
| `0100300012F2A000` | Advance Wars 1+2: Re-Boot Camp | `aw12` | - | 1.0.0 (v0) | 12 | - |
| `0100317013770000` | Mario + Rabbids Sparks of Hope | `mrsoh` | - | 1.6.2225577 (v393216) | 13 | - |
| `010031F019294000` | Super Monkey Ball Banana Rumble | `smbbr` | - | 2.20 (v458752) | 12 | - |
| `010032E00E6E2000` | Return of the Obra Dinn | `obradinn` | - | 1.0.1 (v65536) | 12 | - |
| `0100346017304000` | Taiko Risshiden V DX | `trvdx` | - | 1.1.1 (v131072) | 15 | - |
| `010035F022078000` | DRAGON BALL: Sparking! ZERO | `dbspz` | - | 2.0.76 (v327680) | 12 | - |
| `010036E00FB20000` | The Great Ace Attorney Chronicles | `tgaac` | - | 1.0.0 (v0) | 12 | - |
| `010038600B27E000` | Bastion | `bastion` | - | 1.0.0 (v0) | 12 | - |
| `010038B015560000` | FINAL FANTASY TACTICS - The Ivalice Chronicles | `fftic` | - | 1.4.0 (v393216) | 14 | - |
| `01003E601E324000` | DRAGON QUEST III HD-2D Remake | `dq3hd` | - | 1.1.2 (v262144) | 12 | - |
| `01003FC01670C000` | 13 Sentinels: Aegis Rim | `sentinels` | - | 1.10 (v65536) | 12 | - |
| `010040801A4C8000` | Blasphemous 2 | `blasphemous2` | - | 1.0.8 (v524288) | 12 | - |
| `010042000A986000` | DRAGON QUEST BUILDERS 2 | `dqb2` | - | 1.7.3d (v393216) | 12 | - |
| `0100429011144000` | Subnautica | `subnautica` | - | 1.23.83358 (v458752) | 12 | - |
| `010045C014650000` | 13 Sentinels: Aegis Rim, Japanese release | `sentinelsjp` | - | 1.00 (v0) | 12 | - |
| `01004A4010FEA000` | Bayonetta 3 | `bayo3` | - | 1.2.0 (v131072) | 12 | - |
| `01004AB00A260000` | DARK SOULS: REMASTERED | `dsr` | - | 1.0.3 (v196608) | 14 | - |
| `01004D300C5AE000` | Kirby and the Forgotten Land | `kirbyfl` | - | 1.1.0 (v65536) | 12 | - |
| `010051701FB46000` | Monster Train 2 | `mt2` | - | 1.1.1 (v131072) | 12 | - |
| `01005270232F2000` | Octopath Traveler 0 | `octo0` | - | 1.0.5 (v327680) | 12 | - |
| `010054B01AD92000` | Unicorn Overlord, Asian release | `unicornasia` | - | 1.05 (v262144) | 12 | - |
| `010055D009F78000` | Fire Emblem: Three Houses | `fe3h` | - | 1.2.0 (v327680) | 19 | - |
| `010056B015FE8000` | NieR:Automata The End of YoRHa Edition | `nier` | - | 1.0.3 (v131072) | 21 | - |
| `010057D006492000` | OCTOPATH TRAVELER | `octo` | - | 1.0.5 (v327680) | 12 | - |
| `01005CA01580E000` | Persona 5 Royal | `p5r` | - | 1.0.2 (v65536) | 12 | - |
| `01006000040C2000` | Yoshi's Crafted World | `yoshicw` | - | 1.0.1 (v65536) | 12 | - |
| `0100606026F12000` | FINAL FANTASY RESONANCE | `ffres` | - | 1.0.1 (v65536) | 12 | - |
| `010061D00DB74000` | Ori and the Blind Forest: Definitive Edition | `oribf` | - | 1.0.2 (v131072) | 13 | - |
| `0100620012D6E000` | Big Brain Academy: Brain vs. Brain | `bba` | - | 1.2.0 (v131072) | 12 | - |
| `010062B01525C000` | Persona 4 Golden | `p4g` | - | 1.0.2 (v131072) | 12 | - |
| `0100633007D48000` | Hollow Knight | `hollowknight` | - | 1.5.12620 (v589824) | 12 | - |
| `010063301BD50000` | SUPER ROBOT WARS Y | `srwy` | - | 1.4.1 (v720896) | 12 | - |
| `0100646009FBE000` | Dead Cells | `deadcells` | - | 1.25.0 (v3014656) | 12 | - |
| `010067300059A000` | Mario + Rabbids Kingdom Battle | `mrkb` | - | 1.9.589692 (v589824) | 15 | `snowdrop_pools` |
| `010069401ADB8000` | Unicorn Overlord | `unicorn` | - | 1.05 (v262144) | 12 | - |
| `0100698009C6E000` | Blasphemous | `blasphemous` | - | 1.0.8 (v524288) | 12 | - |
| `010069C01AB82000` | SHIN MEGAMI TENSEI V: Vengeance | `smt5v` | - | 1.0.3 (v196608) | 12 | - |
| `01006A800016E000` | Super Smash Bros. Ultimate | `ssbu` | - | 13.0.5 (v2031616) | 24 | NROs (unchecked) |
| `01006BB00C6F0000` | The Legend of Zelda: Link's Awakening | `zla` | - | 1.1.0 (v131072) | 13 | - |
| `01006C300E9F0000` | DRAGON QUEST XI S: Echoes of an Elusive Age – Definitive Edition | `dq11s` | - | 1.0.4 (v262144) | 12 | - |
| `01006D0017F7A000` | Mario & Luigi: Brothership | `mlbs` | - | 1.0.0 (v0) | 12 | - |
| `01006FE013472000` | Mario Party Superstars | `mps` | - | 1.1.1 (v131072) | 12 | NROs (unchecked) |
| `0100713016CCC000` | AI: THE SOMNIUM FILES - nirvanA Initiative | `aisni` | - | 1.0.2 (v131072) | 12 | - |
| `0100726014352000` | Diablo II: Resurrected | `d2r` | - | 1.0.34.0 (v2228224) | 15 | - |
| `01007300020FA000` | ASTRAL CHAIN | `astral` | - | 1.0.1 (v65536) | 12 | frame step stub, `fps_target` 0 |
| `010074F013262000` | Xenoblade Chronicles 3 | `xc3` | - | 2.2.1 (v720896) | 18 | - |
| `010076F0049A2000` | Bayonetta | `bayo` | - | 1.1 (v65536) | 12 | - |
| `01007820196A6000` | Red Dead Redemption | `rdr` | - | 1.0.7 (v458752) | 12 | - |
| `01007960049A0000` | Bayonetta 2 | `bayo2` | - | 1.2 (v131072) | 12 | - |
| `01007A3009184000` | Princess Peach: Showtime! | `peach` | - | 1.0.0 (v0) | 12 | - |
| `01007BB017812000` | Portal | `portal` | - | 1.0.3 (v196608) | 12 | NROs (unchecked) |
| `01007E3006DDA000` | Kirby Star Allies | `kirbysa` | - | 4.0.0a (v393216) | 12 | - |
| `010086A022444000` | BALL x PIT | `ballxpit` | - | 1.293 (v393216) | 12 | - |
| `010089B00D09C000` | AI: THE SOMNIUM FILES | `ais` | - | 1.0.1 (v65536) | 12 | - |
| `010089C01E9CA000` | SteamWorld Heist II | `swheist2` | - | 1.5.1 (v262144) | 12 | - |
| `01008BC024FDE000` | STEINS;GATE RE:BOOT | `steinsgate` | - | 1.0.3 (v131072) | 12 | - |
| `01008C0016544000` | Sea of Stars | `seaofstars` | - | 2.0.59001 (v786432) | 12 | - |
| `01008CF01BAAC000` | The Legend of Zelda: Echoes of Wisdom | `zeow` | - | 1.1.0 (v196608) | 12 | - |
| `01008D7016438000` | 13 Sentinels: Aegis Rim, Asian release | `sentinelsasia` | - | 1.00 (v0) | 12 | - |
| `01008D9022462000` | The Rogue Prince of Persia | `rpop` | - | 1.1.0 (v262144) | 12 | - |
| `01008DB008C2C000` | Pokémon Shield | `pkshield` | - | 1.3.2 (v458752) | 12 | - |
| `01008DD013200000` | Ori and the Will of the Wisps | `oriwotw` | - | 1.2.1 (v196608) | 13 | code stubs |
| `01008F6008C5E000` | Pokémon Violet | `pkviolet` | - | 1.0.0 (v0) | 12 | - |
| `01008F701C074000` | Sonic Superstars | `sonicss` | - | 1.1.8 (v327680) | 12 | - |
| `010092B0091D0000` | Team Sonic Racing | `tsr` | - | 1.0.3 (v131072) | 12 | - |
| `01009320084A4000` | SteamWorld Dig | `swdig` | - | 1.12.0 (v65536) | 12 | - |
| `010093801237C000` | Metroid Dread | `dread` | - | 2.1.0 (v327680) | 12 | - |
| `0100965017338000` | Super Mario Party Jamboree | `smpj` | - | 2.3.0 (v458752) | 12 | NROs (unchecked) |
| `010097F018538000` | DAVE THE DIVER | `davediver` | - | v1.0.6.1267 (v3080192) | 12 | - |
| `01009BC00C38A000` | The Binding of Isaac: Rebirth | `isaac` | - | 1.0 (v0) | 12 | - |
| `01009BF0072D4000` | Captain Toad: Treasure Tracker | `toad` | agl | 1.4.2 (v393216) | 12 | - |
| `01009D901BC56000` | Donkey Kong Country Returns HD | `dkcrhd` | - | 1.1.0 (v65536) | 12 | - |
| `0100A00019DE0000` | Hades II | `hades2` | - | 0.0.126424 (v0) | 12 | frame step stub |
| `0100A3501946E000` | OCTOPATH TRAVELER II | `octo2` | - | 1.1.1 (v131072) | 12 | - |
| `0100A3900C3E2000` | Paper Mario: The Origami King | `pmok` | - | 1.0.1 (v65536) | 12 | - |
| `0100A3D008C5C000` | Pokémon Scarlet | `pkscarlet` | - | 1.0.0 (v0) | 12 | - |
| `0100A5C00D162000` | Cuphead | `cuphead` | - | 1.3.7 (v655360) | 12 | - |
| `0100A6301214E000` | Fire Emblem Engage | `feengage` | - | 2.0.0 (v327680) | 12 | - |
| `0100A7C01B792000` | Minecraft Dungeons II | `mcd2` | - | 1.1.3.0 (v131072) | 12 | - |
| `0100A83022A22000` | Absolum | `absolum` | - | 1.1.2 (v458752) | 12 | - |
| `0100A9D01C446000` | DRAGON QUEST VII Reimagined | `dq7r` | - | 1.2.0 (v131072) | 12 | - |
| `0100ABD01785C000` | Portal 2 | `portal2` | - | 1.0.3 (v196608) | 12 | NROs (unchecked) |
| `0100ABF008968000` | Pokémon Sword | `pksword` | - | 1.3.2 (v458752) | 12 | - |
| `0100ACD0163D0000` | Batman: Arkham Knight | `batmanak` | - | 1.0.5 (v327680) | 20 | - |
| `0100AE00096EA000` | Hyrule Warriors: Definitive Edition | `hwde` | - | 1.0.1 (v65536) | 12 | - |
| `0100B04011742000` | Monster Hunter Rise | `mhrise` | - | 16.0.2 (v2228224) | 12 | - |
| `0100B7C00933A000` | Pikmin 4 | `pikmin4` | - | 1.1.1 (v262144) | 12 | - |
| `0100B8A026F14000` | FINAL FANTASY RESONANCE, Asian release | `ffresasia` | - | 1.0.1 (v65536) | 12 | - |
| `0100B8E016F76000` | NieR:Automata The End of YoRHa Edition, Asian release | `nierasia` | - | 1.0.3 (v131072) | 21 | - |
| `0100B99019412000` | Mario vs. Donkey Kong | `mvdk` | agl | 1.0.1 (v65536) | 12 | - |
| `0100BA4026E94000` | Castlevania: Belmont's Curse | `cvbc` | - | 1.1.0 (v65536) | 12 | - |
| `0100BC0018138000` | Super Mario RPG | `smrpg` | - | 1.0.1 (v65536) | 12 | - |
| `0100BC300CB48000` | FINAL FANTASY X/X-2 HD Remaster | `ffx` | - | 1.0.0 (v0) | 12 | NROs (unchecked) |
| `0100BF00112C0000` | Catherine: Full Body | `catherine` | - | 1.0.1a (v131072) | 12 | - |
| `0100C1F0051B6000` | Donkey Kong Country: Tropical Freeze | `dkctf` | - | 1.0.2 (v131072) | 12 | - |
| `0100C2A01A730000` | ROMANCE OF THE THREE KINGDOMS 8 REMAKE | `rotk8` | - | 1.1.3 (v851968) | 12 | - |
| `0100C49025D3E000` | EA SPORTS FC 27 | `fc27` | - | 1.8d.310c (v393216) | 12 | NROs (unchecked) |
| `0100CA9002322000` | SteamWorld Dig 2 | `swdig2` | - | 4.1.1 (v196608) | 12 | - |
| `0100CC80140F8000` | TRIANGLE STRATEGY | `tristrat` | - | 1.1.1 (v262144) | 12 | - |
| `0100CE4010AAC000` | FINAL FANTASY CRYSTAL CHRONICLES Remastered Edition | `ffcc` | - | 1.0.3 (v196608) | 13 | - |
| `0100D5B026844000` | Silver Pines | `silverpines` | - | 1.0.0 (v0) | 12 | - |
| `0100D7C01F254000` | DRAGON QUEST I & II HD-2D Remake | `dq12hd` | - | 1.0.2 (v131072) | 12 | - |
| `0100D9F01D474000` | Rhythm Heaven Groove | `rhgroove` | - | 1.0.0 (v0) | 12 | - |
| `0100DA801624E000` | TUNIC | `tunic` | - | 1.0.3 (v196608) | 12 | - |
| `0100DCA0064A6000` | Luigi's Mansion 3 | `lm3` | - | 1.4.0 (v327680) | 12 | - |
| `0100E44016C22000` | Patrick's Parabox | `parabox` | - | 1.0.1 (v65536) | 12 | - |
| `0100E65002BB8000` | Stardew Valley | `stardew` | - | 1.6.15.13 (v1310720) | 12 | - |
| `0100E95004038000` | Xenoblade Chronicles 2 | `xc2` | - | 2.1.0 (v983040) | 12 | - |
| `0100EC9010258000` | Streets of Rage 4 | `sor4` | - | 1.0.9 (v458752) | 12 | - |
| `0100ECD018EBE000` | Paper Mario: The Thousand-Year Door | `pmttyd` | - | 1.0.1 (v65536) | 13 | frame step stub, `fps_target` 0 |
| `0100ED7012DB2000` | ROMANCE OF THE THREE KINGDOMS XIV: Diplomacy and Strategy Expansion Pack | `rotk14` | - | 1.0.10 (v458752) | 12 | - |
| `0100F2C0115B6000` | The Legend of Zelda: Tears of the Kingdom | `totk` | agl | 1.4.3 (v720896) | 12 | frame step stub, `fps_target` 0, NROs (unchecked) |
| `0100F43008C44000` | Pokémon Legends: Z-A | `pkza` | - | 2.0.2 (v393216) | 12 | - |
| `0100F5D01DA3A000` | Brain Memory Training Academy | `bmta` | - | 1.0.0 (v0) | 12 | - |
| `0100FF500E34A000` | Xenoblade Chronicles: Definitive Edition | `xcde` | - | 1.1.2 (v196608) | 12 | - |
| `0400245021D2C000` | Yoshi and the Mysterious Book | `yoshimb` | - | 1.0.2 (v131072) | 12 | - |
| `0400380020056000` | Splatoon Raiders | `splatraiders` | agl | v65536 | 12 | - |
| `04003F40232DE000` | Hyrule Warriors: Age of Imprisonment | `hwaoi` | - | v196608 | 12 | - |
| `040074A01BF12000` | Donkey Kong Bananza | `dkbananza` | agl | v458752 | 12 | - |
| `040077201BE50000` | Kirby Air Riders | `kirbyar` | - | 1.3.3 (v458752) | 12 | - |
| `04007EF01F8AE000` | Fire Emblem Fortune's Weave | `fefw` | - | v131072 | 12 | - |
| `04008E001F098000` | Nintendo Switch Sports Resort | `nssr` | agl | v65536 | 12 | - |
| `0400A3A01EE4C000` | Pokémon Pokopia | `pkpokopia` | - | v655360 | 12 | - |
| `0400C3F00006E000` | Mario Kart World | `mkw` | agl | v0 | 12 | frame step stub, `fps_target` 0 |
| `0400C76024710000` | PRAGMATA | `pragmata` | - | v196608 | 12 | - |
| `0400E88025B1A000` | Star Fox | `starfox` | - | v131072 | 12 | - |

The folders without a module:

- `010014E00DB56000` Digimon Story Cyber Sleuth: Complete Edition: OpenGL game (MVGL): nxkit has no OpenGL layer.
- `0100217023F6C000` Well Dweller: OpenGL game (GameMaker runner): nxkit has no OpenGL layer.
- `010049900F546000` Super Mario 3D All-Stars: an application of several programs (a launcher, Super Mario 64, Super
  Mario Sunshine; Galaxy is 32-bit ARM): nxb build makes one program, and Super Mario 64 runs its code through nn::jit,
  which nxkit stops.
- `010049900F546001` Super Mario 3D All-Stars: Super Mario 64: a program of Super Mario 3D All-Stars (program_of), not a
  game of its own; it runs its code through nn::jit, which nxkit stops.
- `010049900F546002` Super Mario 3D All-Stars: Super Mario Sunshine: a program of Super Mario 3D All-Stars (program_of),
  not a game of its own.
- `0100535012974000` Hades: Vulkan game (The Forge): nxkit has no Vulkan layer, nxb builds only NVN games.
- `010099C022B96000` Super Mario Galaxy: Vulkan game (recompiled PowerPC Wii layer): nxkit has no Vulkan layer.
- `0100CD801CE5E000` Balatro: OpenGL game (LOVE): nxkit has no OpenGL layer.
- `0100FD8022DAA000` Super Mario Galaxy 2: Vulkan game (recompiled PowerPC Wii layer): nxkit has no Vulkan layer.
