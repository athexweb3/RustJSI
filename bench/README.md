# Boundary measurements

Run on macOS with Python 3.11+, rustup and the Command Line Tools installed:

```sh
python3 -B bench/boundary.py run --output bench/results/boundary-001
python3 -B bench/boundary.py report bench/results/boundary-001
```

The runner builds `boundary` once with Rust 1.98.0, then launches ten separate
processes. `--toolchain` selects another installed toolchain; `--runs` accepts
10–1000. Each workload has 10,000 warmup iterations and 1,000,000 measured
iterations per process. The three entry workloads divide those iterations into
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
| `rustjsi_experimental` | RustJSI call preparation, JSC callback dispatch, checked addition and result capture |
| `host_gate_admit_and_exit` | Entry accounting guard creation and drop, without engine entry |
| `jsc_common_empty_entry` | Empty authorized common-backend entry, including maintenance checks |
| `jsc_foreign_common_empty_entry` | Empty non-owning attachment entry, including host-context validation and maintenance checks |
| `direct_jsc_scalar` | Direct number creation, strict type check and number read |
| `rustjsi_common_scalar` | Common-backend number creation, strict type check and number read |

The callback comparison is a lower-bound comparison, not equal setup work:
the direct path reuses arguments while RustJSI translates them per call.
The scalar comparison has closer operation parity, but still excludes host
entry from its timer. Neither comparison measures application throughput.
Callback and scalar results are checked against `42` before and after each
timed workload. These checks do not validate every timed iteration. The direct
callback function has an explicit root outside the timer; RAII releases the
root before its context, including if a validation assertion unwinds.
macOS CI checks successful benchmark execution, not timing thresholds.

For each entry workload, the benchmark also records the four-decimal mean of
every 1,000-operation batch. A counting global allocator snapshots successful
Rust allocation, reallocation and deallocation activity around the same
1,000,000 operations. The batch-sample vector is reserved before the allocator
snapshot. The counter covers Rust allocations made by the benchmark and linked
Rust code in that region. It does not observe JavaScriptCore, Objective-C,
system-framework or other foreign allocator activity.

## Reading the report

The primary metrics remain one mean per process. The report gives their mean,
median, range and sample coefficient of variation (`sample standard deviation /
mean`) across processes. Ratios are calculated within each process before being
summarized. Primary times have two decimal places of nanosecond precision.

`entry_batch_latency` pools the equal-sized, four-decimal batch means and
reports p50, p95 and p99 using the nearest-rank method. These are quantiles of
contiguous 1,000-operation block means, not individual entry latencies. Batching
amortizes timestamp reads enough to expose scheduler and frequency disturbances
without placing a timer around every nanosecond-scale entry. It can hide
single-operation spikes inside a block.

`rust_allocator_activity` summarizes per-process counter totals and their mean
per entry. Zero is a valid observation. It supports a narrowly scoped
zero-Rust-allocation claim only for the named timed region and build; it is not
evidence of zero engine allocation or zero payload copies.

`all_run_mean_cv_at_most_5_percent` is a noise diagnostic, not a performance
pass. It is not the variability of independently estimated medians. No
individual-call distribution exists here, so `individual_call_p99` remains
absent. JavaScriptCore allocation, payload-copy, confidence-interval and
regression-gate work also remains open.

Separate processes do not isolate CPU frequency, thermal state, OS scheduling,
shared caches or background work. Workloads currently run in a fixed order;
order bias is unmeasured. Use an otherwise idle machine and record power/thermal
conditions separately. Do not run collection alongside builds or test suites.

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
