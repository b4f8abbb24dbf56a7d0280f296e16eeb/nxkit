#!/bin/sh
# pair.sh SRC.metal: Apple raw IRGen and own IR, both through air-opt -O1, normalised with irlib, unified diff.
f=$1; d=$(mktemp -d)
SDK=$(xcrun -sdk macosx --show-sdk-path); T=$(dirname "$(xcrun -sdk macosx -find metal)")
env -i SDKROOT="$SDK" "$T/metal" -c -O1 -fmetal-math-mode=relaxed -fpreserve-invariance -fwrapv -Xclang -disable-llvm-passes -S -emit-llvm -x metal - -o "$d/a.ll" < "$f" 2>/dev/null
"${NXB_AIRC:-target/release/airc}" emit "$f" -o "$d/b.ll" || exit 2
for x in a b; do env -i SDKROOT="$SDK" "$T/air-opt" -O1 -S "$d/$x.ll" -o "$d/$x.o.ll"; done
cd "${NXB_REF:?set NXB_REF}" && python3 -c "
import sys, difflib; sys.path.insert(0, '.')
import irlib
a = irlib.normalize(irlib.load_ir('$d/a.o.ll')).splitlines(); b = irlib.normalize(irlib.load_ir('$d/b.o.ll')).splitlines()
out = list(difflib.unified_diff(a, b, 'apple', 'own', lineterm='', n=1))
print('\n'.join(l[:int(__import__('os').environ.get('W','220'))] for l in out) if out else 'IDENTICAL')"
