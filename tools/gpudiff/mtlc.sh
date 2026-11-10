#!/bin/sh
# Compiles MSL like the engine (metal -w -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv, then metallib): mtlc.sh <in.metal> <out.metallib>
set -e
tmp="$2.air.$$"
xcrun -sdk macosx metal -w -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -x metal "$1" -o "$tmp"
xcrun -sdk macosx metallib "$tmp" -o "$2"
rm -f "$tmp"
