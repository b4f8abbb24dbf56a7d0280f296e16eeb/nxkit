# Documentation index

`nxkit` is a Rust monorepo. Its build tool `nxb` changes Switch game packages into Apple builds. It extracts the game package, splits
the shaders, rewrites the code, translates and compiles the shaders into Metal libraries, makes the pipeline archive, and packs and signs the app.
Two parts do not give the same output as Apple's tools. The MSL of the Rust translator and the own shader front end are such parts. Its raw path is on by default since 0.4.0, and GPU tests cover only a part of the shaders.

## Reading order

| # | document | content |
|---|---|---|
| 1 | [glossary.md](glossary.md) | the terms and the number labels |
| 2 | [architecture.md](architecture.md) | layers, crates, data flow of the stages |
| 3 | [titles.md](titles.md) | the title layer: engine families, title modules, hooks and evidence |
| 4 | [shader-step.md](shader-step.md) | the shader step: versions, execution, store, errors |
| 5 | [pipeline-stages.md](pipeline-stages.md) | extract, split, rewrite, bundle, sign, events, cache export, pipeline archive |
| 6 | [frontend.md](frontend.md) | the own MSL to AIR front end and its results |
| 7 | [air-conventions.md](air-conventions.md) | the AIR conventions that a front end must obey |
| 8 | [gpu-differential-testing.md](gpu-differential-testing.md) | the `gpudiff` harness |
| 9 | [translator.md](translator.md) | the Rust shader translator and its contract with the shader step |
| 10 | [early-cutoff.md](early-cutoff.md) | the key scheme of each stage and the cost of a chain change |
| 11 | [benchmarks.md](benchmarks.md) | measured and estimated timings |
| 12 | [decisions.md](decisions.md) | architecture decisions W1 to W23, design decisions and runtime decisions R1 to R55 |
| 13 | [contributing-architecture.md](contributing-architecture.md) | rules for contributors |
| 14 | [research/](research/README.md) | short notes on the studies behind the design |
| 15 | [analysis/oot-open-issues.md](analysis/oot-open-issues.md) | the cause of three defects of one title on Apple GPUs |
| 16 | [plans/runtime-hooks.md](plans/runtime-hooks.md) | the plan for runtime hooks and the nxkit runtime, with owners |
| 17 | [diagnostics.md](diagnostics.md) | the opt-in diagnostics of the GPU path (`NXKIT_DIAG`, knobs) and the render health report |
| 18 | [pacing.md](pacing.md) | the refresh rate of the screen, the ticks per game frame, the display mode switch, timed presents, VRR, the pace profiles and the GPU clock |
| 19 | [nxkit-gpu.md](nxkit-gpu.md) | the frame encoder: pipelines (specialization, background builds, runtime cache, vertex pulling), native replacements, the Neural Engine offload, memory |
| 20 | [plans/ndve-native.md](plans/ndve-native.md), [plans/ndve-impl.md](plans/ndve-impl.md) | the movie network NDVE run natively: what it is, the implementation and the kernel times |
| 21 | [plans/ndve-ane.md](plans/ndve-ane.md) | NDVE on the Neural Engine: the model, decode-time feeding, picture matching |
| 22 | [plans/ndsr-native.md](plans/ndsr-native.md), [plans/ndsr-impl.md](plans/ndsr-impl.md) | the agl upscaler NDSR run natively |
| 23 | [plans/pipelines-at-build-time.md](plans/pipelines-at-build-time.md), [plans/pipeline-prewarm.md](plans/pipeline-prewarm.md) | every render pipeline from the archive: vertex pulling, specialization, the runtime cache |

## Data

The documents give results for one reference title on the reference machine. The game files, keys, the shader corpus and
the run outputs are not in this repository. They are not necessary to compile the workspace. To do the tests with game data on
your own files, refer to [contributing-architecture.md](contributing-architecture.md).
