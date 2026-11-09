#!/bin/sh
# iosone.sh IN.metal WORKDIR: iOS convention, own (front end + air-opt -O1 --discard-value-names on bitcode) vs Apple
# (metal -c with the engine's iOS flags) library bytes. Prints "same|differs|ownfail|applefail IN".
set -u
f=$1; w=$2; mkdir -p "$w"; b=$w/$(basename "$f" .metal)
A=${AIRC:-target/release/airc}
export SDKROOT=$(xcrun -sdk iphoneos --show-sdk-path)
OPT=$(xcrun -sdk iphoneos -find air-opt); LIB=$(xcrun -sdk iphoneos -find metallib)
xcrun -sdk iphoneos metal -c -O1 -mios-version-min=18.0 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -x metal - -o "$b.ref.air" < "$f" 2>/dev/null && "$LIB" "$b.ref.air" -o "$b.ref.lib" || { echo "applefail $f"; exit 0; }
"$A" bc --ios "$f" -o "$b.bc" 2>/dev/null && "$OPT" -O1 --discard-value-names "$b.bc" -o "$b.air" && "$LIB" "$b.air" -o "$b.lib" || { echo "ownfail $f"; exit 0; }
cmp -s "$b.lib" "$b.ref.lib" && echo "same $f" || echo "differs $f"
