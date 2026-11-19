#!/usr/bin/env python3
"""Read-only nxkit shader coverage triage (stdlib only; no build tools)."""
import argparse
import json
from collections import Counter
from bisect import bisect_left
from pathlib import Path
import re

BASELINES = {
    "upstream": "65642bd909a7f229e8119f34de60bd2224658c74",
    "msl-test": "da3711382007bb9687091bfeb0e6749e6a50c201",
}
PATCHED_CALLS = {"simd_ballot", "atomic_fetch_add_explicit",
                 "atomic_fetch_min_explicit", "atomic_fetch_max_explicit"}
SOURCE_SUFFIX = {".metal", ".msl"}
RECOGNIZED = SOURCE_SUFFIX | {".metallib"}
STAGES = {"vs", "ps", "cs", "gs", "hs", "ds", "ts", "ms"}
# Remove comments and literals while preserving newlines/line numbers. This is
# deliberately not a preprocessor or a type checker.
TRIVIA = re.compile(r'//[^\n]*|/\*[\s\S]*?(?:\*/|\Z)|"(?:\\.|[^"\\])*"|\'(?:\\.|[^\'\\])*\'')
NAMES = re.compile(r"\b(simd_shuffle_(?:up|down|xor)|simdgroup_barrier|simd_ballot|"
                   r"atomic_(?:compare_exchange_weak|exchange|fetch_(?:and|xor))_explicit|"
                   r"threadgroup_barrier|atomic_fetch_(?:add|min|max|or)_explicit)\s*\(")
BARRIER_ARG = re.compile(r"\s*((?:mem_flags\s*::\s*mem_\w+\s*)(?:\|\s*mem_flags\s*::\s*mem_\w+\s*)*)\)")
UNAVAILABLE = {"simd_shuffle_up", "simd_shuffle_down", "simd_shuffle_xor", "simdgroup_barrier",
               "atomic_compare_exchange_weak_explicit", "atomic_exchange_explicit",
               "atomic_fetch_and_explicit", "atomic_fetch_xor_explicit"}


def scan(args):
    counts = Counter()
    stages = Counter()
    issues = {}

    def note(level, key, example):
        row = issues.setdefault((level, key), {"count": 0, "examples": []})
        row["count"] += 1
        if len(row["examples"]) < args.examples:
            row["examples"].append(example)

    def read_input(path):
        return path.read_text(encoding="utf-8")

    for path in args.jobs:
        try:
            jobs = json.loads(read_input(path))
            if not isinstance(jobs, list):
                raise ValueError("expected a JSON array of translator jobs")
        except (OSError, ValueError) as exc:
            note("input error", "jobs-read", f"{path}: {exc}")
            continue
        counts["job_files"] += 1
        for index, job in enumerate(jobs, 1):
            where = f"{path}:job {index}"
            if not isinstance(job, dict) or not isinstance(job.get("stage"), str):
                note("input error", "job-stage-invalid", where)
                continue
            stage = job["stage"]
            counts["jobs"] += 1
            stages[stage if stage in STAGES else "unknown"] += 1
            if stage in ("hs", "ds"):
                note("confirmed", "translator-tessellation-" + stage, where)
            elif stage in ("ts", "ms"):
                note("confirmed", "translator-mesh-stage-must-use-cs-job", where)
            elif stage not in STAGES:
                note("confirmed", "translator-unported-stage", where)
            mesh = job.get("mesh_stage")
            if isinstance(mesh, dict) and stage == "cs":
                kind = mesh.get("kind")
                if kind in ("ts", "ms"):
                    counts["compute_job_with_" + kind] += 1
                if isinstance(mesh.get("refused"), str):
                    note("confirmed", "mesh-stage-marked-refused", where)

    sources = set()
    libraries = set()
    for path in args.inputs:
        if not path.exists():
            note("input error", "input-not-found", str(path))
            continue
        if path.is_dir():
            members = (file for file in path.rglob("*") if file.suffix.lower() in RECOGNIZED)
        else:
            members = (path,)
        for file in members:
            if not file.is_file():
                note("input error", "not-a-file", str(file))
                continue
            suffix = file.suffix.lower()
            if suffix in SOURCE_SUFFIX:
                sources.add(file.resolve())
            elif suffix == ".metallib":
                libraries.add(file.resolve())
            else:
                note("input error", "unknown-input-kind", str(file))

    for path in sorted(libraries):
        counts["metallib_inputs"] += 1
        # Sidecar discovery only; binary is not opened and association is not proven.
        sidecars = [path.with_suffix(s) for s in (".metal", ".msl")]
        found = [p.resolve() for p in sidecars if p.is_file()]
        if found:
            counts["metallib_with_name_matched_sidecar"] += 1
            sources.update(found)
        else:
            note("unexamined", "metallib-without-source-sidecar", str(path))

    for path in sorted(sources):
        try:
            source = read_input(path)
        except (OSError, ValueError) as exc:
            note("input error", "source-read", f"{path}: {exc}")
            continue
        counts["sources"] += 1
        source = TRIVIA.sub(lambda m: re.sub(r"[^\n]", " ", m[0]), source)
        by_key = {}
        lines = None
        for call in NAMES.finditer(source):
            name = call[1]
            if name in UNAVAILABLE or (args.profile == "upstream" and name in PATCHED_CALLS):
                key = "own-compiler-call:" + name
            elif name == "threadgroup_barrier":
                flags = BARRIER_ARG.match(source, call.end())
                if (args.profile == "msl-test" and flags
                        and re.sub(r"\s+", "", flags[1]) == "mem_flags::mem_threadgroup"):
                    continue
                key = "own-compiler-barrier-flags"
            elif name == "simd_ballot":
                continue
            else:
                # The recognized name alone says nothing about pointer address
                # space, signedness, or memory order. Do not flag the supported
                # subset as missing. Use GPU/reference compilation for overloads.
                counts["unchecked_atomic_call_sites"] += 1
                continue
            if key not in by_key:
                if lines is None:
                    lines = [m.start() for m in re.finditer("\n", source)]
                by_key[key] = bisect_left(lines, call.start()) + 1
        for key, line in by_key.items():
            note("candidate", key, f"{path}:{line}")

    for path in args.mslcheck_json:
        try:
            with path.open(encoding="utf-8") as stream:
                for index, raw in enumerate(stream, 1):
                    try:
                        row = json.loads(raw)
                        if not isinstance(row, dict) or not isinstance(row.get("path"), str):
                            raise ValueError("expected an mslcheck JSON object with a path")
                        loc = row["path"]
                        if row.get("error") is not None:
                            note("input error", "mslcheck-read", loc)
                            continue
                        reasons = row.get("reasons")
                        if type(row.get("ok")) is not bool or not isinstance(reasons, dict):
                            raise ValueError("missing verdict or reason map")
                        counts["mslcheck_verdicts"] += 1
                        counts["mslcheck_accepted"] += row["ok"]
                        if not row["ok"]:
                            for reason in sorted(reasons or {"unspecified": 1}):
                                note("checker rejection", reason, loc)
                    except ValueError as exc:
                        note("input error", "mslcheck-json", f"{path}:{index}: {exc}")
        except (OSError, UnicodeError) as exc:
            note("input error", "mslcheck-read", f"{path}: {exc}")

    return {
        "baseline": BASELINES[args.profile], "counts": dict(sorted(counts.items())),
        "stages": dict(sorted(stages.items())),
        "findings": [{"level": level, "key": key, **record}
                     for (level, key), record in sorted(issues.items())],
        "limits": ["No shader translator, compiler, or GPU was run.",
                   "MSL patterns are candidate gaps only; no preprocessing, overload checking, or control-flow analysis.",
                   "A checker acceptance is not a full compile or runtime guarantee.",
                   "Metallib binaries are not read or decompiled; adjacent source is only a filename match.",
                   "No findings does not imply compatibility."],
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--jobs", action="append", type=Path, default=[], metavar="FILE",
                        help="existing nxkit translator job JSON array (repeatable)")
    parser.add_argument("--mslcheck-json", action="append", type=Path, default=[], metavar="FILE",
                        help="optional existing mslcheck --json output (repeatable)")
    parser.add_argument("--profile", choices=BASELINES, default="upstream",
                        help="coverage snapshot to compare against (default: upstream)")
    parser.add_argument("--examples", type=int, default=3, metavar="N")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("inputs", type=Path, nargs="*", metavar="SOURCE_OR_DIR")
    args = parser.parse_args(argv)
    if not (args.jobs or args.inputs or args.mslcheck_json):
        parser.error("give --jobs, --mslcheck-json, or a source/library/directory input")
    if not 0 <= args.examples <= 20:
        parser.error("--examples must be between 0 and 20")
    report = scan(args)
    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(f"nxkit shader triage (baseline {report['baseline'][:8]})")
        for k, n in report["counts"].items():
            print(f"  {k}: {n}")
        if report["stages"]:
            print("Stages:", ", ".join(f"{k}={n}" for k, n in report["stages"].items()))
        if not report["findings"]:
            print("No findings from these checks.")
        for row in report["findings"]:
            print(f'[{row["level"]}] {row["key"]}: {row["count"]}')
            for example in row["examples"]:
                print(f"  {example}")
        for limit in report["limits"]:
            print(limit)
    return 2 if any(row["level"] == "input error" for row in report["findings"]) else 0


if __name__ == "__main__":
    raise SystemExit(main())
