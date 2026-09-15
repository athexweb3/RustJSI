# SPDX-License-Identifier: MIT OR Apache-2.0

import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import boundary
import callback_profile


class CallbackProfileTests(unittest.TestCase):
    def test_positive_integer_bounds(self):
        self.assertEqual(callback_profile.positive_int("5", 10), 5)
        for value in ("0", "11", "-1"):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError):
                callback_profile.positive_int(value, 10)

    def test_cargo_executable_selection(self):
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": callback_profile.PROFILE_TARGET, "kind": ["bench"]},
            "executable": "/tmp/callback-profile",
        }
        output = json.dumps({"reason": "build-finished"}) + "\n" + json.dumps(artifact)
        self.assertEqual(
            callback_profile.executable_from_cargo(output),
            Path("/tmp/callback-profile"),
        )
        with self.assertRaisesRegex(ValueError, "did not report"):
            callback_profile.executable_from_cargo("{}")
        artifact["executable"] = "/tmp/other"
        with self.assertRaisesRegex(ValueError, "multiple"):
            callback_profile.executable_from_cargo(
                output + "\n" + json.dumps(artifact)
            )

    def test_js_call_profile_target_and_workloads_are_explicit(self):
        spec = callback_profile.profile_spec("js-call")
        self.assertEqual(spec["target"], "js_call_profile")
        self.assertEqual(spec["environment"], "RUSTJSI_JS_CALL_PROFILE")
        self.assertEqual(spec["workloads"], {"direct", "common"})
        with self.assertRaisesRegex(ValueError, "unsupported profile target"):
            callback_profile.profile_spec("unknown")
        artifact = {
            "reason": "compiler-artifact",
            "target": {"name": "js_call_profile", "kind": ["bench"]},
            "executable": "/tmp/js-call-profile",
        }
        self.assertEqual(
            callback_profile.executable_from_cargo(
                json.dumps(artifact), "js_call_profile"
            ),
            Path("/tmp/js-call-profile"),
        )

    def test_js_call_capture_records_selector_without_path_serialization(self):
        class Process:
            pid = 123
            returncode = 0

            def communicate(self, timeout):
                assert timeout == 300
                return "profile output", ""

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            executable = root / "js-call-profile"
            executable.write_bytes(b"profile executable")
            output = root / "capture"
            artifact = json.dumps({
                "reason": "compiler-artifact",
                "target": {"name": "js_call_profile", "kind": ["bench"]},
                "executable": str(executable),
            })

            def run(arguments, **_kwargs):
                if arguments[0] == "rustup":
                    return subprocess.CompletedProcess(arguments, 0, artifact, "")
                profile = Path(arguments[-1])
                profile.write_text("sample")
                return subprocess.CompletedProcess(arguments, 0, "", "")

            stamp = {"head": "test"}
            with (
                patch.object(callback_profile.platform, "system", return_value="Darwin"),
                patch.object(callback_profile.boundary, "source_stamp", side_effect=[stamp, stamp]),
                patch.object(callback_profile.boundary, "compiler_environment", return_value={
                    "RUSTC": "/test/rustc", "RUSTDOC": "/test/rustdoc",
                }),
                patch.object(callback_profile.boundary, "command", return_value="test metadata"),
                patch.object(callback_profile.subprocess, "run", side_effect=run),
                patch.object(callback_profile.subprocess, "Popen", return_value=Process()),
            ):
                callback_profile.capture(
                    output, "direct", 1, 1, 1, "1.98.0", "js-call"
                )
            metadata = json.loads((output / "metadata.json").read_text())
            self.assertEqual(metadata["profile"], "js-call")
            self.assertEqual(metadata["benchmark"], "js_call_profile")
            self.assertTrue((output / "complete.json").is_file())

    def test_saved_process_output_is_exclusive(self):
        result = subprocess.CompletedProcess([], 0, "stdout", "stderr")
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            callback_profile.save_process(result, directory, "sample")
            self.assertEqual((directory / "sample.stdout").read_text(), "stdout")
            self.assertEqual((directory / "sample.stderr").read_text(), "stderr")
            with self.assertRaises(FileExistsError):
                callback_profile.save_process(result, directory, "sample")

    def test_repository_output_must_be_ignored(self):
        result = subprocess.CompletedProcess([], 1)
        with patch.object(callback_profile.subprocess, "run", return_value=result):
            with self.assertRaisesRegex(ValueError, "must be Git-ignored"):
                callback_profile.require_ignored_output(boundary.ROOT / "public-output")

    def test_capture_rejects_unsupported_platform_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "capture"
            with patch.object(callback_profile.platform, "system", return_value="Linux"):
                with self.assertRaisesRegex(ValueError, "requires macOS"):
                    callback_profile.capture(output, "rustjsi", 1, 1, 1, "1.98.0")
            self.assertFalse(output.exists())

    def test_capture_rejects_unknown_workload_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "capture"
            with patch.object(callback_profile.platform, "system", return_value="Darwin"):
                with self.assertRaisesRegex(ValueError, "unsupported"):
                    callback_profile.capture(output, "unknown", 1, 1, 1, "1.98.0")
            self.assertFalse(output.exists())

    def test_js_call_capture_rejects_wrong_workload_before_writing(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "capture"
            with patch.object(callback_profile.platform, "system", return_value="Darwin"):
                with self.assertRaisesRegex(ValueError, "unsupported"):
                    callback_profile.capture(
                        output, "rustjsi", 1, 1, 1, "1.98.0", "js-call"
                    )
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
