---
name: nxkit-live-debug
description: Debug a running nxkit game without restarting it, through its control socket (tools/nxkit-ctl). Use when the user reports a rendering, performance or behaviour problem in a game that runs now ("the scene is overexposed", "the sky is black", "this stutters"), or asks to inspect, capture, trace, patch or drive a running game.
---

# Live debugging of a running nxkit game

The game runs with a control socket. You change diagnostics, read the log, capture frames, swap shaders, load patch
modules and drive the input while the game stays in its scene. Read `docs/live-debugging.md` first. It has the verbs,
the workflows and the limits. Every switch and category is in `docs/diagnostics.md`.

## Start

1. `tools/nxkit-ctl list`. No line means no game runs with a socket. The host must be a build of this branch or later.
   Ask the user to start the game and to go to the scene.
2. `tools/nxkit-ctl info` and `tools/nxkit-ctl help`.
3. Note the cursor of `tools/nxkit-ctl log 1`, so that later reads give only the new lines (`log +CURSOR`).

## Loop

1. Make a guess about the cause.
2. Turn on only the diagnostics that test the guess (`set KEY=VALUE`). Use `sample` for the per-draw lines now.
3. Read `log +CURSOR`. Read the files of `capture`, `texture` and `gputrace` with the Read tool. PNG files are
   pictures. `manifest.json` has the numbers.
4. Turn the diagnostics off again (`unset`). Many lines make the game slow.
5. Test a fix in the running game: knobs, the skip list, `shader load`, or a patch module (`tools/nxkit-patch new NAME`).
6. When a test proves the fix, write it into the host, the translator or the title module, with a test. Rules:
   `CONTRIBUTING.md`.

## Rules

- Do not change a start-only key with `set`. It refuses. Tell the user which new start it needs.
- Move in the game (`press`, `stick`) only when the user agrees. Never open the quit prompt or a menu that deletes a save.
- Tell the user what you turned on and what you found, with the paths of the files.
- When the host must change, record the way to the scene first (`record start`), so that the next build comes back
  by itself (`NXKIT_INPUT_FILE`).
