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
| `standalone` | `BuildHooks` | what `nxb build` needs to make the game without nxnative: the version, the modules with their build IDs, the NVN function list, the shader split settings, the run settings and memory layout of the game (`game.json` keys `settings` and `memory`) and a romfs marker. A title that answers it builds without `--engine` |
| `shader_role` | `BuildHooks` | what a shader does for the frame, for example the game's own upscaler |
| `seeds` | `BuildHooks` | shaders to prepare before the first frame |
| `rewrite_rules` | `BuildHooks` | rewriter rules as TOML text |
| `needs` | `RuntimeHooks` | host behaviours the game depends on, with the setting. `nxb build` writes the need `vertex_stream_null_bind` with the setting `ignore` as the key of that name in `settings` of `game.json`. |
| `upscaler` | `RuntimeHooks` | keep the game's upscaler, render at the output size, or use MetalFX |
| `pacing` | `RuntimeHooks` | the game, the display or a fixed rate paces the frames |
| `pace_profile` | `RuntimeHooks` | the keys that steer the frame step and the presents |
| `limits` | `RuntimeHooks` | where the game's own frame limit sits, so that the host can write the target into it |
| `runtime` | `RuntimeHooks` | whether nxnative or nxkit runs one service of the game |

A role names what a shader does. The core decides what to do with it. A title module does not change the core's code path for a shader.

## Which engine runs a game

The host is split into services (`Service`): `fs`, `input`, `audio`, `video`, `system`, `offline`, `pacing`, `kernel` and `gpu`. nxnative runs every service by default. A family or title answers `runtime` for a service with `Runtime::Nxkit`, and the nxkit runtime runs that service for the game. `Resolved::nxkit_services` lists them.

The plan for the nxkit runtime and for applying needs is [plans/runtime-hooks.md](plans/runtime-hooks.md).

## Evidence

Every answer is a `Fact`, a value with its `Evidence`.

- `Tested` names a test in this workspace.
- `Recorded` names a record outside the workspace, for example a game README in nxnative at a commit, and the day it was read.
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

The Nintendo Switch 2 game on NVN2, family `agl`. The records are in `seedds/nxnative` `dm/games/0400486024DEC000/README.md`.

- The family marks the `agl_ndsr_vr` networks as the game's upscaler. The archive path that identifies them is unverified.
- The title states `gpu_timestamps`, `draw_barriers`, `mem_map_limit`, `render_cond`, `tex_flush_upload` and `tex_dirty_upload`. The last three come from agl's depth prepass and the terrain maps the CPU writes. They are title facts, because Breath of the Wild sets none of them.
- The game builds without nxnative: `nxb build --nup <game>.nup`. The title module gives update v131072, the modules `main`, `subsdk0` and `sdk` with their build IDs, the shader groups, the marker `Pack/ZsDic.pack.zs` and the run settings of nxnative's `game.tsv` at `37f076a5` (memory split, `sys_resource`, `mem_map_limit`, `fps_target 60`, `pad_layout full`, the graphics keys).
- The NVN function list is not a hand list. The title module gives an empty list, and the build takes the `nvn…` names in the read-only data of the modules: 774 names in v131072 (738 in `main`, 36 more in `sdk`). That is what the game can fetch through the NVN loader, more than one route calls.
- `fps_target 60` needs the frame ratio stub of nxnative's `patches.py` (`MATCH_FRAME_STEP`). It is the one rule of `rules/0400486024DEC000.toml`, a `frame-step` rule for main build `e0f3ccfaf50065ae4ea01eb5aeb13b1a`: at `main+0x6e6cc4` (`fdiv s8,s1,s0`) the ratio becomes the host's step × base rate / 60. The game steps its time by that ratio, so the pacing is the display's. The dynamic resolution stub of the same file is a separate change.
- The `.nup` of v131072 is one standalone update: 186,587 RomFS files, 26.2 GB. 146,000 files (23.2 GB) are stored raw and 40,600 (3.0 GB) as one zstd frame each. The build writes the RomFS as a folder in one pass with the shader split (no `RomFS.img`), and links the files into the app.

Open questions:

- Should the host keep `agl_ndsr_vr`, render at the output size, or use MetalFX?
- Which facts of the family hold for Tears of the Kingdom and other agl games?

## The Legend of Zelda: Breath of the Wild (`01007EF00011E000`)

The Nintendo Switch game on NVN, update 1.9.0 (v1114112), family `agl`. The records are in `seedds/nxnative` `dm/games/01007EF00011E000/`.

- The title states `gpu_timestamps segment`, `fps_target 60`, `present_snap`, `pso_mtl4` and `screen_aspect`.
- `fps_target 60` needs the frame-step stubs. They are the 168 steps of `rules/01007EF00011E000.toml` in the title module, which `rewrite_rules()` gives to the build. No file in `data/rewrite-rules` holds rules of this game. These steps have the variable step, the render pool, the heap growth and the output size. The screen shape, layout and FOV stubs of nxnative main (about 2900 more steps) are not part of the file. They use the same sites and state page offsets as the variable step, so they need their own state page layout and host support first, and `screen_aspect` does nothing until then.
- The settings are nxnative's values. R19 sets the target to 120 fps through the same frame step stubs, so `fps_target 60` is a record and not a cap. R10 and R17 replace `present_snap`, `enc_thread`, `core_preempt`, `draw_barriers_game` and the `tex_*` keys. The host reader reads no `settings` object yet.
- `Settings` names no engine. An engine with a texture pool or a frame rate variable uses `texture_pool_mb` and `fps_cvar`. nxnative has two games with these values. No Unreal family exists in nxkit yet, so those two games are not taken along.
- The game is the pilot for the nxkit runtime. nxnative runs every service of it.
