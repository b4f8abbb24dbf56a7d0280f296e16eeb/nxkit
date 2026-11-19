import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import shader_scan


class CoverageScanTests(unittest.TestCase):
    def setUp(self):
        fixture = tempfile.TemporaryDirectory()
        self.addCleanup(fixture.cleanup)
        self.root = Path(fixture.name)

    def file(self, name, content):
        p = self.root / name
        p.write_text(content, encoding="utf-8")
        return str(p)

    def run_scan(self, *args):
        out = io.StringIO()
        # Detect any subprocess launch, including a translator or compiler.
        with contextlib.redirect_stdout(out), patch(
            "subprocess.Popen", side_effect=AssertionError("must not launch a backend")
        ):
            status = shader_scan.main(["--json", *args])
        data = json.loads(out.getvalue())
        return status, data, {(r["level"], r["key"]): r for r in data["findings"]}

    def test_jobs_census(self):
        jobs = self.file("jobs.json", json.dumps([
            {"stage": "ps"}, {"stage": "ds"}, {"stage": "hs"}, {"stage": "ds"},
            {"stage": "cs", "mesh_stage": {"kind": "ms", "refused": "exceeds the stage memory limit"}},
            {"stage": "ms"}, {"stage": "future"},
        ]))
        code, result, findings = self.run_scan("--jobs", jobs, "--examples", "1")
        self.assertEqual(code, 0)
        self.assertEqual(result["counts"]["jobs"], 7)
        self.assertEqual(result["counts"]["compute_job_with_ms"], 1)
        self.assertEqual(result["stages"]["ds"], 2)
        ds = findings["confirmed", "translator-tessellation-ds"]
        self.assertEqual(ds["count"], 2)
        self.assertEqual(len(ds["examples"]), 1)
        self.assertIn(("confirmed", "translator-mesh-stage-must-use-cs-job"), findings)
        self.assertIn(("confirmed", "translator-unported-stage"), findings)
        self.assertIn(("confirmed", "mesh-stage-marked-refused"), findings)

    def test_real_calls_ignore_comments_and_supported_overlap(self):
        source = self.file("sample.metal", """
// simd_shuffle_xor(v, i)
/* atomic_exchange_explicit(a, b, memory_order_relaxed) */
auto s = "simd_shuffle_up(v, i)";
simd_shuffle(v, i);
simd_shuffle_down(v, i);
simd_shuffle_down(x, y);
threadgroup_barrier(mem_flags :: mem_threadgroup);
threadgroup_barrier(mem_flags::mem_threadgroup | mem_flags::mem_device);
atomic_fetch_add_explicit(a, b, memory_order_relaxed);
""")
        code, result, findings = self.run_scan("--profile", "msl-test", source)
        self.assertEqual(code, 0)
        self.assertEqual(set(findings), {
            ("candidate", "own-compiler-call:simd_shuffle_down"),
            ("candidate", "own-compiler-barrier-flags"),
        })
        self.assertEqual(findings["candidate", "own-compiler-call:simd_shuffle_down"]["count"], 1)
        self.assertTrue(findings["candidate", "own-compiler-call:simd_shuffle_down"]["examples"][0].endswith(":6"))
        self.assertEqual(result["counts"]["unchecked_atomic_call_sites"], 1)

    def test_baseline_selection(self):
        source = self.file("basic.metal", "simd_ballot(p);\n"
                           "threadgroup_barrier(mem_flags::mem_threadgroup);")
        _, report, findings = self.run_scan(source)
        self.assertEqual(report["baseline"], shader_scan.BASELINES["upstream"])
        self.assertIn(("candidate", "own-compiler-call:simd_ballot"), findings)
        self.assertIn(("candidate", "own-compiler-barrier-flags"), findings)
        _, report, findings = self.run_scan("--profile", "msl-test", source)
        self.assertEqual(report["baseline"], shader_scan.BASELINES["msl-test"])
        self.assertEqual(findings, {})

    def test_metallib_unexamined_or_same_name_source(self):
        for name in ("with.metallib", "binary.metallib"):
            (self.root / name).write_bytes(b"MTLB\x00\xff")
        self.file("with.metal", "simdgroup_barrier(mem_flags::mem_threadgroup);")
        code, result, findings = self.run_scan(str(self.root), str(self.root / "with.metallib"))
        self.assertEqual(code, 0)
        self.assertEqual(result["counts"]["metallib_inputs"], 2)
        self.assertEqual(result["counts"]["sources"], 1)
        self.assertEqual(result["counts"]["metallib_with_name_matched_sidecar"], 1)
        self.assertIn(("unexamined", "metallib-without-source-sidecar"), findings)
        self.assertIn(("candidate", "own-compiler-call:simdgroup_barrier"), findings)

    def test_malformed_keeps_other_results(self):
        broken = self.file("broken.json", "not JSON")
        mixed = self.file("mixed.json", '[{"stage":null},{"stage":"hs"}]')
        code, _, findings = self.run_scan("--jobs", broken, "--jobs", mixed, str(self.root / "absent"))
        self.assertEqual(code, 2)
        self.assertIn(("input error", "jobs-read"), findings)
        self.assertIn(("input error", "job-stage-invalid"), findings)
        self.assertIn(("input error", "input-not-found"), findings)
        self.assertIn(("confirmed", "translator-tessellation-hs"), findings)

    def test_optional_real_checker_schema(self):
        verdicts = self.file("verdicts.jsonl", "\n".join(map(json.dumps, [
            {"path": "ok.metal", "ok": True, "reasons": {}, "detail": ""},
            {"path": "reject.metal", "ok": False, "reasons": {"call:example": 3}, "detail": ""},
        ])))
        code, result, findings = self.run_scan("--mslcheck-json", verdicts)
        self.assertEqual(code, 0)
        self.assertEqual(result["counts"]["mslcheck_accepted"], 1)
        self.assertEqual(findings["checker rejection", "call:example"]["count"], 1)
        self.assertTrue(any("not a full compile" in line for line in result["limits"]))


if __name__ == "__main__":
    unittest.main()
