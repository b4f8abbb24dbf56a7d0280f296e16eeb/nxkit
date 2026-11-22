# Live debugging of a running game

This page tells a person or an AI agent how to debug a game while it runs. You do not start the game again for each
question. The game stays in the scene that shows the problem. The decisions are W35 to W39 in
[decisions.md](decisions.md). The details of each switch and verb are in [diagnostics.md](diagnostics.md).

## The control socket

Each running game listens on a Unix socket `$TMPDIR/nxkit/<pid>.sock`. `tools/nxkit-ctl` is the client. It talks to
the newest running game. Use `-p PID` for another one.

```bash
tools/nxkit-ctl list
```

```bash
tools/nxkit-ctl help
```

```bash
tools/nxkit-ctl info
```

`list` gives the running games. `help` gives every verb. `info` gives the pid, the game, the title id, the version,
the diagnostics folder and the socket. The answer of a verb goes to stdout. The exit status is 0 when the verb
worked and 1 when it failed. `NXKIT_CONTROL=0` turns the socket off.

## What you can do while the game runs

| Need | Verb |
|---|---|
| See the live keys and their values | `keys` |
| Turn a diagnostic on or off | `set NXKIT_DIAG=hitch,health`, `unset NXKIT_DIAG` |
| Follow textures, programs or guest memory | `set NXKIT_WATCH_TEX=12,34`, `set NXKIT_DIAG_FS=KEY`, `set NXKIT_TRACE_FS=KEY:1,5`, `set NXKIT_DIAG_WATCH=0xADDR:LEN` |
| Read the output of the diagnostics | `log 200`, then `log +CURSOR` for the lines after it |
| Print the per-draw lines now | `sample` |
| Try an experiment of the encoder | `set NXKIT_KNOB=paint`, `set NXKIT_PAINT_FS=KEY`, `set NXKIT_SKIP_PROGRAMS=KEY` |
| Get the picture and every layer of a frame | `capture passes` |
| Get one texture | `texture ID` |
| Get the passes of a frame as text | `passes` |
| Get the performance numbers | `stats` |
| Open a frame in Xcode (Pixel History) | `gputrace 1` |
| Change a shader | `shader dump KEY`, edit the file, `shader load KEY`, `shader reset KEY` |
| Change the behaviour of the encoder | `patch load PATH`, `patch reload`, `patch unload` |
| Move in the game | `press a`, `hold zl`, `release`, `stick l 0 1 500` |
| Record and play the way to a scene | `record start FILE`, `record stop`, `replay FILE` |

A change of a live key takes effect on the next frame. A change gives every rate limit of the diagnostics its budget
again, so the lines print again after a new `set`.

## What needs a new start

- The keys that change what the host builds: for example `NXKIT_PRIVATE`, the pipeline cache keys and `NXKIT_KNOB_FILE`.
  `set` refuses them and says so.
- The categories `frame` and `gateseq` of `NXKIT_DIAG`.
- A change of the Rust code of the host. Make a patch module first if the change fits the hook points. Else build
  the host again and come back to the scene with a recording (see below).

## Workflows

### Find the pass that breaks the picture

1. Go to the scene. Get the layers:

   ```bash
   tools/nxkit-ctl capture passes
   ```

2. Read `manifest.json` in the folder that the verb gives. Each pass has its attachments, draws, programs and blend
   state. A float target has `stats` with `min`, `max`, `nan`, `inf` and `over1`. Overexposure shows as a large
   `max` or many `over1` values. Black shows as a `max` of 0.
3. Look at `pass-<i>-c<slot>.png` from the first pass to the last. The first pass whose picture is wrong is the
   suspect. Its `.raw` file holds the real float values.
4. Look closer at that pass: `set NXKIT_BARPASS=<i>` for its barriers, `set NXKIT_DIAG_FS=<fragment key>` for its draws,
   then `sample` and `log 300`.
5. Test a fix: `set NXKIT_SKIP_PROGRAMS=<key>`, `set NXKIT_KNOB=...`, or `shader load <key>` with a changed shader.

### Find what wrote a pixel

```bash
tools/nxkit-ctl gputrace 1
```

Start the game with `NXKIT_GPU_CAPTURE=1` for this. Open the `.gputrace` file in Xcode and use Pixel History. The render passes have labels with their pass index and
their fragment keys, so a pass maps to the `NXKIT_DIAG` lines and to the capture manifest.

### Find a hitch or a slow frame

```bash
tools/nxkit-ctl stats
```

```bash
tools/nxkit-ctl set NXKIT_DIAG=hitch,passprof
```

```bash
tools/nxkit-ctl log 200
```

### Come back to the scene after a new build

1. Record the way to the scene once:

   ```bash
   tools/nxkit-ctl record start /abs/path/scene.txt
   ```

   Play to the scene, then stop the recording:

   ```bash
   tools/nxkit-ctl record stop
   ```

2. Change the waits after load screens from frames to seconds (`wait 5s`). Load times change from run to run.
3. Start each new build with `NXKIT_INPUT_FILE=/abs/path/scene.txt`. The game plays the way to the scene by itself.

## Limits

- The frame capture and the GPU trace work for the NVN backend (`nxkit-gpu`). A Vulkan game does not have them yet.
- `shader dump` needs the MSL sources in the app. Build the app with `NXB_KEEP_MSL=1`.
- A patch module can crash the game. A panic in a patch stops the game.
- A recording repeats the input, not the game. A scene that depends on chance can be different.
- These tools were tested with unit tests. Report a problem that you find in a real game.
