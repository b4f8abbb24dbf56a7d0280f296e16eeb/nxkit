#!/bin/sh
# Build the device host from source, separately from game packaging.
# Usage: sh tools/ios/build-host.sh [OUTPUT] (paths are relative to the checkout).
# Default: ${CARGO_TARGET_DIR:-target}/release/nxkit-host-ios, beside a release nxb.
# Re-run after host sources, build settings, SDK or toolchain changes. Cargo tracks the Rust
# dependencies. Always compile and link Swift, so no separate freshness cache is needed.
set -eu
cd "$(dirname "$0")/../.."
target=${CARGO_TARGET_DIR:-target}
output=${1:-$target/release/nxkit-host-ios}
mkdir -p "$(dirname "$output")"
work=$(mktemp -d "$(dirname "$output")/.ios-host.XXXXXX")
trap 'rm -rf "$work"' EXIT
sdk=$(xcrun --sdk iphoneos --show-sdk-path)
cargo rustc -p nxkit-host --lib --release --locked --target aarch64-apple-ios \
    --target-dir "$target" --crate-type staticlib -- --print=native-static-libs \
    2>"$work/rustc.log" || { cat "$work/rustc.log" >&2; exit 1; }
cat "$work/rustc.log" >&2
native=$(sed -n 's/.*native-static-libs: *//p' "$work/rustc.log" | head -n 1)
test -n "$native" || { echo 'rustc did not report native static libraries' >&2; exit 1; }
set -- --sdk iphoneos swiftc -target arm64-apple-ios26.0 -sdk "$sdk" -O \
    -parse-as-library -import-objc-header tools/ios/shell/Nxkit.h \
    tools/ios/shell/App.swift tools/ios/shell/Controls.swift \
    "$target/aarch64-apple-ios/release/libnxkit_host.a"
# rustc's flags are words, not shell code. Keep its native dependency order.
set -f
for flag in $native; do set -- "$@" -Xlinker "$flag"; done
xcrun "$@" -framework UIKit -framework AVFAudio -framework QuartzCore -o "$work/nxkit"
# A failed build leaves the previous host intact. Publish only a complete executable.
mv -f "$work/nxkit" "$output"
printf 'Built %s. Set NXB_NXKIT_HOST to this file if it is not beside nxb.\n' "$output"
