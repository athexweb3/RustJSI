# SPDX-License-Identifier: MIT OR Apache-2.0

import contextlib
import io
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import boundary


BASE_SAMPLE = """direct_jsc_lower_bound: 100.00 ns/call
direct_jsc_prepared_call: 110.00 ns/call
host_gate_admit_and_exit: 4.00 ns/entry
jsc_common_empty_entry: 9.00 ns/entry
jsc_foreign_common_empty_entry: 11.00 ns/entry
rustjsi_experimental: 125.00 ns/call
rustjsi_over_direct: 1.250x (1000000 iterations)
rustjsi_over_prepared: 1.136x (1000000 iterations)
direct_jsc_js_call: 90.00 ns/call
rustjsi_common_js_call: 99.00 ns/call
common_js_call_over_direct: 1.100x (1000000 iterations)
direct_jsc_scalar: 25.00 ns/round-trip
rustjsi_common_scalar: 27.00 ns/round-trip
common_scalar_over_direct: 1.080x (1000000 iterations)
"""
ENTRY_VALUES = {
    "host_gate_admit_and_exit": 4.0,
    "jsc_common_empty_entry": 9.0,
    "jsc_foreign_common_empty_entry": 11.0,
}
CALLBACK_VALUES = {
    "direct_jsc_lower_bound": 100.0,
    "direct_jsc_prepared_call": 110.0,
    "rustjsi_experimental": 125.0,
}
CALLBACK_TIMINGS = "".join(
    f"callback_batches_{name}: 1000 ops/batch "
    + ",".join([f"{value:.4f}"] * 1000)
    + " ns/call\n"
    for name, value in CALLBACK_VALUES.items()
)
JS_CALL_VALUES = {
    "direct_jsc_js_call": 90.0,
    "rustjsi_common_js_call": 99.0,
}
JS_CALL_TIMINGS = "".join(
    f"js_call_batches_{name}: 1000 ops/batch "
    + ",".join([f"{value:.4f}"] * 1000)
    + " ns/call\n"
    for name, value in JS_CALL_VALUES.items()
)
TIMING_METRICS = BASE_SAMPLE + CALLBACK_TIMINGS + JS_CALL_TIMINGS + "".join(
    f"entry_batches_{name}: 1000 ops/batch "
    + ",".join([f"{value:.4f}"] * 1000)
    + " ns/entry\n"
    for name, value in ENTRY_VALUES.items()
)
CALIBRATION_SAMPLE = (
    "calibration_timer_pair: 1000 samples "
    + ",".join(["20.0000"] * 1000)
    + " ns/pair\n"
    + "calibration_empty_batch: 1000 ops/batch "
    + ",".join(["1.0000"] * 1000)
    + " ns/operation\n"
)
ALLOCATION_SAMPLE = "".join(
    f"rust_alloc_{name}: 0 calls 0 bytes 0 deallocations "
    + "0 deallocated-bytes (1000000 iterations)\n"
    for name in boundary.ALLOCATION_METRICS
)


def timing_sample(
    callback_order=boundary.CALLBACK_ORDERS[0],
    js_call_order=boundary.JS_CALL_ORDERS[0],
):
    return (
        f"callback_order: {callback_order}\n"
        f"js_call_order: {js_call_order}\n"
        + TIMING_METRICS
        + CALIBRATION_SAMPLE
    )


def balanced_samples():
    return [
        boundary.parse_sample(timing_sample(callback_order, js_call_order) + ALLOCATION_SAMPLE)
        for callback_order, js_call_order in zip(
            boundary.CALLBACK_SCHEDULE, boundary.JS_CALL_SCHEDULE, strict=True
        )
    ]


TIMING_SAMPLE = timing_sample()
SAMPLE = TIMING_SAMPLE + ALLOCATION_SAMPLE


class SampleTests(unittest.TestCase):
    def test_all_metrics_and_units(self):
        sample = boundary.parse_sample(SAMPLE)
        self.assertEqual(sample["metrics"]["rustjsi_experimental"], 125)
        self.assertEqual(sample["metrics"].keys(), boundary.METRICS.keys())
        self.assertEqual(sample["entry_batches"].keys(), boundary.ENTRY_METRICS)
        self.assertEqual(
            sample["callback_batches"].keys(), boundary.CALLBACK_BATCH_METRICS
        )
        self.assertEqual(
            sample["js_call_batches"].keys(), boundary.JS_CALL_BATCH_METRICS
        )
        self.assertEqual(
            sample["calibration"].keys(),
            {"calibration_timer_pair", "calibration_empty_batch"},
        )
        self.assertEqual(len(sample["calibration"]["calibration_timer_pair"]), 1000)
        self.assertEqual(len(sample["entry_batches"]["jsc_common_empty_entry"]), 1000)
        self.assertEqual(
            sample["rust_allocations"]["jsc_common_empty_entry"]["allocations"], 0
        )
        self.assertEqual(
            sample["rust_allocations"]["rustjsi_experimental"]["allocations"], 0
        )
        self.assertEqual(
            sample["rust_allocations"]["rustjsi_common_js_call"]["allocations"], 0
        )
        self.assertEqual(sample["callback_order"], boundary.CALLBACK_ORDERS[0])
        self.assertEqual(sample["js_call_order"], boundary.JS_CALL_ORDERS[0])

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
            SAMPLE.replace("reused,prepared,rustjsi", "reused,reused,rustjsi"),
            SAMPLE.replace("direct,common", "direct,direct"),
            SAMPLE.replace("4.0000", "NaN", 1),
            SAMPLE.replace("4.0000,", "", 1),
            SAMPLE.replace("4.00 ns/entry", "5.00 ns/entry", 1),
            SAMPLE.replace("100.0000", "120.0000", 1),
            SAMPLE.replace("90.0000", "120.0000", 1),
            SAMPLE.replace("20.0000", "NaN", 1),
            SAMPLE.replace("calibration_timer_pair: 1000", "calibration_timer_pair: 10"),
            SAMPLE.replace("1.0000,", "", 1),
            SAMPLE.replace("0 calls 0 bytes", "-1 calls 0 bytes", 1),
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

    def test_nonnegative_statistics_accept_zero(self):
        result = boundary.describe_nonnegative([0, 0, 0])
        self.assertEqual(result["mean"], 0)
        self.assertEqual(result["sample_cv"], 0)
        self.assertEqual(result["mean_per_operation"], 0)

    def test_calibration_accepts_clock_resolution_zeroes(self):
        sample = boundary.parse_sample(SAMPLE.replace("20.0000", "0.0000"))
        self.assertEqual(
            set(sample["calibration"]["calibration_timer_pair"]), {0.0}
        )

    def test_nearest_rank_percentiles(self):
        values = list(range(1, 101))
        self.assertEqual(boundary.nearest_rank(values, 0.50), 50)
        self.assertEqual(boundary.nearest_rank(values, 0.95), 95)
        self.assertEqual(boundary.nearest_rank(values, 0.99), 99)

    def test_ratios_are_paired_and_no_call_percentile_is_invented(self):
        samples = balanced_samples()
        samples[0]["metrics"]["direct_jsc_lower_bound"] = 50
        report = boundary.summarize(samples)
        ratios = report["paired_ratios"]["call_over_lower_bound"]
        self.assertEqual(ratios["mean"], (2.5 + 11 * 1.25) / 12)
        self.assertEqual(
            report["paired_ratios"]["call_over_prepared"]["mean"], 125 / 110
        )
        self.assertEqual(
            report["paired_ratios"]["common_js_call_over_direct"]["mean"], 1.1
        )
        self.assertFalse(report["all_run_mean_cv_at_most_5_percent"])
        self.assertIsNone(report["individual_call_p99"])
        self.assertFalse(report["performance_gate_qualified"])

    def test_entry_tail_and_allocator_scope_are_explicit(self):
        samples = balanced_samples()
        samples[0]["entry_batches"]["host_gate_admit_and_exit"][-1] = 40
        report = boundary.summarize(samples)
        latency = report["entry_batch_latency"]
        self.assertEqual(latency["sample_kind"], "contiguous_batch_mean")
        self.assertEqual(latency["operations_per_batch"], 1000)
        self.assertEqual(
            latency["metrics"]["host_gate_admit_and_exit"]["samples"], 12_000
        )
        self.assertLessEqual(
            latency["metrics"]["host_gate_admit_and_exit"]["p99"], 40
        )
        allocations = report["rust_allocator_activity"]
        self.assertIn("excludes", allocations)
        self.assertEqual(
            allocations["metrics"]["jsc_common_empty_entry"]["allocations"]["mean"],
            0,
        )
        self.assertEqual(
            allocations["metrics"]["rustjsi_experimental"]["allocations"]["mean"],
            0,
        )

    def test_callback_tail_is_a_block_mean_not_an_individual_call_tail(self):
        samples = balanced_samples()
        samples[0]["callback_batches"]["rustjsi_experimental"][-1] = 500
        report = boundary.summarize(samples)
        latency = report["callback_batch_latency"]
        self.assertEqual(latency["sample_kind"], "contiguous_batch_mean")
        self.assertEqual(latency["operations_per_batch"], 1000)
        self.assertEqual(
            latency["metrics"]["rustjsi_experimental"]["samples"], 12_000
        )
        self.assertLessEqual(
            latency["metrics"]["rustjsi_experimental"]["p99"], 500
        )

    def test_js_call_tail_and_allocator_scope_are_explicit(self):
        samples = balanced_samples()
        samples[0]["js_call_batches"]["rustjsi_common_js_call"][-1] = 500
        report = boundary.summarize(samples)
        latency = report["js_call_batch_latency"]
        self.assertEqual(latency["sample_kind"], "contiguous_batch_mean")
        self.assertEqual(latency["operations_per_batch"], 1000)
        self.assertEqual(
            latency["metrics"]["rustjsi_common_js_call"]["samples"], 12_000
        )
        self.assertLessEqual(
            latency["metrics"]["rustjsi_common_js_call"]["p99"], 500
        )
        self.assertEqual(
            report["rust_allocator_activity"]["metrics"][
                "rustjsi_common_js_call"
            ]["allocations"]["mean"],
            0,
        )

    def test_calibration_is_diagnostic_and_never_subtracted(self):
        samples = balanced_samples()
        samples[0]["calibration"]["calibration_timer_pair"][-1] = 200
        samples[0]["calibration"]["calibration_empty_batch"][-1] = 10
        calibration = boundary.summarize(samples)["measurement_calibration"]
        self.assertEqual(
            calibration["sample_kind"], "post_workload_diagnostic_control"
        )
        self.assertFalse(calibration["subtracted_from_workloads"])
        self.assertEqual(calibration["timer_pair"]["samples"], 12_000)
        self.assertEqual(
            calibration["timer_pair_amortized_over_measured_batch"][
                "operations_per_batch"
            ],
            1000,
        )
        self.assertEqual(calibration["empty_batch"]["samples"], 12_000)

    def test_mirrored_permutation_blocks_are_required(self):
        with self.assertRaises(ValueError):
            boundary.summarize(balanced_samples()[:-1])
        reordered = balanced_samples()
        reordered[0], reordered[1] = reordered[1], reordered[0]
        with self.assertRaisesRegex(ValueError, "schedule does not match"):
            boundary.summarize(reordered)
        wrong_js_order = balanced_samples()
        wrong_js_order[0]["js_call_order"] = boundary.JS_CALL_ORDERS[1]
        with self.assertRaisesRegex(ValueError, "JavaScript call workload schedule"):
            boundary.summarize(wrong_js_order)

    def test_run_count_requires_complete_mirrored_blocks(self):
        self.assertEqual(
            boundary.CALLBACK_SCHEDULE,
            boundary.CALLBACK_ORDERS + tuple(reversed(boundary.CALLBACK_ORDERS)),
        )
        for runs in (12, 24, 996):
            with self.subTest(runs=runs):
                self.assertTrue(boundary.valid_run_count(runs))
        for runs in (6, 18, 1000, True):
            with self.subTest(runs=runs):
                self.assertFalse(boundary.valid_run_count(runs))

    def test_identical_runs_have_zero_noise_not_gate_qualification(self):
        report = boundary.summarize(balanced_samples())
        self.assertTrue(report["all_run_mean_cv_at_most_5_percent"])
        self.assertFalse(report["performance_gate_qualified"])
        self.assertEqual(
            set(report["callback_ordering"]["counts"].values()), {2}
        )
        self.assertEqual(set(report["js_call_ordering"]["counts"].values()), {6})
        for metric in boundary.CALLBACK_METRICS.values():
            effects = report["callback_position_effects"][metric]
            self.assertEqual(effects["max_mean_spread"], 0)
            self.assertEqual(
                {item["samples"] for item in effects["positions"].values()}, {4}
            )
        for metric in boundary.JS_CALL_METRICS.values():
            effects = report["js_call_position_effects"][metric]
            self.assertEqual(effects["max_mean_spread"], 0)
            self.assertEqual(
                {item["samples"] for item in effects["positions"].values()}, {6}
            )


class ArtifactTests(unittest.TestCase):
    HASHES = {name: "a" * 64 for name in boundary.BENCHMARKS}

    @staticmethod
    def host_environment():
        return {
            "kind": "collection_context_not_isolation",
            "sysctl": {
                name: {"status": "available", "output": "test"}
                for name in boundary.HOST_SYSCTL_KEYS
            },
            "power_source": {"status": "available", "output": "Now drawing from 'AC Power'"},
            "power_settings": {"status": "unavailable"},
            "limits": list(boundary.HOST_ENVIRONMENT_LIMITS),
        }

    def test_host_environment_records_optional_facts_without_claiming_control(self):
        values = {
            ("sysctl", "-n", "hw.model"): "TestMac",
            ("sysctl", "-n", "hw.logicalcpu"): "12",
            ("sysctl", "-n", "hw.physicalcpu"): "10",
            ("sysctl", "-n", "hw.memsize"): "17179869184",
            ("sysctl", "-n", "hw.perflevel0.physicalcpu"): "4",
            ("sysctl", "-n", "hw.perflevel1.physicalcpu"): "6",
            ("pmset", "-g", "ps"): "Now drawing from 'AC Power'",
            ("pmset", "-g"): "Currently in use:\n lowpowermode 0",
        }
        timeouts = set()

        def fake_command(arguments, *, timeout):
            timeouts.add(timeout)
            return values[tuple(arguments)]

        with patch.object(boundary, "command", side_effect=fake_command):
            snapshot = boundary.host_environment()
        self.assertTrue(boundary.valid_host_environment(snapshot))
        self.assertEqual(snapshot["kind"], "collection_context_not_isolation")
        self.assertIn("does_not_measure_thermal_state", snapshot["limits"])
        self.assertIn("recorded_once_before_process_runs", snapshot["limits"])
        self.assertEqual(snapshot["sysctl"], {
            name: {"status": "available", "output": output}
            for name, output in {
                "model": "TestMac",
                "logical_cpus": "12",
                "physical_cpus": "10",
                "memory_bytes": "17179869184",
                "performance_level_0_physical_cpus": "4",
                "performance_level_1_physical_cpus": "6",
            }.items()
        })
        self.assertEqual(snapshot["power_source"]["output"], "Now drawing from 'AC Power'")
        self.assertEqual(timeouts, {5})

    def test_host_environment_tolerates_unavailable_optional_fields(self):
        failures = (
            OSError("missing"),
            subprocess.CalledProcessError(1, "sysctl"),
            subprocess.TimeoutExpired("pmset", boundary.HOST_COMMAND_TIMEOUT_SECONDS),
            UnicodeDecodeError("utf-8", b"\xff", 0, 1, "invalid start byte"),
        )
        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                with patch.object(boundary, "command", side_effect=failure):
                    snapshot = boundary.host_environment()
                self.assertTrue(boundary.valid_host_environment(snapshot))
                for name in boundary.HOST_POWER_COMMANDS:
                    self.assertEqual(snapshot[name], {"status": "unavailable"})

    def test_host_environment_validation_rejects_ambiguous_or_overclaimed_records(self):
        valid = self.host_environment()
        self.assertTrue(boundary.valid_host_environment(valid))
        invalid = dict(valid)
        invalid["kind"] = "isolated_machine"
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["sysctl"] = {"model": {"status": "available"}}
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["power_source"] = {"status": "available", "output": "", "guessed": True}
        self.assertFalse(boundary.valid_host_environment(invalid))
        for extra in ("performance_gate_qualified", "isolated"):
            invalid = self.host_environment()
            invalid[extra] = True
            self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        del invalid["power_settings"]
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["sysctl"]["isolated_cpus"] = {"status": "available", "output": "4"}
        self.assertFalse(boundary.valid_host_environment(invalid))
        for malformed in (
            {"status": "isolated"},
            {"status": "available", "output": "10", "guessed": True},
            {"status": "unavailable", "output": "10"},
        ):
            invalid = self.host_environment()
            invalid["sysctl"]["physical_cpus"] = malformed
            self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        del invalid["sysctl"]["memory_bytes"]
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["sysctl"] = list(invalid["sysctl"].items())
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["power_settings"] = {"status": "isolated"}
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["power_settings"] = {"status": "unavailable", "output": "guess"}
        self.assertFalse(boundary.valid_host_environment(invalid))
        for output in (16, {"isolated": True}, None):
            invalid = self.host_environment()
            invalid["power_source"] = {"status": "available", "output": output}
            self.assertFalse(boundary.valid_host_environment(invalid))

    def test_optional_command_bounds_the_real_subprocess_call(self):
        calls = []

        def slow_run(arguments, **options):
            calls.append(options["timeout"])
            raise subprocess.TimeoutExpired(arguments, options["timeout"])

        with patch.object(boundary.subprocess, "run", side_effect=slow_run):
            self.assertEqual(
                boundary.optional_command(["pmset", "-g"]), {"status": "unavailable"}
            )
        self.assertEqual(calls, [5])

    def test_host_environment_masks_process_names_and_battery_identity(self):
        outputs = {
            ("pmset", "-g", "ps"): "Now drawing from 'AC Power'\n"
            " -InternalBattery-0 (id=12345678)\t92%; charging",
            ("pmset", "-g"): " sleep                1 (sleep prevented by Editor, powerd)\n"
            " displaysleep         2 (display sleep prevented by Player)\n"
            " standby              1 (standby prevented by Code Helper (Renderer), (null))\n"
            " hibernatefile        /Volumes/Archive/custom-sleepimage",
        }
        with patch.object(
            boundary, "command",
            side_effect=lambda arguments, *, timeout: outputs.get(tuple(arguments), "1"),
        ):
            snapshot = boundary.host_environment()
        text = json.dumps(snapshot)
        for secret in (
            "12345678", "Editor", "powerd", "Player", "Renderer", "Code Helper", "custom-sleepimage",
        ):
            self.assertNotIn(secret, text)
        self.assertEqual(
            snapshot["power_source"]["output"].splitlines()[1],
            " -InternalBattery-0 (id=[redacted])\t92%; charging",
        )
        settings = snapshot["power_settings"]["output"].splitlines()
        self.assertEqual(settings[0], " sleep                1 (sleep prevented by [redacted]")
        self.assertEqual(settings[2], " standby              1 (standby prevented by [redacted]")
        self.assertEqual(settings[3], " hibernatefile        [redacted]")
        self.assertIn("Now drawing from 'AC Power'", snapshot["power_source"]["output"])
        invalid = self.host_environment()
        invalid["limits"] = invalid["limits"][:-1]
        self.assertFalse(boundary.valid_host_environment(invalid))
        invalid = self.host_environment()
        invalid["limits"] = list(reversed(invalid["limits"]))
        self.assertFalse(boundary.valid_host_environment(invalid))

    def test_report_requires_host_context_for_current_schema(self):
        variants = {"missing": None, "overclaimed": {
            **self.host_environment(), "kind": "isolated_machine",
        }}
        for name, host in variants.items():
            with self.subTest(host=name), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                metadata = {
                    "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 12,
                    "source": {"head": "test"},
                    "binary_sha256": self.HASHES,
                    "callback_ordering": boundary.CALLBACK_ORDERING,
                    "js_call_ordering": boundary.JS_CALL_ORDERING,
                }
                if host is not None:
                    metadata["host_environment"] = host
                boundary.write_json(directory / "metadata.json", metadata)
                boundary.write_json(directory / "complete.json", {
                    key: metadata[key] for key in ("source", "binary_sha256")
                })
                with self.assertRaisesRegex(ValueError, "unsupported benchmark metadata"):
                    boundary.read_report(directory)

    def test_cli_report_is_byte_identical_across_hash_seeds(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 12,
                "source": {"head": "test"},
                "binary_sha256": self.HASHES,
                "host_environment": self.host_environment(),
                "callback_ordering": boundary.CALLBACK_ORDERING,
                "js_call_ordering": boundary.JS_CALL_ORDERING,
            }
            boundary.write_json(directory / "metadata.json", metadata)
            for index in range(12):
                (directory / f"run-{index:03}.stdout").write_text(timing_sample(
                    boundary.CALLBACK_SCHEDULE[index], boundary.JS_CALL_SCHEDULE[index],
                ))
                (directory / f"allocation-run-{index:03}.stdout").write_text(
                    ALLOCATION_SAMPLE
                )
            boundary.write_json(directory / "complete.json", {
                key: metadata[key] for key in ("source", "binary_sha256")
            })
            outputs = set()
            # Set-typed metric names iterate in a seed-dependent order.
            for seed in ("0", "1", "2"):
                result = subprocess.run(
                    [sys.executable, "-B", str(Path(boundary.__file__)), "report",
                     str(directory)],
                    capture_output=True, check=True, timeout=60,
                    env={**os.environ, "PYTHONHASHSEED": seed},
                )
                outputs.add(result.stdout)
            self.assertEqual(len(outputs), 1)

    def test_cli_report_output_uses_canonical_key_order(self):
        output = io.StringIO()
        with (
            patch.object(boundary, "read_report", return_value={"z": {"b": 2, "a": 1}, "a": 0}),
            patch.object(boundary.sys, "argv", ["boundary.py", "report", "unused"]),
            contextlib.redirect_stdout(output),
        ):
            boundary.main()
        self.assertEqual(
            output.getvalue(),
            '{\n  "a": 0,\n  "z": {\n    "a": 1,\n    "b": 2\n  }\n}\n',
        )

    def test_cargo_executable_selection(self):
        output = json.dumps({"reason": "build-finished", "success": True}) + "\n"
        with self.assertRaises(ValueError):
            boundary.executables_from_cargo(output)
        artifacts = [
            {
                "reason": "compiler-artifact",
                "target": {"name": name, "kind": ["bench"]},
                "executable": f"/tmp/{name}",
            }
            for name in boundary.BENCHMARKS
        ]
        output += "\n".join(json.dumps(artifact) for artifact in artifacts)
        self.assertEqual(boundary.executables_from_cargo(output), {
            name: Path(f"/tmp/{name}") for name in boundary.BENCHMARKS
        })
        artifacts[0]["executable"] = "/tmp/other"
        with self.assertRaises(ValueError):
            boundary.executables_from_cargo(output + "\n" + json.dumps(artifacts[0]))

    def test_outputs_are_never_overwritten(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "record.json"
            boundary.write_json(target, {"original": True})
            with self.assertRaises(FileExistsError):
                boundary.write_json(target, {"original": False})
            self.assertEqual(json.loads(target.read_text()), {"original": True})

    def test_json_artifacts_use_canonical_key_order(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary) / "record.json"
            boundary.write_json(target, {"z": {"b": 2, "a": 1}, "a": 0})
            self.assertEqual(
                target.read_text(),
                '{\n  "a": 0,\n  "z": {\n    "a": 1,\n    "b": 2\n  }\n}\n',
            )

    def test_report_uses_raw_samples_and_requires_completion(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 12,
                "source": {"head": "test"},
                "binary_sha256": self.HASHES,
                "host_environment": self.host_environment(),
                "callback_ordering": boundary.CALLBACK_ORDERING,
                "js_call_ordering": boundary.JS_CALL_ORDERING,
            }
            boundary.write_json(directory / "metadata.json", metadata)
            for index in range(12):
                callback_order = boundary.CALLBACK_SCHEDULE[index]
                js_call_order = boundary.JS_CALL_SCHEDULE[index]
                (directory / f"run-{index:03}.stdout").write_text(
                    timing_sample(callback_order, js_call_order)
                )
                (directory / f"allocation-run-{index:03}.stdout").write_text(
                    ALLOCATION_SAMPLE
                )
            with self.assertRaises(FileNotFoundError):
                boundary.read_report(directory)
            completion = {key: metadata[key] for key in ("source", "binary_sha256")}
            boundary.write_json(directory / "complete.json", completion)
            report = boundary.read_report(directory)
            self.assertEqual(report["compiler_selection"], "unverified")
            self.assertEqual(report["host_environment"], metadata["host_environment"])
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

    def test_collection_cannot_pollute_its_source_fingerprint(self):
        result = subprocess.CompletedProcess([], 1)
        with (
            patch.object(boundary.platform, "system", return_value="Darwin"),
            patch.object(boundary.subprocess, "run", return_value=result),
        ):
            with self.assertRaisesRegex(ValueError, "must be Git-ignored"):
                boundary.collect(boundary.ROOT / "not-created", 12, "1.98.0")

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
                executables = {
                    name: root / name for name in boundary.BENCHMARKS
                }
                for executable in executables.values():
                    executable.write_bytes(b"original binary")
                directory = root / "results"
                artifacts = "\n".join(json.dumps({
                    "reason": "compiler-artifact",
                    "target": {"name": name, "kind": ["bench"]},
                    "executable": str(executable),
                }) for name, executable in executables.items())

                events = []

                def fake_command(arguments, **_):
                    events.append(("command", tuple(arguments)))
                    return "test metadata"

                def fake_process(arguments, destination, name, *, environment=None):
                    events.append(("process", name))
                    if name == "build":
                        self.assertEqual(environment["RUSTC"], "/test/rustc")
                    if name == "build":
                        output = artifacts
                    elif name.startswith("allocation-run-"):
                        self.assertEqual(
                            environment["RUSTJSI_CALLBACK_ORDER"],
                            boundary.CALLBACK_SCHEDULE[
                                int(name.rsplit("-", 1)[1])
                                % len(boundary.CALLBACK_SCHEDULE)
                            ],
                        )
                        self.assertEqual(
                            environment["RUSTJSI_JS_CALL_ORDER"],
                            boundary.JS_CALL_SCHEDULE[
                                int(name.rsplit("-", 1)[1])
                                % len(boundary.JS_CALL_SCHEDULE)
                            ],
                        )
                        output = ALLOCATION_SAMPLE
                    else:
                        output = timing_sample(
                            environment["RUSTJSI_CALLBACK_ORDER"],
                            environment["RUSTJSI_JS_CALL_ORDER"],
                        )
                    (destination / f"{name}.stdout").write_text(output)
                    (destination / f"{name}.stderr").write_text("")
                    if changed == "binary" and name == "allocation-run-011":
                        executables["boundary_allocations"].write_bytes(b"changed binary")
                    return output

                stamp = {"head": "original"}
                final = {"head": "changed"} if changed == "source" else stamp
                with (
                    patch.object(boundary.platform, "system", return_value="Darwin"),
                    patch.object(boundary, "source_stamp", side_effect=[stamp, final]),
                    patch.object(boundary, "command", side_effect=fake_command),
                    patch.object(boundary, "compiler_environment", return_value={
                        "RUSTC": "/test/rustc", "RUSTDOC": "/test/rustdoc",
                    }),
                    patch.object(boundary, "record_process", side_effect=fake_process) as run,
                    patch("builtins.print"),
                ):
                    if changed:
                        with self.assertRaisesRegex(RuntimeError, "changed during collection"):
                            boundary.collect(directory, 12, "test-toolchain")
                        self.assertFalse((directory / "complete.json").exists())
                        self.assertFalse((directory / "summary.json").exists())
                        with self.assertRaisesRegex(ValueError, "collection failed"):
                            boundary.read_report(directory)
                    else:
                        report = boundary.collect(directory, 12, "test-toolchain")
                        self.assertEqual(boundary.read_report(directory), report)
                    self.assertEqual(run.call_count, 25)
                    # Host context is captured exactly once, after the build and
                    # before the first benchmark process.
                    host_commands = {
                        ("sysctl", "-n", key) for key in boundary.HOST_SYSCTL_KEYS.values()
                    } | set(boundary.HOST_POWER_COMMANDS.values())
                    host_positions = [
                        index for index, event in enumerate(events)
                        if event[0] == "command" and event[1] in host_commands
                    ]
                    self.assertEqual(len(host_positions), len(host_commands))
                    self.assertEqual(
                        {events[index][1] for index in host_positions}, host_commands
                    )
                    first_run = events.index(("process", "run-000"))
                    self.assertLess(events.index(("process", "build")), min(host_positions))
                    self.assertLess(max(host_positions), first_run)
                    # An existing collection is never reused, including failed ones.
                    with self.assertRaises(FileExistsError):
                        boundary.collect(directory, 12, "test-toolchain")

    def test_report_rejects_invalid_completion_metadata(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": boundary.SCHEMA, "benchmark": "boundary", "runs": 12,
                "source": {"head": "before"},
                "binary_sha256": self.HASHES,
                "host_environment": self.host_environment(),
                "callback_ordering": boundary.CALLBACK_ORDERING,
                "js_call_ordering": boundary.JS_CALL_ORDERING,
            }
            boundary.write_json(directory / "metadata.json", metadata)
            boundary.write_json(directory / "complete.json", {
                "source": {"head": "after"}, "binary_sha256": "test-digest",
            })
            with self.assertRaisesRegex(ValueError, "source or binary changed"):
                boundary.read_report(directory)

    def test_scoped_call_analysis_requires_schema_eleven(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            metadata = {
                "schema": 10,
                "benchmark": "boundary",
                "runs": 12,
                "source": {"head": "before"},
                "binary_sha256": self.HASHES,
            }
            boundary.write_json(directory / "metadata.json", metadata)
            boundary.write_json(directory / "complete.json", {
                "source": metadata["source"],
                "binary_sha256": metadata["binary_sha256"],
            })
            with self.assertRaisesRegex(ValueError, "unsupported benchmark metadata"):
                boundary.read_report(directory)

    def test_report_rejects_invalid_binary_hash_metadata(self):
        for hashes in ("digest", {"boundary": "a" * 64}, {
            name: "not-a-hash" for name in boundary.BENCHMARKS
        }):
            with self.subTest(hashes=hashes), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                boundary.write_json(directory / "metadata.json", {
                    "schema": boundary.SCHEMA,
                    "benchmark": "boundary",
                    "runs": 12,
                    "source": {"head": "before"},
                    "binary_sha256": hashes,
                    "host_environment": self.host_environment(),
                })
                boundary.write_json(directory / "complete.json", {
                    "source": {"head": "before"}, "binary_sha256": hashes,
                })
                with self.assertRaisesRegex(ValueError, "unsupported benchmark metadata"):
                    boundary.read_report(directory)

    def test_compiler_selection_overrides_path_and_wrappers_locally(self):
        original = {"PATH": "/wrong/bin", "RUSTC": "/wrong/rustc",
                    "RUSTC_WRAPPER": "wrapper", "RUSTC_WORKSPACE_WRAPPER": "other"}
        with (
            patch.dict(boundary.os.environ, original, clear=True),
            patch.object(boundary, "command", side_effect=["/selected/rustc", "/selected/rustdoc"]),
        ):
            selected = boundary.compiler_environment("test")
            self.assertEqual(dict(boundary.os.environ), original)
        self.assertEqual(selected["RUSTC"], "/selected/rustc")
        self.assertEqual(selected["RUSTDOC"], "/selected/rustdoc")
        self.assertEqual(selected["CARGO_BUILD_RUSTC"], "/selected/rustc")
        self.assertEqual(selected["CARGO_BUILD_RUSTDOC"], "/selected/rustdoc")
        self.assertEqual(selected["RUSTC_WRAPPER"], "")
        self.assertEqual(selected["RUSTC_WORKSPACE_WRAPPER"], "")
        self.assertEqual(selected["CARGO_BUILD_RUSTC_WRAPPER"], "")
        self.assertEqual(selected["CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"], "")
        self.assertEqual(selected["RUSTUP_TOOLCHAIN"], "test")

    def test_compiler_path_must_be_absolute(self):
        with patch.object(boundary, "command", return_value="rustc"):
            with self.assertRaisesRegex(ValueError, "non-absolute"):
                boundary.compiler_environment("test")


if __name__ == "__main__":
    unittest.main()
