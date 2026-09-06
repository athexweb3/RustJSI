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


if __name__ == "__main__":
    unittest.main()
