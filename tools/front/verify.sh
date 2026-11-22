#!/bin/sh
# Decision equality on the corpus and on the prototype's mutation runs.
# usage: tools/verify.sh <work dir> [jobs]
# Needs <work dir>/corpus.txt (the paths of parse_results.json, one per line). The script writes it when it is missing.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=$1; jobs=${2:-4}
proto=${MSL_PROTO:?set MSL_PROTO}
target=${CARGO_TARGET_DIR:-target}
bin=$target/release/mslcheck
mkdir -p "$work"
[ -f "$work/corpus.txt" ] || python3 -c "import json,sys; print('\n'.join(r['path'] for r in json.load(open(sys.argv[1]))))" "$proto/parse_results.json" > "$work/corpus.txt"
echo "== corpus against parse_results.json"
"$bin" -j "$jobs" --json --stats -l "$work/corpus.txt" > "$work/rust_full.jsonl"
python3 "$here/compare_results.py" "$proto/parse_results.json" "$work/rust_full.jsonl"
for m in mut300 mut500; do
    echo "== $m"
    [ -f "$work/$m/expected.jsonl" ] || python3 "$here/mutants.py" "$proto/$m.json" "$work/$m"
    ls "$work/$m"/*.metal | sort -t/ -k2 -n > "$work/$m.list"
    [ -f "$work/$m.py.jsonl" ] || J=$jobs python3 "$here/py_check.py" "$work/$m.list" "$work/$m.py.jsonl"
    "$bin" -j "$jobs" --json --stats -l "$work/$m.list" > "$work/$m.rs.jsonl"
    python3 "$here/compare_results.py" "$work/$m.py.jsonl" "$work/$m.rs.jsonl"
    python3 "$here/compare_results.py" "$work/$m/expected.jsonl" "$work/$m.rs.jsonl" --no-stats > /dev/null && echo "verdicts equal to expected.jsonl"
done
echo "== case file against the prototype"
python3 "$here/check_cases.py"
