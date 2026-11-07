# Contributing

This file is for people who change the code. Read it before you open a pull request.

## Rules

1. Keep each change to one concern.
2. Run `tools/check.sh` before you commit. It must pass.
3. Add a test with each fix. If no test can exist, write the reason in the pull request.
4. Do not commit game data, keys, shader sources or build outputs. `tools/no-game-data.sh` checks this.
5. A new dependency needs an entry in `deps.toml`. Today the workspace uses the standard library only.
6. Write documentation in Simplified Technical English. Use short sentences and the active voice. Do not use semicolons.
7. Do not copy code from other projects. Write from the public format description. See [THIRD_PARTY.md](THIRD_PARTY.md).

## Code rules

- No `unwrap`, `expect` or `panic!` outside tests. Inputs are untrusted files.
- Only `nxb-sys` and `nxkit-rt` can contain `unsafe` code.
- Output must not depend on the order of completion or on the order of a `HashMap`.
- Start child processes with `nxb_tool::Invocation` only.
- A change that alters output bytes must prove byte identity with the reference build. Show the comparison in the pull request.
- Keep dependencies between layers legal. `cargo run -p xtask -- layers` checks them.

## Tests that need game files

These tests are `#[ignore]` or skip themselves. They read their inputs from `NXB_PARITY_*` variables.
They never use a default path. See the README for an example.

## Pull requests

1. Describe what the change does and why.
2. State which tests you ran.
3. If the change alters output, show the byte comparison.
4. Use the pull request template.

A contribution is licensed under GPL-3.0-only, the licence of the repository.

## Architecture

The crate map and the architecture decisions are in [docs/README.md](docs/README.md).
