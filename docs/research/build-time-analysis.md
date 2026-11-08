# Build-time analysis

**Date:** 2026-11-05.

**Question:** Where does the time of a build go in a full build, and what are the lower limits of each stage?

**Method:**

- Read the build logs and the outputs of one full Mac build of the reference title. No stage ran again for this study.
- Measured `metal -c` and its parts on 300 compiles (4 workers, no PCH, direct tool calls).
- Measured each compile step on 30 samples, 5 runs each.
- Calculated the lower limits from these numbers. The labels show measured, derived and estimate.

## Result

| stage | measured | where the time goes | lower limit |
|---|---|---|---|
| shader step | 13:19 of 23:42 | 23,785 compile inputs. `metal -c` with PCH: 53 each second at 12 processes. 5.8 busy cores on average, 22 % system time. The Python orchestrator used one core at 90 %. | Apple path 7.5 min (derived: 23,785 / 53) |
| extract | 2:27 cold | one thread decrypts, verifies and writes. 47 MB/s cold. The base version is read two times. | estimate 20 to 30 s |
| rewrite | 0:19 | Capstone through Python for each instruction | estimate 2 s |
| split | 0:43 | reads 7.5 GB of images again | estimate 5 to 10 s if fused with the extract |
| Mac app | 2:12 | `codesign` hashes 4.7 GB again, the reuse key hashes 1.2 GB again, 58,152 single clones | estimate 2 to 5 s |
| pipeline archive | 4:17 | 19 `metal-tt` runs in series, approximately 13.5 s each (derived) | estimate 40 to 60 s, if batches run in parallel |

Facts from the compile measurements:

- Orchestrator CPU: 32 % file system calls, 28 % regular expressions, 21 % interpreter, 13 % fork (measured).
- Full `metal -c -O1` from MSL: median 336 ms. Back half from unoptimized IR with `-O1`: median 57 ms (measured).
- With PCH, a full compile takes approximately 140 ms (derived). The back half is thus approximately 40 % of a PCH compile.
- Equivalent IR does not always give the same library. Plain `air-opt -O1` is not the pass pipeline of `metal -c -O1` (41,438 against 25,822 bytes on one sample).

## Decision

- Make the orchestration a streaming task graph that keeps all cores busy (shader step).
- Call tools directly without `xcrun`. Pass `SDKROOT`.
- Fuse the split with the extract and read each block one time.
- Calculate one hash for each byte, at the producer. Use the own ad-hoc signer.
- Measure byte identity on the last output library, not on normalized IR.
- Make the own front end the next lever after the orchestration, because the Apple front end sets the floor.
