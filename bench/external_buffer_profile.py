# SPDX-License-Identifier: MIT OR Apache-2.0
"""Collect matched owned-external-buffer construction samples on macOS."""

import argparse
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import subprocess

import boundary


BENCHMARK = "external_buffer_profile"
SIZES = (0, 1, 4 * 1024, 64 * 1024)
ORDERS = ("direct,rustjsi", "rustjsi,direct")
DEFAULT_RUNS = 12
BLOCK_SIZE = 8
MEASURED_BLOCKS = 128
SAMPLE_LINE = re.compile(
    r"(?P<name>direct_jsc_external_buffer|rustjsi_external_buffer): "
    r"(?P<block_size>[0-9]+) ops/batch (?P<samples>.+) ns/transfer"
)
RATIO_LINE = re.compile(
    r"rustjsi_external_buffer_over_direct: (?P<ratio>[0-9]+\.[0-9]+)x "
    rf"\((?P<blocks>{MEASURED_BLOCKS}) blocks\)"
)


def even_run_count(value):
    parsed = int(value)
    if not 2 <= parsed <= 96 or parsed % len(ORDERS) != 0:
        raise argparse.ArgumentTypeError("runs must be an even number from 2 through 96")
    return parsed


def executable_from_cargo(output):
    executable = None
    for line in output.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == BENCHMARK
            and "bench" in message.get("target", {}).get("kind", [])
            and message.get("executable")
        ):
            candidate = Path(message["executable"])
            if executable is not None and executable != candidate:
                raise ValueError("Cargo reported multiple external-buffer profile executables")
            executable = candidate
    if executable is None:
        raise ValueError("Cargo did not report the external-buffer profile executable")
    return executable


def parse_samples(payload):
    samples = [float(value) for value in payload.split(",")]
    if (
        len(samples) != MEASURED_BLOCKS
        or any(not math.isfinite(value) or value <= 0 for value in samples)
    ):
        raise ValueError("invalid external-buffer block samples")
    return samples


def parse_profile(output, expected_bytes, expected_order):
    record = {}
    sample_sets = {}
    for line in output.splitlines():
        if not line.strip():
            continue
        name, separator, payload = line.partition(": ")
        if not separator:
            raise ValueError("malformed external-buffer profile line")
        if name == "external_buffer_order":
            if "order" in record or payload != expected_order:
                raise ValueError("invalid or unexpected external-buffer workload order")
            record["order"] = payload
        elif name == "external_buffer_payload_bytes":
            if "payload_bytes" in record or not payload.isdecimal():
                raise ValueError("invalid or duplicate external-buffer payload size")
            if int(payload) != expected_bytes:
                raise ValueError("external-buffer profile reported the wrong payload size")
            record["payload_bytes"] = expected_bytes
        elif name == "external_buffer_cleanup":
            if "cleanup" in record or payload != "property-clear,runtime-teardown":
                raise ValueError("invalid or duplicate external-buffer cleanup contract")
            record["cleanup"] = payload
        elif name in {"direct_jsc_external_buffer", "rustjsi_external_buffer"}:
            if name in sample_sets:
                raise ValueError("duplicate external-buffer sample set")
            match = SAMPLE_LINE.fullmatch(line)
            if not match or int(match["block_size"]) != BLOCK_SIZE:
                raise ValueError("invalid external-buffer sample line")
            sample_sets[name] = parse_samples(match["samples"])
        elif name == "rustjsi_external_buffer_over_direct":
            if "ratio" in record:
                raise ValueError("duplicate external-buffer ratio")
            match = RATIO_LINE.fullmatch(line)
            if not match:
                raise ValueError("invalid external-buffer ratio")
            ratio = float(match["ratio"])
            if not math.isfinite(ratio) or ratio <= 0:
                raise ValueError("external-buffer ratio must be finite and positive")
            record["ratio"] = ratio
        else:
            raise ValueError("unknown external-buffer profile line")
    if set(record) != {"order", "payload_bytes", "cleanup", "ratio"}:
        raise ValueError("incomplete external-buffer profile output")
    if set(sample_sets) != {"direct_jsc_external_buffer", "rustjsi_external_buffer"}:
        raise ValueError("incomplete external-buffer sample sets")
    record["block_size"] = BLOCK_SIZE
    record["direct_samples_ns_per_transfer"] = sample_sets["direct_jsc_external_buffer"]
    record["rustjsi_samples_ns_per_transfer"] = sample_sets["rustjsi_external_buffer"]
    return record


def require_ignored_output(directory):
    if not directory.is_relative_to(boundary.ROOT):
        return
    ignored = subprocess.run(
        ["git", "check-ignore", "--quiet", str(directory)],
        cwd=boundary.ROOT,
        check=False,
        timeout=60,
    )
    if ignored.returncode != 0:
        raise ValueError("output inside the repository must be Git-ignored")


def save_process(result, directory, name):
    for suffix, content in (("stdout", result.stdout), ("stderr", result.stderr)):
        with (directory / f"{name}.{suffix}").open("x", encoding="utf-8") as output:
            output.write(content)


def collect(directory, toolchain, runs):
    if platform.system() != "Darwin":
        raise ValueError("external-buffer profile requires macOS system JavaScriptCore")
    require_ignored_output(directory)
    directory.mkdir(parents=True, exist_ok=False)
    started_utc = datetime.datetime.now(datetime.UTC).isoformat()
    stamp = boundary.source_stamp()
    environment = boundary.compiler_environment(toolchain)
    build = [
        "rustup", "run", toolchain, "cargo", "bench", "--locked",
        "-p", "rustjsi-backend-jsc", "--features", "experimental-jsc",
        "--bench", BENCHMARK, "--no-run", "--message-format=json",
    ]
    built = subprocess.run(
        build, cwd=boundary.ROOT, capture_output=True, text=True, check=False,
        timeout=300, env=environment,
    )
    save_process(built, directory, "build")
    if built.returncode:
        raise RuntimeError(f"build exited with {built.returncode}; see saved stderr")
    executable = executable_from_cargo(built.stdout)
    binary_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
    records = []
    for payload_bytes in SIZES:
        for run in range(runs):
            order = ORDERS[run % len(ORDERS)]
            run_environment = os.environ.copy()
            run_environment["RUSTJSI_EXTERNAL_BUFFER_PROFILE_BYTES"] = str(payload_bytes)
            run_environment["RUSTJSI_EXTERNAL_BUFFER_PROFILE_ORDER"] = order
            completed = subprocess.run(
                [str(executable)], cwd=boundary.ROOT, capture_output=True, text=True,
                check=False, timeout=300, env=run_environment,
            )
            name = f"payload-{payload_bytes}-run-{run:02d}"
            save_process(completed, directory, name)
            if completed.returncode:
                raise RuntimeError(
                    f"payload {payload_bytes}, run {run} exited with {completed.returncode}"
                )
            records.append({
                "run": run,
                **parse_profile(completed.stdout, payload_bytes, order),
            })
    if boundary.source_stamp() != stamp:
        raise RuntimeError("source state changed during collection")
    if hashlib.sha256(executable.read_bytes()).hexdigest() != binary_hash:
        raise RuntimeError("external-buffer profile executable changed during collection")
    metadata = {
        "schema": 1,
        "benchmark": BENCHMARK,
        "started_utc": started_utc,
        "source": stamp,
        "build_command": build,
        "compiler_selection": "explicit",
        "compiler_paths": {key: environment[key] for key in ("RUSTC", "RUSTDOC")},
        "compiler_wrappers": "disabled",
        "rustc": boundary.command([environment["RUSTC"], "-Vv"]),
        "os": boundary.command(["sw_vers"]),
        "architecture": platform.machine(),
        "sdk": boundary.command(["xcrun", "--sdk", "macosx", "--show-sdk-version"]),
        "binary_sha256": binary_hash,
        "payload_sizes": list(SIZES),
        "runs_per_payload": runs,
        "ordering": {"design": "alternating-pair", "sequence": [
            ORDERS[index % len(ORDERS)] for index in range(runs)
        ]},
        "timed_scope": "external ArrayBuffer construction and global-property publication",
        "excluded_scope": [
            "payload allocation", "property cleanup", "runtime teardown",
            "deallocator verification", "engine allocation", "payload copy accounting",
        ],
    }
    boundary.write_json(directory / "metadata.json", metadata)
    boundary.write_json(directory / "records.json", records)
    boundary.write_json(directory / "complete.json", {
        "source": stamp,
        "binary_sha256": binary_hash,
        "completed_utc": datetime.datetime.now(datetime.UTC).isoformat(),
    })
    return records


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--toolchain", default="1.98.0")
    parser.add_argument("--runs", default=DEFAULT_RUNS, type=even_run_count)
    arguments = parser.parse_args()
    collect(arguments.output, arguments.toolchain, arguments.runs)


if __name__ == "__main__":
    main()
