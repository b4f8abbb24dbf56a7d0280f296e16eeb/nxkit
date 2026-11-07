# Documentation index

`nxkit` is a Rust monorepo. Its build tool `nxb` changes Switch game packages into Apple builds. It extracts the game package, splits
the shaders, rewrites the code, translates and compiles the shaders into Metal libraries, makes the pipeline archive, and packs and signs the app.
The output files are byte-identical to the output of the reference build scripts, with two exceptions. The MSL of the Rust translator is not byte-identical.
The own shader front end is not byte-identical either. Its raw path is on by default since 0.4.0, and GPU tests cover only a part of the shaders.

## Reading order

| # | document | content |
|---|---|---|
| 1 | [glossary.md](glossary.md) | the terms and the number labels |
| 2 | [architecture.md](architecture.md) | layers, crates, data flow of the stages |
| 3 | [titles.md](titles.md) | the title layer: engine families, title modules, hooks and evidence |
| 4 | [shader-step.md](shader-step.md) | the shader step: versions, execution, store, errors |
| 5 | [pipeline-stages.md](pipeline-stages.md) | extract, split, rewrite, NRO collection, pack, sign, iOS stages, events, cache export, pipeline archive |
| 6 | [frontend.md](frontend.md) | the own MSL to AIR front end and its results |
| 7 | [air-conventions.md](air-conventions.md) | the AIR conventions that a front end must obey |
| 8 | [gpu-differential-testing.md](gpu-differential-testing.md) | the `gpudiff` harness |
| 9 | [translator.md](translator.md) | the Rust shader translator and its contract with the shader step |
| 10 | [early-cutoff.md](early-cutoff.md) | the key scheme of each stage and the cost of a chain change |
| 11 | [benchmarks.md](benchmarks.md) | measured and estimated timings |
| 12 | [decisions.md](decisions.md) | architecture decisions W1 to W23, design decisions and runtime decisions R1 to R50 |
| 13 | [contributing-architecture.md](contributing-architecture.md) | rules for contributors |
| 14 | [research/](research/README.md) | short notes on the studies behind the design |
| 15 | [analysis/oot-open-issues.md](analysis/oot-open-issues.md) | the cause of three defects of one title on Apple GPUs |
| 16 | [plans/runtime-hooks.md](plans/runtime-hooks.md) | the plan for runtime hooks and the nxkit runtime, with owners |

## Data

The documents give results for one reference title on the reference machine. The game files, keys, the shader corpus and
the run outputs are not in this repository. They are not necessary to compile the workspace. To do the parity tests on
your own files, refer to [contributing-architecture.md](contributing-architecture.md).
