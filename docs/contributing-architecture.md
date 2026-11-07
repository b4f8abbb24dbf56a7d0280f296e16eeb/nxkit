# Rules for contributors

This document gives the architecture rules for a change to the workspace. Read [architecture.md](architecture.md) first.

## Output rules

1. A port gives byte-identical output. Compare the last output files (`.metallib`, `.json`, `.metal`, tables, images, bundles),
   not `.air` files. Show the parity result in the pull request.
2. GPU tests validate a front-end change: `gpudiff` for each family, coverage, and scene comparisons for a release.
   Only shaders with the verdict `own-identical` go on the allow list.
3. Do not put a port, a speed change and an output change into one commit. V is a hash of the engine files that `dm/gfx/tools/dmsh_chain.py` lists in `FILES`, plus the options string. No Rust source goes into V.
   A change of the translator in `nxb-xlate` translates again only when you increase `nxb_xlate::msl::TRANSLATOR_ID` by hand. Without the increase the store gives the old MSL.
4. The reference build scripts are only the reference. Do not change them, and do not copy code from them.
5. Do not change the fixed strings (refer to [shader-step.md](shader-step.md)). Add new texts. Keep old texts.

## Layers and ownership

- A crate uses only crates of a lower layer, plus the sideways edges in [architecture.md](architecture.md). A new edge must have a decision in [decisions.md](decisions.md) first.
- L1 crates do no I/O. Only `nxb-sys` and `nxkit-rt` have `unsafe` code.
- A stage module has a maximum of 400 lines. A file has a maximum of 600 lines.
- Each stage crate has one owner. A change touches only the files of its own concern.
- `cargo run -p xtask -- layers` and `cargo run -p xtask -- limits` examine these rules.

## Errors, determinism and safety

- Do not use `unwrap`, `expect` or `panic!` outside tests. The inputs are game data, and you must not trust them.
- A shader failure is a typed error with a reason string. It does not stop the other shaders. A front-end reject is a fallback.
- A build-wide problem is a `BuildError`. A machine problem (full disk, too many open files, missing toolchain) is an `EnvError`: abort, with no stamp and no reuse key.
- Do not make output from the completion order or from the order of a `HashMap`. Use `BTreeMap` or sort.
- Start child processes only with `nxb_tool::Invocation`.
- Write to the store only through `nxb-store`. Take digests from manifests. Do not read payloads again to hash them.
- Do not put keys or game data into logs, events, errors or fixtures.

## Procedure to add a stage

1. If the stage must have a new crate or a new edge, write a short decision in [decisions.md](decisions.md).
2. Put the pure logic into an L1 crate. Give it no I/O.
3. Put the code that connects the stage into one module of `nxb-stages`, or into a new L3 stage crate (W19).
4. Set `[package.metadata.nxb] layer` and `in_v` in the `Cargo.toml` of each new crate.
5. Write events through `nxb-obs` with a step name and a unit.
6. Add a store object with a versioned `ContentKey`.
7. Add a parity test against the reference build scripts. Read the inputs from environment variables.
8. Do `tools/check.sh`.

Other additions follow the same pattern. A new container is one module and one registry line in the split. A new operating system is one implementation in `nxb-sys`.

## Quality gates

Do `tools/check.sh` before a commit. It runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, all tests,
`rustdoc` with warnings denied, `xtask layers` and `xtask limits`. CI runs the same script on macOS.
`tools/no-game-data.sh` examines the tree for game data. `CARGO_BUILD_JOBS` limits the compiler processes.
`CARGO_TARGET_DIR` moves the build output.

The workspace has no external crates at this time. A new crate is its own commit with an entry in `deps.toml`.
A speed claim must have a benchmark before and after the change: the median of 3 runs on the reference machine.

## Tests

- Put unit tests next to the code. Property tests use `nxb_testkit::{Rng, check}` with a seed. A failure shows the seed.
- Store tests write below `NXB_TEST_TMP` or the system temporary folder, not into the repository.
- Fixtures are synthetic.

## Game data

Do not commit data from a game: shader sources, MSL, SPIR-V, IR, libraries, NCA or NSP files, RomFS parts, keys, corpus lists.
Tests that use game data have `#[ignore]`, or they stop at the start with the message `skipped`. They read their inputs
from environment variables and do not use a default path.

## Parity tests with local game files

These tests compare the output with the work folder of a build with the reference build scripts. Set the variables to your own files.

| test | variables | command |
|---|---|---|
| extract and split | `NXB_PARITY_BASE` (base NSP), `NXB_PARITY_UPDATE` (update NSP), `NXB_PARITY_KEYS` (`prod.keys`), `NXB_PARITY_GAME` (profile `game.tsv`), `NXB_PARITY_REF` (reference work folder), `NXB_TEST_TMP` (scratch folder) | `cargo test -p nxb-extract --release --test parity_real_data -- --ignored` |
| JSON writer | `NXB_BDIR` (a `b-<V>` folder) | `cargo test -p nxb-pyjson --test real_files -- --ignored` |
| shader stages | `NXB_FRONT_WORK`, `NXB_PKG`, `NXB_PROFILE` | the tests in `crates/nxb-stages/tests` |
| own front end and `gpudiff` | `NXB_AIRC`, `NXB_PROTO`, `NXB_REF`, `NXB_CORPUS`, `NXB_SPECS`, `NXB_CAS` | refer to [frontend.md](frontend.md) and [gpu-differential-testing.md](gpu-differential-testing.md) |

The header of each test file gives its variables. Keep keys, game files and all data from them on your machine.

## Pull requests

Open an issue first for a large change. Keep one concern in each change. A fix comes with a test, or the description
tells why no test is possible. Write English without semicolons and without em-dashes.
