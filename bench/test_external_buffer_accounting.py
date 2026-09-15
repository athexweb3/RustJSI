# SPDX-License-Identifier: MIT OR Apache-2.0

import json
from pathlib import Path
import unittest

import external_buffer_accounting as accounting


class ExternalBufferAccountingTests(unittest.TestCase):
    def test_parse_nonempty_probe(self):
        output = (
            "external_buffer_accounting: payload_bytes=4096 backing_origin=Some(true) "
            "js_mutation=verified deallocator_origin=Some(true) "
            "deallocator_runtime_thread=Some(false) rust_transfer_allocations=5 "
            "rust_transfer_allocated_bytes=226 rust_transfer_deallocations=3 "
            "rust_transfer_deallocated_bytes=154"
        )
        self.assertEqual(accounting.parse_probe(output, 4096), {
            "deallocator_runtime_thread": False,
            "allocations": 5,
            "allocated_bytes": 226,
            "deallocations": 3,
            "deallocated_bytes": 154,
            "payload_copy_bytes": 0,
            "payload_copy_basis": "jsc-backing-origin-equals-transferred-allocation",
            "engine_allocation": "unmeasured",
        })

    def test_parse_empty_probe_has_no_pointer_copy_claim(self):
        output = (
            "external_buffer_accounting: payload_bytes=0 backing_origin=None "
            "js_mutation=verified deallocator_origin=Some(true) "
            "deallocator_runtime_thread=Some(true) rust_transfer_allocations=5 "
            "rust_transfer_allocated_bytes=226 rust_transfer_deallocations=3 "
            "rust_transfer_deallocated_bytes=154"
        )
        result = accounting.parse_probe(output, 0)
        self.assertIsNone(result["payload_copy_bytes"])
        self.assertEqual(result["payload_copy_basis"], "not-applicable-empty-payload")

    def test_parse_rejects_wrong_size_and_origin(self):
        with self.assertRaisesRegex(ValueError, "wrong payload size"):
            accounting.parse_probe(
                "external_buffer_accounting: payload_bytes=1 backing_origin=Some(true) "
                "js_mutation=verified deallocator_origin=Some(true) "
                "deallocator_runtime_thread=Some(true) rust_transfer_allocations=0 "
                "rust_transfer_allocated_bytes=0 rust_transfer_deallocations=0 "
                "rust_transfer_deallocated_bytes=0",
                2,
            )
        with self.assertRaisesRegex(ValueError, "expected origin"):
            accounting.parse_probe(
                "external_buffer_accounting: payload_bytes=1 backing_origin=None "
                "js_mutation=verified deallocator_origin=Some(true) "
                "deallocator_runtime_thread=Some(true) rust_transfer_allocations=0 "
                "rust_transfer_allocated_bytes=0 rust_transfer_deallocations=0 "
                "rust_transfer_deallocated_bytes=0",
                1,
            )

    def test_cargo_executable_selection(self):
        artifact = json.dumps({
            "reason": "compiler-artifact",
            "target": {"name": accounting.BENCHMARK, "kind": ["bench"]},
            "executable": "/tmp/external-buffer-accounting",
        })
        self.assertEqual(
            accounting.executable_from_cargo(artifact),
            Path("/tmp/external-buffer-accounting"),
        )


if __name__ == "__main__":
    unittest.main()
