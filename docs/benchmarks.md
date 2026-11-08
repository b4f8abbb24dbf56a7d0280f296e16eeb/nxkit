# Benchmarks

This document gives the timings of `nxkit` on the reference title.
Each number has a label: measured, derived or estimate. Read the conditions first.

## Conditions

- Machine: the reference machine, macOS 27.2. The work folders were on the internal disk. The game packages were on an external USB SSD.
- The machine was not quiet. The 1-minute load average before a run was 3 to 25. Other processes used one or more cores. Thus the numbers are slightly pessimistic.
- "Cold" means an empty work folder, store and output. The system file cache was not cleared.
- Each stage ran one time. Extract and split ran 3 times cold and 2 times warm. There is no median of 3.
- `/usr/bin/time -l` measured each command. Average cores = CPU time / wall time.
- `nxb build` does not call `nxb shaders`. The end-to-end run thus contains extract, split, archive and app, without the shader step.

## nxkit (measured, 2026-11-05)

| stage and run | wall | CPU user + system (s) | average cores |
|---|---|---|---|
| extract and split, cold, 3 runs | 15.5 s, 13.4 s, 10.5 s | approximately 49 + 7 to 11 | 3.9 to 5.4 |
| extract and split, warm, 2 runs | 14.7 s, 13.3 s | 48.9 + 11.1 | 4.1 to 4.5 |
| shader step, cold, Apple front end | 650.2 s (10:50) | 4,347.5 + 728.6 | 7.8 |
| shader step, cold, own front end for `own-identical` shaders | 586.4 s (9:46) | 3,862.4 + 667.5 | 7.7 |
| shader step, cold, own front end for `own-identical` and `own-validated` shaders | 416.1 s (6:56) | 2,528.9 + 509.5 | 7.3 |
| shader step, warm store, outputs removed | 113.3 s | 333.1 + 116.7 | 4.0 |
| pipeline archive, cold, `metal-tt` for each batch | 273.0 s | 1,974.2 + 174.3 | 7.9 |
| pipeline archive, cold, `applegpu-nt -j 1` and `air-pack` for each batch, one batch for each core (load 7 to 33) | 207 s | 958 to 1,076 | |
| pipeline archive, no change | 3.2 s | | |
| Mac app, new | 23.0 s | 7.5 + 18.3 | 1.1 |
| Mac app, no change | 4.8 s | 7.7 + 6.0 | 2.9 |
| `nxb build`, cold, without the shader step | 316.6 s (5:17) | 2,025.0 + 190.1 | 7.0 |
| `nxb build`, warm, no change | 14.1 s | 10.3 + 8.6 | 1.3 |

Peak memory: extract and split 3.0 GiB. Shader step 0.17 to 0.94 GiB. One `metal-tt` child used up to 5.9 GiB.

The Apple front end and the own front end for `own-identical` shaders gave the same 74,421 files (0 differences).

## Full build (derived)

The sum of the stages is approximately 16:00 with the Apple front end, 15:00 with the own front end for `own-identical` shaders,
and 12:10 with the own front end for `own-identical` and `own-validated` shaders. These sums use the first archive time (273 s). With the archive time of 207 s, each sum is approximately 1 min shorter (derived). Nobody ran this sum as one process. There is no overlap of stages in this sum.
The overlap of stages in version 0.3.0 can make a real run shorter than the sum. Nobody timed a cold build with overlap, thus the size of the gain is an estimate.
The changes of version 0.4.0 (raw path, Rust translator, host) are not in these sums. The 0.5.1 changes (the fix of the chain value, the rewrite step of the current engine, the new inputs and options of `nxb build`) are not in these sums. Nobody timed a full run of `nxb build` with them.

## Findings

- Extract and split, the Mac app stage and the paths with no change take seconds.
- The shader step is compile-bound. All 12 cores are busy. The CPU average is 7.3 to 7.8 cores, because the other cores wait for Apple's tools.
- The own front end decreases the wall time by 10 % with the same output and by 36 % with validated output.
- With `metal-tt` for each batch, the archive took 273 s. With `applegpu-nt -j 1` and
  `air-pack` for each batch and one batch for each core, it took 207 s. The loader probe found all 18,300 bases.
- Hardware SHA-256: 1,841 MB/s with the ARMv8 path, 225 MB/s scalar (1 GiB, measured with load).

## Version 0.3.0

- Overlap in `nxb build` (measured, one run, other jobs used the CPU, load 13 to 25): 136.2 s for the old binary and 133.0 s for the new binary.
  The run reused extract, split and shaders, thus the host, the app parts and the app stage were the critical path in both runs.
- Raw path (measured, 40 shaders, 105 libraries, `recompile --jobs 4`, load 11 to 18): the median compile CPU time of the 44 raw shaders was 97.5 ms
  against 109 ms for the same shaders without the raw path (about 11 % less). The whole compile phase took 4.5 s against 5.1 s.
  The sample is small and the machine had a high load.
- Estimates, not measured: the gain of the overlap on a cold build. Also 4 to 6 minutes less for a full cold build with the raw path on all shaders.
  The raw allow list covers about 14 % of the corpus, thus the second estimate does not apply to the list as it is.

## Version 0.4.0

All runs are on the reference machine. The machine was not quiet (the load average was 8 to 12 in the shader runs). Other jobs used the CPU.
No run below is a full build. The numbers of the earlier sections do not include the changes of 0.4.0.

| run | before | 0.4.0 | label |
|---|---|---|---|
| rewrite of a 29 MB main module | 63 s | 3.4 s | measured |
| rewrite of a larger main module | 121 s | 8.9 s | measured |
| rewrite of synthetic modules with 100 to 185.6 MB of text | 183 to 672 s | 1 to 3.5 s | measured, synthetic modules |
| NRO collection, one image | 4.5 s | 3.5 s | measured. The archive table dominates both times |
| translation of 802 jobs, host against process bridge | 6.3 to 8.4 s (19 starts) | 1.0 to 1.7 s (1 start) | measured, load average 9 to 10 |
| shader step subset (160 keys, 261 shaders), host against process bridge | 20.7 s | 20.2 s | measured. The Metal compile dominates |
| shader step subset (160 keys), Rust translator against C# process bridge | 16 s | 17 s | measured with 0.4.0, with the Rust translator on. No gain, no .NET process |
| shader step subset (160 keys), raw path on all shaders against Apple's compiler | 14.4 s | 6.5 s | measured, not a benchmark |

Notes:

- The subset runs used a cold store and one run each. They show that the new paths work and what the order of size is. They are not a benchmark.
- The raw path gives different output than Apple's compiler for some shaders. The comparison of the times is thus not a comparison of equal work (refer to [frontend.md](frontend.md)).
- Estimates, not measured: the gain of the raw path on a full cold build (the earlier estimate was 4 to 6 minutes), the gain of the overlap on a cold build,
  and the change of the .NET starts in a full build from 667 to about 1.
- Not measured: a full `nxb build` in one run with the 0.4.0 defaults, and the iOS flow end to end.

## Estimates

| change | expected decrease (estimate) | basis |
|---|---|---|
| own front end at 90 % coverage | shader step 5 min or less | 62 to 72 ms against 140 to 226 ms core time for each input |
| store format 2 with manifests | full-hit path 5 to 10 times faster | 108,320 store files (measured) |
| pipes as an alternative to temporary files | 1 % to 3 % on an internal SSD, 5 % to 10 % on an external disk | 31 % of thread time in open, read and close (measured) |


## Chain changes (measured)

Each stage has its own content key (refer to [early-cutoff.md](early-cutoff.md)). The translator key `nxb.xlate.v2` contains V.
The compile key `nxb.metallib.v2` does not contain V. Thus a change of V translates again, but the same MSL does not compile again.
The table comes from the 0.1.0 release. Measured on the reference title, Mac target, from the store of a full build. The machine load was 21 to 47.

| change | wall time |
|---|---|
| V changes, no other change (0 compiles, 0 translator starts) | 47 s |
| a pass changes 1 % of the pixel and compute shaders (131) | 66 s |
| a pass changes 10 % (1,119) | 119 s |
| a pass changes 100 % (11,411) | 528 s |
| new translator build with the same output | 107 s |
| new compiler identity | approximately a full build |
| full build from an empty store | 687 s |

## Shader step speed-up (measured, 2026-11-06)

Standalone `nxb shaders build` on the reference title, empty store and cache, default jobs. Each output was compared with
the 0.6.4 output by a SHA-256 manifest of all 90,442 files. All outputs are byte-identical.

| change | wall time | CPU user (s) |
|---|---|---|
| 0.6.4 | 327 s | 2,222 |
| `mem2reg` rewrite in `air-bc` | 214 s | 1,360 |
| Fx hasher, structural CSE key, metadata text without `fmt` | 196 s | 1,219 |
| TBAA node cache, hash index for writer constants, work-list DCE, borrowed tokens | 170 s | 1,115 |

Peak memory went from 1.5 GB to 0.6 GB. In 0.6.4, 1,079 of 23,601 libraries came from Apple's `metal -c`.

0.6.5 also closes front end gaps. The same run with 0.6.5 took 879 s of CPU user time and 1.2 GB of peak memory
(measured on 2026-11-06). Apple's `metal -c` made 35 libraries. This run wrote to an external USB SSD, so its wall time
of 214 s does not compare with the table. The outputs differ from 0.6.4 only in the libraries that Apple made before
and in 2 libraries with a fixed call attribute.
The shader step of a second title is in [research/second-title-shader-step.md](research/second-title-shader-step.md).
