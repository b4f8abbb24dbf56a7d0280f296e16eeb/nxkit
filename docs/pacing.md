# Pacing and the refresh rate of the screen

This page says how the host turns the refresh rate of the screen into the ticks of the display link, and how a game frame stays on screen for a whole number of them. The code is in `nxkit-svc-pacing/src/refresh.rs` (the plan, pure), `nxkit-metal/src/display.rs` (CoreGraphics modes), `nxkit-host/src/refresh.rs` (the host side) and `nxkit-host/src/sys/appkit.rs` (the screen and its notifications). The diagnostics are in `docs/diagnostics.md`.

## What was wrong

- The window used `NSScreen.maximumFramesPerSecond` of the screen that `NXKIT_SCREEN` names, read once at the start. That is the highest rate the panel can take, not the rate of the current mode. It did not follow the window to another screen, and it did not change when the mode changed.
- The link was asked for 60 or 120 Hz only. The pacing service had the display rate fixed at 120 in `seams.rs` and `frames.rs`, so the ticks per vsync did not follow the screen.
- The `hz 60 ... at120 false` in the `NXKIT_PACE_DIAG` line is the unit of the frame step (a target of 60 fps), not the rate of the screen. It stays 60 for a game at 60 fps on any screen.

## The refresh rate

`refresh::apply` runs at the start, on `windowDidChangeScreen:` and on `applicationDidChangeScreenParameters:` (a mode change, a screen plugged or moved). It reads the screen the window is on now (`NSWindow.screen`), and before the window is on a screen, the screen of `NXKIT_SCREEN`, else the main one. The refresh rate is, in this order:

1. The rate of the current CoreGraphics mode (`CGDisplayCopyDisplayMode`), found by the display id of the screen (`NSScreenNumber`).
2. `maximumFramesPerSecond`, when the mode reports 0 (a screen that adapts its rate).

A screen is adaptive when its longest refresh interval is above its shortest (`NSScreen.minimumRefreshInterval`, `maximumRefreshInterval`). The host also measures the real tick rate from the target times of the ticks (`RateTracker`, median of the last 60 gaps). When it is more than 4% off the plan, the pacing service gets the measured rate, and the host prints a line.

## The plan

`plan_ticks(refresh, target_fps, free_rate, adaptive)` gives the link rate and the ticks per game frame:

| screen | plan |
|---|---|
| adaptive (ProMotion) | The link asks for the target (60), one tick per frame. The screen follows. Timed presents are on. |
| the refresh is an even multiple of the target (120 for 60, within 0.5%) | The link ticks at the refresh rate, `refresh / target` ticks per frame (2 at 120 Hz). |
| no even split, and the game accepts any frame rate (`fps_free`) | The nearest even split: 144 Hz gives 72 fps, 2 ticks (a tie goes to the faster rate). |
| no even split, fixed rate (Breath of the Wild) | The link asks for the target, the plan says `(uneven)` and names a refresh rate to switch to. At 144 Hz a 60 fps game alternates 2 and 3 refreshes. |

The pacing service gets the link rate and the vsync rate of the frame step (60), so `ticks_per_vsync` is `link / 60`, and a hold of N vsyncs is `N * ticks_per_vsync` ticks of the scheduler (`FrameSource::Hold`). The scheduler counts whole ticks, so a hold is exact. The hold follows the screen: a new rate changes the source of the pacer at the next tick.

`fps_free` is for a game whose step follows the vsync event and not a fixed 1/60 s. The frame step of the load-driven pacing still counts in vsyncs of 1/60 s, so keep `fps_free` off for a game that steps by the frame step.

## Switching the display mode

`NXKIT_SET_REFRESH=120` (or `set_refresh` in `game.json`) switches the display to that rate at the start. It looks for the mode with the same size and pixels, usable by the desktop and within 1 Hz of the rate. The change is made with `CGBeginDisplayConfiguration`, `CGConfigureDisplayWithDisplayMode` and `CGCompleteDisplayConfiguration` with `kCGConfigureForAppOnly`. That is `CGDisplaySetDisplayMode` with the scope of the application: the system puts the old mode back when the process ends, also after a crash or a kill. The host also restores it on `applicationWillTerminate:` and when the loop returns. An adaptive screen is not switched. A request that finds no mode prints why and leaves the rate.

When the plan is uneven and no switch was asked, the screen line names an even rate that the display offers.

## Timed presents

The display link of a ProMotion screen can run at any rate between its limits, and the system chooses the rate from the link's `preferredFrameRateRange`. The host pins the range to the target (60 Hz) and also presents each drawable with `presentAfterMinimumDuration:` of one frame period (1/60 s). The display then keeps the frame before it on screen at least that long, so each frame stays exactly its period whatever rate the panel runs at at that moment. Both are cheap and do not conflict: the range tells the system which rate to run at, and the minimum duration guards a frame that the display would otherwise replace early.

Metal may refuse `presentAfterMinimumDuration:` on a drawable of `CAMetalDisplayLink` (it does so for `presentAtTime:`). The call is wrapped in an exception catch. On the first refusal the host prints one line, turns timed presents off for the run and presents plainly. `NXKIT_TIMED_PRESENT=0` or `1` overrides the default (on for adaptive screens only).

## Variable refresh (VRR)

On a screen that adapts its refresh rate (ProMotion, or an external Adaptive-Sync display that macOS reports with a longest refresh interval above the shortest) and a game that steps by the measured frame time (`StepMode::Variable`, a target above 60 with a state page), the host does not hold a frame for a tick of a fixed grid. On the grid a frame of 13 ms showed after 16.7 ms. The choice is `refresh::choose_presentation`; the Load (hold-based) step, a screen that does not adapt and `NXKIT_VRR=0` keep the grid, unchanged.

- **The link has a range.** `preferredFrameRateRange` is minimum = the floor the game needs (60, `NXKIT_VRR_MIN` changes it), maximum = the highest rate of the screen (`maximumFramesPerSecond`, 120 or 144) and preferred = the target of the step, cut to the maximum. `RateRange::plan` makes it. The screen line says `VRR on` with the range.
- **A frame shows when it is ready.** The `Pacer` is in its immediate mode (`Pacer::set_immediate`). A tick that finds no frame keeps its drawable (parked) instead of letting it go. The frame that the game presents next is drawn into the parked drawable the moment it is queued, on the encoder thread, and the drawable is presented at once (`Frame::present`, no target time). A frame that is ready before a tick takes the drawable of that tick. The hold of the frame step is not used.
- **At most one frame is queued.** A newer frame replaces one that waited for a tick (`Pacer::superseded` counts them), so latency stays one frame. The game thread is still held back by the backend when three presents are on their way (`MAX_PENDING`).
- **The step is measured at the presentation.** The display's `presentedTime` of each frame goes to the pacing service (`frame_shown`, through `nxkit_nvn::screen::set_shown_hook`), converted to the clock of the frame starts. The step of the next frame is the time between the last two shown frames (`VariableStep::note_shown`), with the same floor and carry. Game time follows what the screen shows. Before the first shown frame, and on the grid, the step is the time between frame starts as before.
- **Timed presents are off** under VRR, and the measured tick rate is not compared with the plan (`note_measured`): the tick rate follows the frames.

`NXKIT_PACE_DIAG=1` prints the mean and the spread of the interval between shown frames every 120 presents, see `docs/diagnostics.md`.

## Profiles of the frame step

The load-driven step holds a frame for a number of vsyncs of 1/60 s from the cost of the frames. The key `pace_profile` of `game.json` (the hook `RuntimeHooks::pace_profile` of the title) picks how:

| profile | rule |
|---|---|
| `hold` (default) | A late frame may stay longer than the cap's vsyncs (`fps_hold`). The thermal cap and the CPU watch work. A frame is late when it was shown longer than planned. |
| `work` | A frame is late when its cost does not fit the hold, not when it was shown longer; the verdict of the screen is ignored too. A frame over three vsyncs past the hold is a hitch that is paid back and not counted. On a ProMotion screen the `hold` profile stepped one title from 30 to 20 fps at 0.71 vsyncs per frame (measured). |
| `steady` | The step never leaves the cap's frame rate: no hold above the cap and no thermal cap (a movie still caps it). A frame that runs long shows late once and the game pays the time back. Lost time is dropped and not paid back: with no longer hold the debt never cleared and frames showed two vsyncs. One title fell to 30 fps for short hitches under `hold`, but under `steady` its presents fell behind and the game waited up to 400 ms for a window texture, so that title uses `work` (measured). |

## The variable step only under VRR

The variable step (`StepMode::Variable`: the time between the last two shown frames) runs only when the screen is adaptive and the link has a range (see "Variable refresh (VRR)"). On a fixed screen the game keeps the load-driven step at 60. One title ran at about 10 fps on a fixed 120 Hz screen with the variable step (measured). `NXKIT_VRR=0` keeps the fixed grid on an adaptive screen.

## GPU time and the clock of the GPU (DVFS)

The GPU time per frame in `NXKIT_PACE_DIAG` is not a constant of the work. Assumed, not measured here: an Apple GPU lowers its clock when its load is low, so a lighter frame or a capped frame rate can show a higher GPU time in ms for the same work. A speed-up of a kernel can thus raise the GPU ms of a paced frame. Compare GPU times at equal pacing (the same cap, the same screen mode), or use `NXKIT_DIAG=nativeprof`, which times the steps of a native plan inside one pass.

## Open points

- Variable refresh has not run on a screen. The tests cover the choice, the range, the scheduler, the step and the interval statistics. Check on the ProMotion screen: the screen line says `VRR on`, the `present interval` line of `NXKIT_PACE_DIAG=1` follows the frame time (13 ms frames show at about 13 ms) and the panel rate follows. Whether macOS keeps the link at its maximum and the panel follows the presents, whether a parked drawable some milliseconds old presents without complaint, and how the game's vsync event behaves while the tick rate moves are open.

- None of this has been run on a screen. The tests cover the plan, the tracker, the mode choice and the histogram. Check the screen line and the `judder` line on the 120 Hz monitor, on the 144 Hz setting and on the built-in display.
- If the window is in full screen (`--fullscreen`), macOS can give it another mode or display than the window had. The notification replans, but this was not tried.
- A screen whose mode is a non-integer multiple (for example 100 Hz) with a fixed-rate game stays uneven until the user switches the mode.
