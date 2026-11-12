#!/bin/sh
# Quality gates for the nxkit workspace: format, lints, tests, layers, size limits.
# Usage: tools/check.sh            (run from anywhere)
# NXB_ORACLE_EXACT=1 makes the signer tests compare bytes with the host codesign (set it on the Mac where the signer was proven).
# Without it they only require that codesign --verify accepts our output.
# Environment: CARGO_BUILD_JOBS (default 4, from .cargo/config.toml), NXB_TEST_TMP (scratch for store tests).
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

step "rustfmt"
# workspace members only: --all would also check path dependencies outside the workspace (air-front)
cargo fmt --check

step "clippy (deny warnings)"
# needless_range_loop: Rust 1.100 also flags loops whose index names a slot of several arrays (pixels, pads, fingers,
# output slots); an iterator does not make those clearer.
cargo clippy --workspace --all-targets --locked --offline -- -D warnings -A clippy::needless_range_loop

step "tests"
cargo test --workspace --locked --offline --no-fail-fast

step "docs (deny warnings)"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked --offline --quiet

step "layers and limits"
cargo run --quiet --locked --offline -p xtask -- layers limits

printf '\nall checks passed\n'
