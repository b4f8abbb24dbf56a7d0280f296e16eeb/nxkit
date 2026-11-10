#!/bin/sh
# Hash of the optimized LLVM IR (AIR) of an MSL file, without file names: equal hashes = the compiler saw the same program
xcrun -sdk macosx metal -w -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -S -emit-llvm -x metal - -o - < "$1" | grep -v -e '^; ModuleID' -e '^source_filename' | shasum | cut -c1-16
