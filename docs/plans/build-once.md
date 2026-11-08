# Build once: how nxkit splits a game app so that changes stay small

Status: design, 2026-11-08. Nothing of it is built. Mac only; iOS comes later.

## The problem

A ported game app is made of parts that change for different reasons and at different speeds: the runtime changes
with almost every fix, the translated shaders when the translator changes, the game's own files only with a game
update, and the player's saves and settings all the time. When one key covers all of them, any change to any part
asks for a rebuild of everything, and a rebuild of a big Switch 2 game costs an hour and a half of compiler time.

The engine-based builds (nxnative and launchers on top of it, such as NX2Apple) show the pattern. From NX2Apple's
code (survey of `LNPANDY/NX2Apple` at `v0lumehi/nxbuild-feature-toggle`, launcher v1.3.1):

- A game counts as built with old tools when a `git diff` of the engine tree touches the host, graphics, tool,
  platform or third-party folders, also when the game's app would run unchanged.
- A shader-chain change flags every game and retranslates and recompiles every shader and the pipeline archive.
- Stage keys hash the whole host source, the chain tools, the Xcode and Metal toolchain and the packer's inputs.
- Fixes for one game are patches to the shared engine, so they change the engine for all games.

## How nxkit splits it

Each part has one owner, one reason to change and its own key.

| part | where | owner | changes when | redo cost |
|---|---|---|---|---|
| runtime | `Contents/MacOS/nxkit-host` | nxkit core | the runtime changes | copy, seconds |
| game code | `nxkit/nxgame.dylib` | rewriter + title module | the rewriter or the title's patches change | rewrite + link, seconds |
| shaders | `nxkit/shaders/` | translator (`TRANSLATOR_ID`) | the translator's output changes | minutes |
| pipeline archive | `nxkit/pipelines.mtl4archive` | archive step | the shader libraries or the GPU family change | minutes to an hour |
| game files | `nxkit/romfs/`, `aoc/` | the input (NUP, packages) | a game update | hard links, seconds |
| manifest | `nxkit/game.json` | title module | the title's settings change | seconds |
| player data | outside the app | the player | always | never rebuilt |

Rules that keep the parts apart:

- **Game knowledge is data.** Everything game-specific is in the title module (`titles/games/<title id>/`): patches,
  settings, memory layout, resource patches. The runtime has no game knowledge, so a fix for one game never changes
  another game's app.
- **The translator is versioned on its output.** `TRANSLATOR_ID` changes only when emitted code changes, and such
  changes are collected and released together, never one per commit.
- **Each layer records its key.** `game.json` has `build.layers` with the key of every part it was built from (runtime
  version, rewriter and rules key, translator id, archive key with the GPU family, game file identity). The runtime
  checks at start that it can run the app's layers (module ABI, shader reflection format) and says in plain words what
  is missing instead of running wrongly.
- **Player data never lives in a layer.** Saves, logs, settings and mods are outside the app, so no update can touch
  them.

## The contracts

### Status and update (`nxb`)

- `nxb status <app> --json` compares the app's layer keys with this `nxb` and its bundled runtime and data, and prints
  one event per layer: `fresh`, or `stale` with the reason (`runtime 0.10.0 -> 0.10.1`, `translator msl-2.17 ->
  msl-2.18`) and the expected cost (`seconds`, `minutes`, `hour`). It reads only the app and its work folder and
  runs in well under a second.
- `nxb update <app> [--work-dir D] --progress json` redoes the stale layers in order and nothing else, reusing the
  work folder (extract, unpacked game files, shader store, archive store), and replaces the app atomically.
- `nxb titles --json` lists the title modules, so a launcher knows which games nxkit builds.

### Launch contract (`nxkit-host`)

- A launcher writes `launch.json` into the run folder before start, with `"schema": 1` and nxkit's own names: screen,
  fullscreen, language, render scale, frame rate target, controller mapping, mods overlay, save sync, frame-time
  recording, and `env` for development overrides. Precedence: environment variables (`NXKIT_*`) > `launch.json` >
  `game.json` defaults. Unknown keys and newer schemas are logged once and ignored.
- The runtime knows no launcher's private names. A launcher translates its own settings into `launch.json`.

### Player data

- With a library marker (`.nxkit-library`, or a launcher's own such as `.nx2apple-library`) in the folder that holds
  the app, saves go to `<folder>/save/<title id>` and logs to `<folder>/log/<title id>.log`; otherwise to
  `~/Library/Application Support/nxkit/<title id>/` and `~/Library/Logs/nxkit/`.
- A save found inside an app (older builds) moves out once on the first start.

## For a launcher: NX2Apple

NX2Apple is the first launcher to use these contracts. Its builder is already behind a feature toggle
(`NX2APPLE_NXBUILD=1`, off by default). The switch-over:

1. **Builder.** With the toggle on and a title module for the game, the launcher runs `nxb build <input> --progress
   json --platform mac -o <app> --work-dir <work>` with no engine tree. Games without a title module keep today's
   path.
2. **Recognition.** An app with `Contents/MacOS/nxkit-host` and `Contents/Resources/nxkit/game.json` is a game app,
   besides today's layout; `DMTitleID` stays in Info.plist for the library and artwork.
3. **Staleness.** For nxkit apps the engine diff and chain compare are replaced by `nxb status`; the library shows
   "update available (seconds)" or "shaders are translated again (about 7 min)" and runs `nxb update` on request.
4. **Play.** The launcher writes `launch.json` from its settings instead of setting its engine's environment
   variables for that game.
5. **Distribution.** The release carries `nxb` and `nxkit-host` from one nxkit release, pinned as today in
   `frontend/nxbuild.pin`. Nothing is downloaded at run time.

Phases: (0) the toggle exists and is off; (1) builds without the engine behind the toggle, for testers; (2) status
and update; (3) per-game release once a game's nxkit build passes its checks; (4) the engine path leaves the launcher
when every supported game has a title module and ran on nxkit for a release.

## Open points

- `nxb` has no `titles --json`, `status` or `update` yet, and `game.json` has no layer keys (in progress).
- `nxkit-host` does not read a library marker or `launch.json` yet (in progress).
- The launcher's launch-options UI needs a schema for each game's options; `game.json` can carry it.
- Pipeline archive coverage: the build leaves out vertex shaders with stage inputs, primitive-ID fragments, layered
  vertex shaders and mesh shaders, and uses default render states (`docs/pipeline-stages.md`). Until it archives the
  real pipelines, the first play of an area compiles pipelines at run time.
- The first start of a freshly built app waits until macOS has assessed the whole signed bundle. With the game files
  inside the bundle (Ocarina of Time: 186,587 files, 25 GB as hard links, plus a 4.8 GB pipeline archive) that takes
  minutes, and it repeats after every update that replaces the bundle. The game files and probably the archive belong
  next to the app, like the player data, so the bundle holds only what changes with nxkit: the runtime, the game code,
  the shaders and the manifest.
- iOS is out of scope for now.
