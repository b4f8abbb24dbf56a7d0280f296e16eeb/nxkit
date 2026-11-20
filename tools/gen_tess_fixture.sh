#!/bin/sh
# Generate the synthetic inputs for the opt-in Metal tessellation replay test.
set -eu

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
    printf 'Usage: %s OUTPUT_DIR\n' "$0" >&2
    exit 2
fi
case "$1" in
    /*) out=$1 ;;
    *) out=$PWD/$1 ;;
esac
cd "$(dirname "$0")/.."
cargo run --locked -p nxb-xlate --example generate_tess_fixture -- "$out"
for key in 0101010101010101_b 0101010101010101_bt 0202020202020202_b 0303030303030303_b; do
    cargo run --locked -p airc -- raw "$out/shaders/$key.metal" -o "$out/shaders/$key.metallib"
done
