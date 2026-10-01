# SPDX-License-Identifier: MIT OR Apache-2.0

import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import external_buffer_profile as profile


def sample_line(name, first):
    samples = ",".join(str(first + index / 10) for index in range(profile.MEASURED_BLOCKS))
    return f"{name}: {profile.BLOCK_SIZE} ops/batch {samples} ns/transfer"


def valid_output(order="direct,rustjsi", payload_bytes=4096):
    return "\n".join((
        f"external_buffer_order: {order}",
        f"external_buffer_payload_bytes: {payload_bytes}",
        "external_buffer_cleanup: property-clear,runtime-teardown",
        sample_line("direct_jsc_external_buffer", 10),
        sample_line("rustjsi_external_buffer", 12),
        "rustjsi_external_buffer_over_direct: 1.2000x (128 blocks)",
    ))


def valid_allocation_output(payload_bytes=4096):
    return (
        f"external_buffer_profile_allocations: payload_bytes={payload_bytes} "
        "block_size=8 cleanup=property-clear,runtime-teardown "
        "direct_allocations=32 direct_allocated_bytes=1648 "
        "direct_deallocations=24 direct_deallocated_bytes=1456 "
        "rustjsi_allocations=40 rustjsi_allocated_bytes=2032 "
        "rustjsi_deallocations=24 rustjsi_deallocated_bytes=1456"
    )


class ExternalBufferProfileTests(unittest.TestCase):
    def test_even_run_count_bounds(self):
        self.assertEqual(profile.even_run_count("12"), 12)
        for value in ("1", "3", "98"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                profile.even_run_count(value)

    def test_parse_profile(self):
        record = profile.parse_profile(valid_output(), 4096, "direct,rustjsi")
        self.assertEqual(record["payload_bytes"], 4096)
        self.assertEqual(record["order"], "direct,rustjsi")
        self.assertEqual(record["block_size"], 8)
        self.assertEqual(len(record["direct_samples_ns_per_transfer"]), 128)
        self.assertEqual(len(record["rustjsi_samples_ns_per_transfer"]), 128)

    def test_parse_profile_rejects_unknown_or_incomplete_output(self):
        with self.assertRaisesRegex(ValueError, "unknown"):
            profile.parse_profile(valid_output() + "\nextra: value", 4096, "direct,rustjsi")
        with self.assertRaisesRegex(ValueError, "incomplete"):
            profile.parse_profile(
                "\n".join(valid_output().splitlines()[:-1]), 4096, "direct,rustjsi"
            )

    def test_parse_profile_rejects_wrong_order_and_sample_count(self):
        with self.assertRaisesRegex(ValueError, "unexpected"):
            profile.parse_profile(valid_output("rustjsi,direct"), 4096, "direct,rustjsi")
        incomplete = valid_output().replace(
            ",".join(str(10 + index / 10) for index in range(profile.MEASURED_BLOCKS)),
            "10.0",
        )
        with self.assertRaisesRegex(ValueError, "samples"):
            profile.parse_profile(incomplete, 4096, "direct,rustjsi")

    def test_parse_allocation_probe(self):
        self.assertEqual(profile.parse_allocation_probe(valid_allocation_output(), 4096), {
            "direct_allocations": 32,
            "direct_allocated_bytes": 1648,
            "direct_deallocations": 24,
            "direct_deallocated_bytes": 1456,
            "rustjsi_allocations": 40,
            "rustjsi_allocated_bytes": 2032,
            "rustjsi_deallocations": 24,
            "rustjsi_deallocated_bytes": 1456,
        })
        with self.assertRaisesRegex(ValueError, "wrong payload size"):
            profile.parse_allocation_probe(valid_allocation_output(1), 4096)

    def test_cargo_executable_selection(self):
        artifact = json.dumps({
            "reason": "compiler-artifact",
            "target": {"name": profile.BENCHMARK, "kind": ["bench"]},
            "executable": "/tmp/external-buffer-profile",
        })
        self.assertEqual(profile.executable_from_cargo(artifact), Path("/tmp/external-buffer-profile"))
        with self.assertRaisesRegex(ValueError, "did not report"):
            profile.executable_from_cargo("{}")

    def test_repository_output_must_be_ignored(self):
        class Result:
            returncode = 1

        with patch.object(profile.subprocess, "run", return_value=Result()):
            with self.assertRaisesRegex(ValueError, "must be Git-ignored"):
                profile.require_ignored_output(profile.boundary.ROOT / "public-output")

    def test_collect_records_balanced_orders_and_completion(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "external-buffer-profile"
            executable.write_bytes(b"profile executable")
            output = root / "capture"
            artifact = json.dumps({
                "reason": "compiler-artifact",
                "target": {"name": profile.BENCHMARK, "kind": ["bench"]},
                "executable": str(executable),
            })

            def run(arguments, **kwargs):
                if arguments[0] == "rustup":
                    target = arguments[arguments.index("--bench") + 1]
                    executable = root / target
                    executable.write_bytes(b"profile executable")
                    artifact = json.dumps({
                        "reason": "compiler-artifact",
                        "target": {"name": target, "kind": ["bench"]},
                        "executable": str(executable),
                    })
                    return subprocess.CompletedProcess(arguments, 0, artifact, "")
                environment = kwargs["env"]
                if arguments[0].endswith(profile.ALLOCATION_BENCHMARK):
                    return subprocess.CompletedProcess(
                        arguments,
                        0,
                        valid_allocation_output(
                            int(environment["RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES"])
                        ),
                        "",
                    )
                return subprocess.CompletedProcess(
                    arguments,
                    0,
                    valid_output(
                        environment["RUSTJSI_EXTERNAL_BUFFER_PROFILE_ORDER"],
                        int(environment["RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES"]),
                    ),
                    "",
                )

            stamp = {"head": "test"}
            with (
                patch.object(profile.platform, "system", return_value="Darwin"),
                patch.object(profile.boundary, "source_stamp", side_effect=[stamp, stamp]),
                patch.object(profile.boundary, "compiler_environment", return_value={
                    "RUSTC": "/test/rustc", "RUSTDOC": "/test/rustdoc",
                }),
                patch.object(profile.boundary, "command", return_value="test metadata"),
                patch.object(profile.subprocess, "run", side_effect=run),
            ):
                records = profile.collect(output, "1.98.0", 2)

            metadata = json.loads((output / "metadata.json").read_text())
            self.assertEqual(len(records), len(profile.SIZES) * 2)
            self.assertEqual(metadata["ordering"]["sequence"], list(profile.ORDERS))
            self.assertTrue((output / "allocation-records.json").is_file())
            self.assertTrue((output / "complete.json").is_file())

    def test_collect_rejects_non_macos_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "capture"
            with patch.object(profile.platform, "system", return_value="Linux"):
                with self.assertRaisesRegex(ValueError, "requires macOS"):
                    profile.collect(output, "1.98.0", 2)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
