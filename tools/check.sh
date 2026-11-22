#!/bin/sh
# Quality gates for the nxkit workspace: format, lints, tests, docs, layers, size limits, module cycles, the settings
# source and the statics of the runtime crates.
# Usage: tools/check.sh [STEP...]   (run from anywhere)
# Steps: fmt clippy test doc xtask. Without a step all steps run in this order. CI runs the steps in parallel jobs.
# NXB_ORACLE_EXACT=1 makes the signer tests compare bytes with the host codesign (set it on the Mac where the signer was proven).
# Without it they only require that codesign --verify accepts our output.
# Environment: CARGO_BUILD_JOBS, NXB_TEST_TMP (scratch for store tests).
set -eu
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$*"; }

run_fmt() {
  step "rustfmt"
  # workspace members only: --all would also check path dependencies outside the workspace (air-front)
  cargo fmt --check
}

run_clippy() {
  step "clippy (deny warnings)"
  # needless_range_loop: Rust 1.100 also flags loops whose index names a slot of several arrays (pixels, pads, fingers,
  # output slots); an iterator does not make those clearer.
  cargo clippy --workspace --all-targets --locked --offline -- -D warnings -A clippy::needless_range_loop
}

run_test() {
  step "tests"
  cargo test --workspace --locked --offline --no-fail-fast
}

run_doc() {
  step "docs (deny warnings)"
  RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked --offline --quiet
}

run_xtask() {
  step "layers, limits, module cycles, settings and statics"
  cargo run --quiet --locked --offline -p xtask -- layers limits modules settings statics
}

[ "$#" -eq 0 ] && set -- fmt clippy test doc xtask
for s in "$@"; do
  case "$s" in
    fmt|clippy|test|doc|xtask) ;;
    *) printf 'check.sh: unknown step %s (steps: fmt clippy test doc xtask)\n' "$s" >&2; exit 2 ;;
  esac
done
for s in "$@"; do "run_$s"; done

printf '\nchecks passed: %s\n' "$*"
