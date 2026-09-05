# SPDX-License-Identifier: MIT OR Apache-2.0
"""Collect independent runs of the JSC boundary smoke benchmark (Python 3.11+)."""

import argparse
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import statistics
import subprocess
import sys


ROOT = Path(__file__).resolve().parent.parent
METRICS = {
    "direct_jsc_lower_bound": "ns/call",
    "host_gate_admit_and_exit": "ns/entry",
    "jsc_common_empty_entry": "ns/entry",
    "jsc_foreign_common_empty_entry": "ns/entry",
    "rustjsi_experimental": "ns/call",
    "direct_jsc_scalar": "ns/round-trip",
    "rustjsi_common_scalar": "ns/round-trip",
}
RATIOS = {"rustjsi_over_direct", "common_scalar_over_direct"}
ENTRY_METRICS = {
    "host_gate_admit_and_exit",
    "jsc_common_empty_entry",
    "jsc_foreign_common_empty_entry",
}
ITERATIONS = 1_000_000
ENTRY_BATCHES = 1_000
ENTRY_BATCH_ITERATIONS = ITERATIONS // ENTRY_BATCHES
ALLOCATION_FIELDS = (
    "allocations", "allocated_bytes", "deallocations", "deallocated_bytes"
)
BENCHMARKS = ("boundary", "boundary_allocations")
SCHEMA = 3


def parse_sample(output):
    """Reject partial, duplicated, unknown, or non-finite benchmark output."""
    values = {}
    ratios = set()
    entry_batches = {}
    rust_allocations = {}
    for line in output.splitlines():
        if not line.strip():
            continue
        name, separator, payload = line.partition(": ")
        if not separator:
            raise ValueError(f"malformed benchmark line: {line!r}")
        if name in METRICS:
            number, separator, unit = payload.partition(" ")
            if not separator or unit != METRICS[name] or name in values:
                raise ValueError(f"invalid or duplicate metric: {name}")
            value = float(number)
            if not math.isfinite(value) or value <= 0:
                raise ValueError(f"metric must be finite and positive: {name}")
            values[name] = value
        elif name in RATIOS:
            match = re.fullmatch(r"([0-9]+\.[0-9]+)x \(([0-9]+) iterations\)", payload)
            if (
                not match
                or name in ratios
                or int(match[2]) != ITERATIONS
                or not math.isfinite(float(match[1]))
                or float(match[1]) <= 0
            ):
                raise ValueError(f"invalid ratio or iteration count: {name}")
            ratios.add(name)
        elif name.startswith("entry_batches_"):
            metric = name.removeprefix("entry_batches_")
            match = re.fullmatch(r"([0-9]+) ops/batch (.+) ns/entry", payload)
            if not match or metric not in ENTRY_METRICS or metric in entry_batches:
                raise ValueError(f"invalid or duplicate entry batch metric: {metric}")
            samples = [float(value) for value in match[2].split(",")]
            if (
                int(match[1]) != ENTRY_BATCH_ITERATIONS
                or len(samples) != ENTRY_BATCHES
                or any(not math.isfinite(value) or value <= 0 for value in samples)
            ):
                raise ValueError(f"invalid entry batch samples: {metric}")
            entry_batches[metric] = samples
        elif name.startswith("rust_alloc_"):
            metric = name.removeprefix("rust_alloc_")
            match = re.fullmatch(
                r"([0-9]+) calls ([0-9]+) bytes ([0-9]+) deallocations "
                r"([0-9]+) deallocated-bytes \(([0-9]+) iterations\)",
                payload,
            )
            if not match or metric not in ENTRY_METRICS or metric in rust_allocations:
                raise ValueError(f"invalid or duplicate allocation metric: {metric}")
            if int(match[5]) != ITERATIONS:
                raise ValueError(f"invalid allocation iteration count: {metric}")
            rust_allocations[metric] = dict(zip(
                ALLOCATION_FIELDS, (int(value) for value in match.groups()[:4]), strict=True
            ))
        else:
            raise ValueError(f"unknown benchmark metric: {name}")
    if (
        values.keys() != METRICS.keys()
        or ratios != RATIOS
        or entry_batches.keys() != ENTRY_METRICS
        or rust_allocations.keys() != ENTRY_METRICS
    ):
        raise ValueError("incomplete benchmark output")
    for name, samples in entry_batches.items():
        if not math.isclose(statistics.mean(samples), values[name], abs_tol=0.011):
            raise ValueError(f"entry batch mean does not match metric: {name}")
    return {
        "metrics": values,
        "entry_batches": entry_batches,
        "rust_allocations": rust_allocations,
    }


def describe(values):
    """Statistics across process-level batch means, not individual call latencies."""
    if len(values) < 2 or any(not math.isfinite(x) or x <= 0 for x in values):
        raise ValueError("need at least two finite positive samples")
    mean = statistics.mean(values)
    return {
        "samples": len(values),
        "mean": mean,
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "sample_cv": statistics.stdev(values) / mean,
    }


def describe_nonnegative(values):
    """Describe counters where an exact zero is a valid and useful result."""
    if len(values) < 2 or any(not math.isfinite(x) or x < 0 for x in values):
        raise ValueError("need at least two finite nonnegative samples")
    mean = statistics.mean(values)
    return {
        "samples": len(values),
        "mean": mean,
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "sample_cv": statistics.stdev(values) / mean if mean else 0.0,
        "mean_per_entry": mean / ITERATIONS,
    }


def nearest_rank(values, percentile):
    """Return a nearest-rank percentile from finite positive observations."""
    if not values or not 0 < percentile <= 1:
        raise ValueError("need observations and a percentile in (0, 1]")
    ordered = sorted(values)
    return ordered[math.ceil(percentile * len(ordered)) - 1]


def summarize(samples):
    if len(samples) < 10:
        raise ValueError("need at least ten independent process runs")
    metrics = {
        name: {
            "unit": unit,
            **describe([sample["metrics"][name] for sample in samples]),
        }
        for name, unit in METRICS.items()
    }
    pairs = {
        "call_over_lower_bound": ("rustjsi_experimental", "direct_jsc_lower_bound"),
        "common_scalar_over_direct": ("rustjsi_common_scalar", "direct_jsc_scalar"),
    }
    return {
        "schema": SCHEMA,
        "sample_kind": "process_batch_mean",
        "metrics": metrics,
        "paired_ratios": {
            name: describe([
                sample["metrics"][top] / sample["metrics"][bottom]
                for sample in samples
            ])
            for name, (top, bottom) in pairs.items()
        },
        "entry_batch_latency": {
            "sample_kind": "contiguous_batch_mean",
            "operations_per_batch": ENTRY_BATCH_ITERATIONS,
            "metrics": {
                name: describe_entry_batches([
                    value
                    for sample in samples
                    for value in sample["entry_batches"][name]
                ], len(samples))
                for name in ENTRY_METRICS
            },
        },
        "rust_allocator_activity": {
            "scope": "Rust global allocator calls in the timed entry region",
            "excludes": "JavaScriptCore, system-framework, and foreign allocator activity",
            "iterations_per_process": ITERATIONS,
            "metrics": {
                name: {
                    field: describe_nonnegative([
                        sample["rust_allocations"][name][field] for sample in samples
                    ])
                    for field in ALLOCATION_FIELDS
                }
                for name in ENTRY_METRICS
            },
        },
        "all_run_mean_cv_at_most_5_percent": all(
            item["sample_cv"] <= 0.05 for item in metrics.values()
        ),
        "individual_call_p99": None,
        "performance_gate_qualified": False,
    }


def describe_entry_batches(values, processes):
    """Describe pooled, equal-sized block means without calling them call tails."""
    result = describe(values)
    result.update({
        "unit": "ns/entry",
        "processes": processes,
        "batches_per_process": ENTRY_BATCHES,
        "p50": nearest_rank(values, 0.50),
        "p95": nearest_rank(values, 0.95),
        "p99": nearest_rank(values, 0.99),
    })
    return result


def command(arguments, *, cwd=ROOT):
    result = subprocess.run(
        arguments, cwd=cwd, capture_output=True, text=True, check=True, timeout=60
    )
    return result.stdout.strip()


def source_stamp():
    # Ignored files are excluded; only hashes of public worktree contents are saved.
    diff = subprocess.run(
        ["git", "diff", "--binary", "HEAD"], cwd=ROOT, capture_output=True,
        check=True, timeout=60,
    ).stdout
    paths = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=ROOT, capture_output=True, check=True, timeout=60,
    ).stdout.split(b"\0")
    digest = hashlib.sha256()
    for relative in sorted(set(paths) - {b""}):
        path = ROOT / os.fsdecode(relative)
        digest.update(relative + b"\0")
        if path.is_symlink():
            digest.update(b"link\0" + os.fsencode(path.readlink()))
        elif path.is_file():
            digest.update(b"file\0" + hashlib.sha256(path.read_bytes()).digest())
        else:
            digest.update(b"missing-or-non-file\0")
    return {
        "head": command(["git", "rev-parse", "HEAD"]),
        "tracked_dirty": bool(diff),
        "tracked_diff_sha256": hashlib.sha256(diff).hexdigest(),
        "worktree_sha256": digest.hexdigest(),
        "untracked_files_present": bool(
            command(["git", "ls-files", "--others", "--exclude-standard"])
        ),
    }


def write_json(path, value):
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, indent=2, allow_nan=False)
        output.write("\n")


def executables_from_cargo(output):
    executables = {}
    for line in output.splitlines():
        message = json.loads(line)
        name = message.get("target", {}).get("name")
        if (
            message.get("reason") == "compiler-artifact"
            and name in BENCHMARKS
            and "bench" in message.get("target", {}).get("kind", [])
            and message.get("executable")
        ):
            if name in executables and executables[name] != message["executable"]:
                raise ValueError(f"Cargo reported multiple executables for {name}")
            executables[name] = Path(message["executable"])
    if executables.keys() != set(BENCHMARKS):
        raise ValueError("Cargo did not report both boundary benchmark executables")
    return executables


def compiler_environment(toolchain):
    environment = os.environ.copy()
    for variable, binary in (("RUSTC", "rustc"), ("RUSTDOC", "rustdoc")):
        path = command(["rustup", "which", "--toolchain", toolchain, binary])
        if not Path(path).is_absolute():
            raise ValueError(f"rustup returned a non-absolute {binary} path")
        environment[variable] = path
        environment[f"CARGO_BUILD_{variable}"] = path
    # Disable both environment- and Cargo-configured wrappers for this build.
    environment["RUSTC_WRAPPER"] = ""
    environment["RUSTC_WORKSPACE_WRAPPER"] = ""
    environment["CARGO_BUILD_RUSTC_WRAPPER"] = ""
    environment["CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER"] = ""
    environment["RUSTUP_TOOLCHAIN"] = toolchain
    return environment


def record_process(arguments, directory, name, *, environment=None):
    result = subprocess.run(
        arguments, cwd=ROOT, capture_output=True, text=True, check=False, timeout=300,
        env=environment,
    )
    for suffix, content in (("stdout", result.stdout), ("stderr", result.stderr)):
        with (directory / f"{name}.{suffix}").open("x", encoding="utf-8") as output:
            output.write(content)
    if result.returncode:
        raise RuntimeError(f"{name} exited with {result.returncode}; see saved stderr")
    return result.stdout


def valid_binary_hashes(value):
    return (
        isinstance(value, dict)
        and value.keys() == set(BENCHMARKS)
        and all(
            isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest)
            for digest in value.values()
        )
    )


def read_report(directory):
    if (directory / "failure.json").exists():
        raise ValueError("collection failed; saved samples are diagnostic only")
    with (directory / "metadata.json").open(encoding="utf-8") as source:
        metadata = json.load(source)
    with (directory / "complete.json").open(encoding="utf-8") as source:
        completion = json.load(source)
    if not isinstance(metadata, dict) or not isinstance(completion, dict):
        raise ValueError("metadata and completion must be objects")
    if (
        metadata.get("schema") != SCHEMA
        or metadata.get("benchmark") != "boundary"
        or not valid_binary_hashes(metadata.get("binary_sha256"))
    ):
        raise ValueError("unsupported benchmark metadata")
    count = metadata.get("runs")
    if type(count) is not int or not 10 <= count <= 1_000:
        raise ValueError("invalid run count")
    if (
        completion.get("source") != metadata.get("source")
        or completion.get("binary_sha256") != metadata.get("binary_sha256")
        or not metadata.get("source")
        or not metadata.get("binary_sha256")
    ):
        raise ValueError("source or binary changed during collection")
    samples = [
        parse_sample(
            (directory / f"run-{index:03}.stdout").read_text(encoding="utf-8")
            + (directory / f"allocation-run-{index:03}.stdout").read_text(
                encoding="utf-8"
            )
        )
        for index in range(count)
    ]
    report = summarize(samples)
    report["compiler_selection"] = metadata.get("compiler_selection", "unverified")
    return report


def collect(directory, runs, toolchain):
    if platform.system() != "Darwin":
        raise ValueError("the boundary benchmark requires macOS system JavaScriptCore")
    if not 10 <= runs <= 1_000:
        raise ValueError("runs must be between 10 and 1000")
    if directory.is_relative_to(ROOT):
        ignored = subprocess.run(
            ["git", "check-ignore", "--quiet", str(directory)], cwd=ROOT,
            check=False, timeout=60,
        )
        if ignored.returncode != 0:
            raise ValueError("output inside the repository must be Git-ignored")
    directory.mkdir(parents=True, exist_ok=False)
    try:
        stamp = source_stamp()
        build_environment = compiler_environment(toolchain)
        build = [
            "rustup", "run", toolchain, "cargo", "bench", "--locked",
            "-p", "rustjsi-backend-jsc", "--features", "experimental-jsc",
            "--bench", "boundary", "--bench", "boundary_allocations",
            "--no-run", "--message-format=json",
        ]
        executables = executables_from_cargo(record_process(
            build, directory, "build", environment=build_environment,
        ))
        binary_hashes = {
            name: hashlib.sha256(path.read_bytes()).hexdigest()
            for name, path in executables.items()
        }
        metadata = {
            "schema": SCHEMA,
            "benchmark": "boundary",
            "runs": runs,
            "warmup_iterations": 10_000,
            "measured_iterations": ITERATIONS,
            "entry_batches": ENTRY_BATCHES,
            "entry_batch_iterations": ENTRY_BATCH_ITERATIONS,
            "started_utc": datetime.datetime.now(datetime.UTC).isoformat(),
            "source": stamp,
            "build_command": build,
            "compiler_selection": "explicit",
            "compiler_paths": {
                key: build_environment[key] for key in ("RUSTC", "RUSTDOC")
            },
            "compiler_wrappers": "disabled",
            "rustc": command([build_environment["RUSTC"], "-Vv"]),
            "cargo": command(["rustup", "run", toolchain, "cargo", "-V"]),
            "os": command(["sw_vers"]),
            "architecture": platform.machine(),
            "cpu": command(["sysctl", "-n", "machdep.cpu.brand_string"]),
            "sdk": command(["xcrun", "--sdk", "macosx", "--show-sdk-version"]),
            "binary_sha256": binary_hashes,
            "environment_overrides": {
                key: value for key, value in os.environ.items()
                if key in {"RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTC_WRAPPER",
                           "RUSTC_WORKSPACE_WRAPPER", "CARGO_BUILD_TARGET"}
                or key.startswith(("CARGO_PROFILE_", "JSC_"))
            },
        }
        write_json(directory / "metadata.json", metadata)
        samples = []
        for index in range(runs):
            timing = record_process(
                [str(executables["boundary"])], directory, f"run-{index:03}"
            )
            allocations = record_process(
                [str(executables["boundary_allocations"])],
                directory,
                f"allocation-run-{index:03}",
            )
            samples.append(parse_sample(timing + allocations))
            print(f"boundary run {index + 1}/{runs}", file=sys.stderr)
        final_stamp = source_stamp()
        if final_stamp != stamp:
            raise RuntimeError("source state changed during collection; results are incomplete")
        final_hashes = {
            name: hashlib.sha256(path.read_bytes()).hexdigest()
            for name, path in executables.items()
        }
        if final_hashes != metadata["binary_sha256"]:
            raise RuntimeError("a benchmark executable changed during collection")
        report = summarize(samples)
        report["compiler_selection"] = "explicit"
        write_json(directory / "summary.json", report)
        write_json(directory / "complete.json", {
            "source": final_stamp,
            "binary_sha256": final_hashes,
            "completed_utc": datetime.datetime.now(datetime.UTC).isoformat(),
        })
        return report
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        write_json(directory / "failure.json", {"error": str(error)})
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    run = commands.add_parser("run", help="build once, then launch independent benchmark processes")
    run.add_argument("--output", type=Path, required=True, help="new output directory")
    run.add_argument("--runs", type=int, default=10)
    run.add_argument("--toolchain", default="1.98.0")
    report = commands.add_parser("report", help="recompute statistics from saved raw stdout")
    report.add_argument("directory", type=Path)
    args = parser.parse_args()
    try:
        result = (
            collect(args.output.resolve(), args.runs, args.toolchain)
            if args.action == "run"
            else read_report(args.directory)
        )
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"boundary: {error}\n")
    print(json.dumps(result, indent=2, allow_nan=False))


if __name__ == "__main__":
    main()
