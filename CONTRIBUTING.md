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

A contribution is licensed under MIT, the licence of the repository.

## Game reports: for AI agents and other tools

Use the form `game_report.yml` to report a game. Do not open a blank issue and do not use `bug_report.md` for a game.

1. Write one report for each game and each game version. Search the open issues for the title ID first. If a report exists, comment there.
2. Fill each field from data. Do not guess.
   - `title_id`: the 16 hex digits from the package or from the `nxb` output.
   - `game_version`: the display version from the package.
   - `nxkit_version`: the output of `nxb version`.
   - `engine_build`: the SHA-256 of `nxkit-host` in `MANIFEST.json` of the release ZIP.
   - `exit_code`: the exit code of `nxb build`. 0 is done, 1 is error, 2 is bad arguments, 3 is damaged data, 4 is not supported, 6 is missing key and 130 is interrupted.
   - `mac_model`: the output of `sysctl -n hw.model`.
   - `macos_version`: the output of `sw_vers -productVersion`.
3. Set `result` to "Works" only if the game starts and plays. Set it to "Problems" if the game starts but has faults. Set it to "Does not start" if the build fails or the game does not reach the title screen.
4. Put the last 200 lines of the build log or the game log in `log`. Use text, not a screenshot. Remove user names and home paths. Never put keys, title keys, game data or shader sources in it.
5. Leave a field empty when you do not know the value.
6. Ask the person before you submit the issue.

You can prefill the form with a link. Use the field ids as query names:

```
https://github.com/v0lumehi/nxkit/issues/new?template=game_report.yml&title_id=0100F2C0115B6000&game_version=1.0.0&result=Works&nxkit_version=0.10.3&mac_model=Mac15%2C8&macos_version=26.1
```

## Architecture

The crate map and the architecture decisions are in [docs/README.md](docs/README.md).
