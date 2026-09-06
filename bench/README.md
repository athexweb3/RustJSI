# Boundary measurements

Run on macOS with Python 3.11+, rustup and the Command Line Tools installed:

```sh
python3 -B bench/boundary.py run --output bench/results/boundary-001
python3 -B bench/boundary.py report bench/results/boundary-001
```

The runner builds the timing and allocation-probe executables once with Rust
1.98.0, then launches each executable in twelve separate process pairs.
`--toolchain` selects another installed toolchain. `--runs` accepts multiples
of twelve from 12 through 996.
Each workload has 10,000 warmup iterations and 1,000,000 measured iterations
per process. The callback and entry workloads divide those iterations into
1,000 contiguous batches of 1,000 operations. Startup, runtime creation and
compilation are outside the workload timers.

The collector passes absolute `RUSTC` and `RUSTDOC` paths from `rustup which`
to Cargo through both direct and configuration environment keys, pins nested
rustup selection, and disables compiler wrappers for that build. This does not change
the calling shell. Selecting Cargo alone is insufficient when PATH contains
a different `rustc`. Reports from older collectors without explicit compiler
selection are marked `unverified`; their recorded `rustc` label may not identify
the compiler that produced the benchmark binary.

The output directory must not exist and must be outside the repository or
Git-ignored. It contains Cargo output, raw stdout and
stderr for every process, hardware/OS/compiler metadata, a binary hash, and
summary statistics. A completion record is written only after all samples
validate and source/binary checks match. Failed collections retain diagnostic
files but cannot produce a report. `report` recalculates from raw stdout, not
the saved summary. Results under `bench/results/` are ignored by Git.

## What is measured

| Metric | Timed work |
| --- | --- |
| `direct_jsc_lower_bound` | Direct JSC callback call with pre-created number arguments; callback checks types and adds them |
| `direct_jsc_prepared_call` | Direct JSC number-argument creation plus the same checked callback and exception capture on every call |
| `rustjsi_experimental` | RustJSI call preparation, JSC callback dispatch, checked addition and result capture |
| `host_gate_admit_and_exit` | Entry accounting guard creation and drop, without engine entry |
| `jsc_common_empty_entry` | Empty authorized common-backend entry, including maintenance checks |
| `jsc_foreign_common_empty_entry` | Empty non-owning attachment entry, including host-context validation and maintenance checks |
| `direct_jsc_scalar` | Direct number creation, strict type check and number read |
| `rustjsi_common_scalar` | Common-backend number creation, strict type check and number read |

The reused-argument callback remains a lower-work baseline, not equal setup
work. The prepared direct path creates both JSC number arguments per call,
performs the same strict callback reads and captures the exception output. It
does not perform RustJSI handle identity, registry and local-budget checks;
those are part of the RustJSI boundary cost being compared. The metric name
`lower_bound` describes workload construction, not a guarantee that every noisy
run will report a smaller number. The scalar comparison has closer operation
parity, but still excludes host entry from its timer. None of the comparisons
measures application throughput.

The three callback workloads run once in each of their six possible orders,
followed by the same six permutations in reverse order. Additional runs repeat
this twelve-process schedule. Each timing process writes its selected order to
raw stdout; the collector validates the exact schedule before reporting.
`callback_position_effects` reports each workload separately in first, second
and third position, plus the spread between those position means. Runtime,
context and function construction and warmup remain outside each workload
timer. Complete permutations balance position and immediate carryover. Pairing
each permutation at mirrored points in the block reduces first-order
chronological drift; it does not eliminate nonlinear thermal, scheduler or
between-process effects.
Callback and scalar results are checked against `42` before and after each
timed workload. These checks do not validate every timed iteration. The direct
callback function has an explicit root outside the timer; RAII releases the
root before its context, including if a validation assertion unwinds.
macOS CI checks successful benchmark execution, not timing thresholds.

For each callback and entry workload, the timing executable also records the
four-decimal mean of every 1,000-operation batch. A separate executable with a
counting global allocator snapshots successful Rust allocation, reallocation
and deallocation activity around the equivalent 1,000,000 operations. It runs
the callback workloads in the same selected order as the timing executable.
Keeping the probe separate prevents its atomics from changing the timed
workloads. The counter covers Rust allocations made by the probe and linked
Rust code in that region. It does not observe JavaScriptCore, Objective-C,
system-framework or other foreign allocator activity.

After all timed workloads finish, the timing executable records two diagnostic
controls. `calibration_timer_pair` measures back-to-back `Instant` reads in the
same process. `calibration_empty_batch` runs the same 1,000-by-1,000 batching
harness with only a Rust `black_box` operation. Running the controls last keeps
them from changing the preceding workload measurements. It also means they
sample a later interval rather than the exact scheduler state of each workload
block.

## Reading the report

The primary metrics remain one mean per process. The report gives their mean,
median, range and sample coefficient of variation (`sample standard deviation /
mean`) across processes. Ratios are calculated within each process before being
summarized. Primary times have two decimal places of nanosecond precision.

`callback_batch_latency` and `entry_batch_latency` pool the equal-sized,
four-decimal batch means and report p50, p95 and p99 using the nearest-rank
method. These are quantiles of contiguous 1,000-operation block means, not
individual call or entry latencies. Batching amortizes timestamp reads enough
to expose scheduler and frequency disturbances without placing a timer around
every operation. It can hide single-operation spikes inside a block.

`measurement_calibration` reports the timer-pair and empty-batch distributions,
plus the timer-pair cost amortized over one 1,000-operation measured block.
These controls expose the harness floor; they are not assumed to be additive
with engine work and are never subtracted from a workload metric.

`rust_allocator_activity` summarizes per-process counter totals and their mean
per operation for the callback and entry workloads. Zero is a valid
observation. It supports a narrowly scoped zero-Rust-allocation claim only for
the named measured region and build; it is not evidence of zero engine
allocation or zero payload copies.

`all_run_mean_cv_at_most_5_percent` is a noise diagnostic, not a performance
pass. It is not the variability of independently estimated medians. No
individual-call distribution exists here, so `individual_call_p99` remains
absent. JavaScriptCore allocation, payload-copy, confidence-interval and
regression-gate work also remains open.

Separate processes do not isolate CPU frequency, thermal state, OS scheduling,
shared caches or background work. Callback workload order is counterbalanced;
entry and scalar workloads still run in a fixed order after the callback group.
Their order bias is unmeasured. Use an otherwise idle machine and record
power/thermal conditions separately. Do not run collection alongside builds or
test suites.

Metadata records selected compiler/profile/JSC environment overrides, not the
entire environment. The source fingerprint covers tracked and non-ignored
untracked files; ignored/generated inputs, symlink targets, external Cargo
configuration and system engine internals are not fully captured. OS build and
SDK version identify the system-JSC tuple, not a WebKit source revision. The
binary hash identifies the built artifact but does not prove reproducibility.

Test the runner without JSC:

```sh
python3 -B -m unittest discover -s bench -p 'test_*.py'
```

## Callback profiling

On macOS, capture one long-running callback workload with debug symbols and
Apple's sampling profiler:

```sh
python3 -B bench/callback_profile.py \
  --output bench/results/callback-profile-rustjsi-001 \
  --workload rustjsi
```

`--workload` also accepts `prepared` and `reused`. The runner pins the requested
Rust compiler paths, disables compiler wrappers, builds the profiling target
with bench-profile debug information, then attaches `/usr/bin/sample` by PID.
The default 200 million calls keep the target live for the five-second sample;
iterations, duration and sample interval are configurable.

The new output directory follows the same outside-or-Git-ignored rule as the
boundary collector. It retains source/compiler/binary metadata, raw build and
workload output, the stack profile, and a completion record. Compare captures
from the same source and host tuple. Sample counts are statistical attribution,
not additive nanoseconds or a latency distribution, and collapsed output also
contains non-workload threads.
