# SPDX-License-Identifier: MIT OR Apache-2.0
"""Collect exact-owner external-buffer evidence on macOS JavaScriptCore."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess

import boundary


BENCHMARK = "external_buffer_accounting"
SIZES = (0, 1, 4 * 1024, 1024 * 1024)
LINE = re.compile(
    r"external_buffer_accounting: payload_bytes=(?P<bytes>[0-9]+) "
    r"backing_origin=(?P<origin>None|Some\(true\)) "
    r"js_mutation=verified deallocator_origin=Some\(true\) "
    r"deallocator_runtime_thread=Some\((?P<thread>true|false)\) "
    r"rust_transfer_allocations=(?P<allocations>[0-9]+) "
    r"rust_transfer_allocated_bytes=(?P<allocated_bytes>[0-9]+) "
    r"rust_transfer_deallocations=(?P<deallocations>[0-9]+) "
    r"rust_transfer_deallocated_bytes=(?P<deallocated_bytes>[0-9]+)"
)


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
                raise ValueError("Cargo reported multiple accounting executables")
            executable = candidate
    if executable is None:
        raise ValueError("Cargo did not report the accounting executable")
    return executable


def parse_probe(output, expected_bytes):
    lines = [line for line in output.splitlines() if line.strip()]
    if len(lines) != 1:
        raise ValueError("accounting probe must emit exactly one line")
    match = LINE.fullmatch(lines[0])
    if not match:
        raise ValueError("invalid accounting probe output")
    values = match.groupdict()
    if int(values.pop("bytes")) != expected_bytes:
        raise ValueError("accounting probe reported the wrong payload size")
    expected_origin = "None" if expected_bytes == 0 else "Some(true)"
    if values.pop("origin") != expected_origin:
        raise ValueError("external backing did not retain the expected origin")
    values["deallocator_runtime_thread"] = values.pop("thread") == "true"
    for key in tuple(values):
        if key != "deallocator_runtime_thread":
            values[key] = int(values[key])
    values["payload_copy_bytes"] = 0 if expected_bytes else None
    values["payload_copy_basis"] = (
        "not-applicable-empty-payload"
        if expected_bytes == 0
        else "jsc-backing-origin-equals-transferred-allocation"
    )
    values["engine_allocation"] = "unmeasured"
    return values


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
        (directory / f"{name}.{suffix}").write_text(content, encoding="utf-8")


def collect(directory, toolchain):
    if platform.system() != "Darwin":
        raise ValueError("external-buffer accounting requires macOS system JavaScriptCore")
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
    for size in SIZES:
        run_environment = os.environ.copy()
        run_environment["RUSTJSI_EXTERNAL_BUFFER_BYTES"] = str(size)
        completed = subprocess.run(
            [str(executable)], cwd=boundary.ROOT, capture_output=True, text=True,
            check=False, timeout=300, env=run_environment,
        )
        save_process(completed, directory, f"payload-{size}")
        if completed.returncode:
            raise RuntimeError(f"payload {size} exited with {completed.returncode}")
        records.append({"payload_bytes": size, **parse_probe(completed.stdout, size)})
    if boundary.source_stamp() != stamp:
        raise RuntimeError("source state changed during collection")
    if hashlib.sha256(executable.read_bytes()).hexdigest() != binary_hash:
        raise RuntimeError("accounting executable changed during collection")
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
        "engine_allocation": "unmeasured",
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
    arguments = parser.parse_args()
    collect(arguments.output, arguments.toolchain)


if __name__ == "__main__":
    main()
