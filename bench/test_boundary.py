# SPDX-License-Identifier: MIT OR Apache-2.0

import json
import math
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import boundary


SAMPLE = """direct_jsc_lower_bound: 100.00 ns/call
host_gate_admit_and_exit: 4.00 ns/entry
jsc_common_empty_entry: 9.00 ns/entry
rustjsi_experimental: 125.00 ns/call
rustjsi_over_direct: 1.250x (1000000 iterations)
direct_jsc_scalar: 25.00 ns/round-trip
rustjsi_common_scalar: 27.00 ns/round-trip
common_scalar_over_direct: 1.080x (1000000 iterations)
"""


class SampleTests(unittest.TestCase):
    def test_all_metrics_and_units(self):
        sample = boundary.parse_sample(SAMPLE)
        self.assertEqual(sample["rustjsi_experimental"], 125)
        self.assertEqual(sample.keys(), boundary.METRICS.keys())

    def test_bad_samples(self):
        cases = [
            "", SAMPLE + SAMPLE, SAMPLE + "unexpected: 12 ns/call\n",
            SAMPLE.replace("100.00 ns/call", "NaN ns/call"),
            SAMPLE.replace("100.00 ns/call", "inf ns/call"),
            SAMPLE.replace("100.00 ns/call", "0 ns/call"),
            SAMPLE.replace("100.00 ns/call", "-1 ns/call"),
            SAMPLE.replace("100.00 ns/call", "100.00 us/call"),
            SAMPLE.replace("1000000 iterations", "10 iterations"),
            SAMPLE.replace("1.250x", "0.000x"),
            SAMPLE.replace("direct_jsc_lower_bound: ", "direct_jsc_lower_bound="),
            "\n".join(SAMPLE.splitlines()[:-1]),
        ]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                boundary.parse_sample(value)

    def test_statistics_use_sample_standard_deviation(self):
        result = boundary.describe([1, 2, 3])
        self.assertEqual(result["median"], 2)
        self.assertEqual(result["sample_cv"], 0.5)
        self.assertEqual(result["samples"], 3)

    def test_statistics_reject_insufficient_or_invalid_values(self):
        for values in ([], [1], [1, 0], [1, -1], [1, math.nan], [1, math.inf]):
            with self.subTest(values=values), self.assertRaises(ValueError):
                boundary.describe(values)

    def test_ratios_are_paired_and_no_call_percentile_is_invented(self):
        samples = [boundary.parse_sample(SAMPLE) for _ in range(10)]
        samples[0]["direct_jsc_lower_bound"] = 50
        report = boundary.summarize(samples)
        ratios = report["paired_ratios"]["call_over_lower_bound"]
        self.assertEqual(ratios["mean"], (2.5 + 9 * 1.25) / 10)
        self.assertFalse(report["all_run_mean_cv_at_most_5_percent"])
        self.assertIsNone(report["individual_call_p99"])
        self.assertFalse(report["performance_gate_qualified"])

    def test_at_least_ten_runs(self):
        with self.assertRaises(ValueError):
            boundary.summarize([boundary.parse_sample(SAMPLE)] * 9)

    def test_identical_runs_have_zero_noise_not_gate_qualification(self):
        report = boundary.summarize([boundary.parse_sample(SAMPLE)] * 10)
        self.assertTrue(report["all_run_mean_cv_at_most_5_percent"])
        self.assertFalse(report["performance_gate_qualified"])


class ArtifactTests(unittest.TestCase):
    def test_cargo_executable_selection(self):
        artifact = {
            "reason": "compiler-artifact", "target": {"name": "boundary", "kind": ["bench"]},
            "executable": "/tmp/boundary",
        }
        output = json.dumps({"reason": "build-finished", "success": True}) + "\n"
        with self.assertRaises(ValueError):
            boundary.executable_from_cargo(output)
        output += json.dumps(artifact)
        self.assertEqual(boundary.executable_from_cargo(output), Path("/tmp/boundary"))
        artifact["executable"] = "/tmp/other"
        with self.assertRaises(ValueError):
            boundary.executable_from_cargo(output + "\n" + json.dumps(artifact))

    def test_outputs_are_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "record.json"
            boundary.write_json(target, {"original": True})
            with self.assertRaises(FileExistsError):
                boundary.write_json(target, {"original": False})
            self.assertEqual(json.loads(target.read_text()), {"original": True})

    def test_report_uses_raw_samples_and_requires_completion(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 10,
                "source": {"head": "test"}, "binary_sha256": "test-digest",
            }
            boundary.write_json(directory / "metadata.json", metadata)
            for index in range(10):
                (directory / f"run-{index:03}.stdout").write_text(SAMPLE)
            with self.assertRaises(FileNotFoundError):
                boundary.read_report(directory)
            completion = {key: metadata[key] for key in ("source", "binary_sha256")}
            boundary.write_json(directory / "complete.json", completion)
            report = boundary.read_report(directory)
            self.assertEqual(report["metrics"]["direct_jsc_lower_bound"]["mean"], 100)
            # A stale summary is not used to reconstruct the report.
            boundary.write_json(directory / "summary.json", {"wrong": True})
            self.assertEqual(boundary.read_report(directory), report)
            boundary.write_json(directory / "failure.json", {"error": "test failure"})
            with self.assertRaisesRegex(ValueError, "collection failed"):
                boundary.read_report(directory)

    def test_nonzero_process_exit_preserves_output(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.CompletedProcess(["test"], 2, "partial", "failed")
            with patch.object(boundary.subprocess, "run", return_value=result):
                with self.assertRaisesRegex(RuntimeError, "exited with 2"):
                    boundary.record_process(["test"], Path(temporary), "run-000")
            self.assertEqual((Path(temporary) / "run-000.stdout").read_text(), "partial")
            self.assertEqual((Path(temporary) / "run-000.stderr").read_text(), "failed")

    def test_no_collection_on_unsupported_platform(self):
        with patch.object(boundary.platform, "system", return_value="Linux"):
            with self.assertRaisesRegex(ValueError, "requires macOS"):
                boundary.collect(Path("not-created"), 10, "1.98.0")

    def test_source_fingerprint_includes_untracked_contents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "untracked.rs"
            source.write_text("first")
            results = [
                subprocess.CompletedProcess([], 0, b"", b""),
                subprocess.CompletedProcess([], 0, b"untracked.rs\0", b""),
            ]
            with (
                patch.object(boundary, "ROOT", root),
                patch.object(boundary.subprocess, "run", side_effect=results * 2),
                patch.object(boundary, "command", side_effect=["head", "untracked.rs"] * 2),
            ):
                before = boundary.source_stamp()
                source.write_text("second")
                after = boundary.source_stamp()
            self.assertTrue(before["untracked_files_present"])
            self.assertNotEqual(before["worktree_sha256"], after["worktree_sha256"])

    def test_collection_success_and_changed_inputs(self):
        for changed in (None, "source", "binary"):
            with self.subTest(changed=changed), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                executable = root / "binary"
                executable.write_bytes(b"original binary")
                directory = root / "results"
                artifact = json.dumps({
                    "reason": "compiler-artifact",
                    "target": {"name": "boundary", "kind": ["bench"]},
                    "executable": str(executable),
                })

                def fake_process(arguments, destination, name):
                    output = artifact if name == "build" else SAMPLE
                    (destination / f"{name}.stdout").write_text(output)
                    (destination / f"{name}.stderr").write_text("")
                    if changed == "binary" and name == "run-009":
                        executable.write_bytes(b"changed binary")
                    return output

                stamp = {"head": "original"}
                final = {"head": "changed"} if changed == "source" else stamp
                with (
                    patch.object(boundary.platform, "system", return_value="Darwin"),
                    patch.object(boundary, "source_stamp", side_effect=[stamp, final]),
                    patch.object(boundary, "command", return_value="test metadata"),
                    patch.object(boundary, "record_process", side_effect=fake_process) as run,
                    patch("builtins.print"),
                ):
                    if changed:
                        with self.assertRaisesRegex(RuntimeError, "changed during collection"):
                            boundary.collect(directory, 10, "test-toolchain")
                        self.assertFalse((directory / "complete.json").exists())
                        self.assertFalse((directory / "summary.json").exists())
                        with self.assertRaisesRegex(ValueError, "collection failed"):
                            boundary.read_report(directory)
                    else:
                        report = boundary.collect(directory, 10, "test-toolchain")
                        self.assertEqual(boundary.read_report(directory), report)
                    self.assertEqual(run.call_count, 11)
                    # An existing collection is never reused, including failed ones.
                    with self.assertRaises(FileExistsError):
                        boundary.collect(directory, 10, "test-toolchain")

    def test_report_rejects_invalid_completion_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 10,
                "source": {"head": "before"}, "binary_sha256": "test-digest",
            }
            boundary.write_json(directory / "metadata.json", metadata)
            boundary.write_json(directory / "complete.json", {
                "source": {"head": "after"}, "binary_sha256": "test-digest",
            })
            with self.assertRaisesRegex(ValueError, "source or binary changed"):
                boundary.read_report(directory)


if __name__ == "__main__":
    unittest.main()
