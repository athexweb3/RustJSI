# SPDX-License-Identifier: MIT OR Apache-2.0
"""Capture a compiler-pinned macOS sample of one JSC callback workload."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

import boundary


PROFILE_TARGET = "callback_profile"
WORKLOADS = {"reused", "prepared", "rustjsi"}
DEFAULT_ITERATIONS = 200_000_000
MAX_ITERATIONS = 2**32 - 1


def positive_int(value, maximum):
    parsed = int(value)
    if not 1 <= parsed <= maximum:
        raise argparse.ArgumentTypeError(f"value must be between 1 and {maximum}")
    return parsed


def executable_from_cargo(output):
    executable = None
    for line in output.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == PROFILE_TARGET
            and "bench" in message.get("target", {}).get("kind", [])
            and message.get("executable")
        ):
            candidate = Path(message["executable"])
            if executable is not None and executable != candidate:
                raise ValueError("Cargo reported multiple callback profile executables")
            executable = candidate
    if executable is None:
        raise ValueError("Cargo did not report the callback profile executable")
    return executable


def save_process(result, directory, name):
    for suffix, content in (("stdout", result.stdout), ("stderr", result.stderr)):
        with (directory / f"{name}.{suffix}").open("x", encoding="utf-8") as output:
            output.write(content)


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


def capture(directory, workload, iterations, duration, interval, toolchain):
    if platform.system() != "Darwin":
        raise ValueError("callback sampling requires macOS system JavaScriptCore")
    if workload not in WORKLOADS:
        raise ValueError("unsupported callback workload")
    require_ignored_output(directory)
    directory.mkdir(parents=True, exist_ok=False)
    process = None
    try:
        stamp = boundary.source_stamp()
        build_environment = boundary.compiler_environment(toolchain)
        build_environment["CARGO_PROFILE_BENCH_DEBUG"] = "1"
        build = [
            "rustup", "run", toolchain, "cargo", "bench", "--locked",
            "-p", "rustjsi-backend-jsc", "--features", "experimental-jsc",
            "--bench", PROFILE_TARGET, "--no-run", "--message-format=json",
        ]
        built = subprocess.run(
            build,
            cwd=boundary.ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=300,
            env=build_environment,
        )
        save_process(built, directory, "build")
        if built.returncode:
            raise RuntimeError(f"build exited with {built.returncode}; see saved stderr")
        executable = executable_from_cargo(built.stdout)
        binary_hash = hashlib.sha256(executable.read_bytes()).hexdigest()
        profile = directory / "profile.txt"
        sample_command_template = [
            "/usr/bin/sample", "<pid>", str(duration), str(interval),
            "-mayDie", "-fullPaths", "-file", str(profile),
        ]
        metadata = {
            "schema": 1,
            "benchmark": PROFILE_TARGET,
            "workload": workload,
            "iterations": iterations,
            "sample_duration_seconds": duration,
            "sample_interval_milliseconds": interval,
            "started_utc": datetime.datetime.now(datetime.UTC).isoformat(),
            "source": stamp,
            "build_command": build,
            "sample_command_template": sample_command_template,
            "compiler_selection": "explicit",
            "compiler_paths": {
                key: build_environment[key] for key in ("RUSTC", "RUSTDOC")
            },
            "compiler_wrappers": "disabled",
            "rustc": boundary.command([build_environment["RUSTC"], "-Vv"]),
            "cargo": boundary.command(["rustup", "run", toolchain, "cargo", "-V"]),
            "os": boundary.command(["sw_vers"]),
            "architecture": platform.machine(),
            "cpu": boundary.command(["sysctl", "-n", "machdep.cpu.brand_string"]),
            "sdk": boundary.command(["xcrun", "--sdk", "macosx", "--show-sdk-version"]),
            "binary_sha256": binary_hash,
            "environment_overrides": {
                key: value for key, value in os.environ.items()
                if key in {
                    "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC",
                    "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET",
                }
                or key.startswith(("CARGO_PROFILE_", "JSC_"))
            },
        }
        boundary.write_json(directory / "metadata.json", metadata)

        environment = os.environ.copy()
        environment["RUSTJSI_CALLBACK_PROFILE"] = workload
        environment["RUSTJSI_PROFILE_ITERATIONS"] = str(iterations)
        process = subprocess.Popen(
            [str(executable)],
            cwd=boundary.ROOT,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=environment,
        )
        sample_command = sample_command_template.copy()
        sample_command[1] = str(process.pid)
        sampled = subprocess.run(
            sample_command,
            cwd=boundary.ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=duration + 120,
        )
        save_process(sampled, directory, "sample")
        stdout, stderr = process.communicate(timeout=300)
        workload_result = subprocess.CompletedProcess(
            [str(executable)], process.returncode, stdout, stderr
        )
        save_process(workload_result, directory, "workload")
        if sampled.returncode:
            raise RuntimeError(
                f"sample exited with {sampled.returncode}; see saved stderr"
            )
        if process.returncode:
            raise RuntimeError(
                f"workload exited with {process.returncode}; see saved stderr"
            )
        if not profile.is_file() or profile.stat().st_size == 0:
            raise RuntimeError("sample did not produce a profile")
        final_stamp = boundary.source_stamp()
        if final_stamp != stamp:
            raise RuntimeError("source state changed during capture")
        if hashlib.sha256(executable.read_bytes()).hexdigest() != binary_hash:
            raise RuntimeError("callback profile executable changed during capture")
        boundary.write_json(directory / "complete.json", {
            "source": final_stamp,
            "binary_sha256": binary_hash,
            "completed_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        })
        return metadata
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        if process is not None and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        boundary.write_json(directory / "failure.json", {"error": str(error)})
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workload", choices=sorted(WORKLOADS), required=True)
    parser.add_argument(
        "--iterations",
        type=lambda value: positive_int(value, MAX_ITERATIONS),
        default=DEFAULT_ITERATIONS,
    )
    parser.add_argument(
        "--duration", type=lambda value: positive_int(value, 60), default=5
    )
    parser.add_argument(
        "--interval-ms", type=lambda value: positive_int(value, 1_000), default=1
    )
    parser.add_argument("--toolchain", default="1.98.0")
    args = parser.parse_args()
    try:
        result = capture(
            args.output.resolve(),
            args.workload,
            args.iterations,
            args.duration,
            args.interval_ms,
            args.toolchain,
        )
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"callback-profile: {error}\n")
    print(json.dumps(result, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
