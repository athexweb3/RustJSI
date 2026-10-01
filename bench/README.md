# Boundary measurements

Run on macOS with Python 3.11+, rustup and the Command Line Tools installed:

```sh
python3 -B bench/boundary.py run --output bench/results/boundary-001
python3 -B bench/boundary.py report bench/results/boundary-001
```

For a collection intended to contribute to a controlled-machine comparison,
require confirmed AC power and disabled Low Power Mode explicitly:

```sh
python3 -B bench/boundary.py run \
  --output bench/results/boundary-ac-001 \
  --require-ac-power \
  --require-low-power-mode-off
```

The optional checks fail closed when macOS cannot confirm AC power or that Low
Power Mode is off. Each check runs once before the release build and again
before benchmark processes start. They do not pin CPU frequency, verify thermal
stability, exclude background work, or themselves qualify a performance gate.

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
| `direct_jsc_js_call` | Direct JSC call to a prepared JavaScript function, including scalar argument creation and strict result validation |
| `rustjsi_common_js_call` | Common-backend call to the same prepared JavaScript function with equivalent engine work and RustJSI safety checks |
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
The two scoped JavaScript call workloads alternate order across that mirrored
twelve-process block. Each callback permutation is paired once with each call
order. This balances first and second position for the call pair, but does not
remove thermal, scheduler, cache or frequency effects.
The three entry workloads follow their own mirrored complete six-permutation
schedule, and the direct/common scalar pair alternates across the same
twelve-process block. `entry_position_effects` and `scalar_position_effects`
report the mean by observed position for those workloads. The allocator probe
receives the selected entry order too and writes it separately; the collector
rejects a process when its allocation and timing entry orders disagree.
Callback, scoped-call and scalar results are checked against `42` before and
after each timed workload. These checks do not validate every timed iteration.
The direct functions have explicit roots outside the timer; RAII releases each
root before its context, including if a validation assertion unwinds.
macOS CI checks successful benchmark execution, not timing thresholds.

For each callback, scoped-call and entry workload, the timing executable also
records the four-decimal mean of every 1,000-operation batch. A separate
executable with a counting global allocator snapshots successful Rust
allocation, reallocation and deallocation activity around the equivalent
1,000,000 operations. It runs the callback and scoped-call workloads in the
same selected order as the timing executable, and runs the entry workloads in
the matching selected entry order.
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

Each block-latency metric also reports `per_process_batch_quantiles`. It first
calculates p50, p95 and p99 from the 1,000 contiguous batch means in each
process, then summarizes those independent process-local quantile estimates
with deterministic confidence intervals. This makes the uncertainty scope
explicit: the intervals describe variation across process-local block
quantiles. They are not confidence intervals for the pooled block quantile and
are not individual-call or individual-entry tail intervals.

`js_call_batch_latency` applies the same block-mean model to the direct and
common scoped-call pair. `js_call_position_effects`,
`entry_position_effects`, and `scalar_position_effects` report each workload
by observed position. Neither section is an individual-call tail model.

`measurement_calibration` reports the timer-pair and empty-batch distributions,
plus the timer-pair cost amortized over one 1,000-operation measured block.
These controls expose the harness floor; they are not assumed to be additive
with engine work and are never subtracted from a workload metric.

`rust_allocator_activity` summarizes per-process counter totals and their mean
per operation for the callback, scoped-call and entry workloads. Zero is a valid
observation. It supports a narrowly scoped zero-Rust-allocation claim only for
the named measured region and build; it is not evidence of zero engine
allocation or zero payload copies.

`all_run_mean_cv_at_most_5_percent` is a noise diagnostic, not a performance
pass. The process-level metric means, medians, paired ratios, and Rust allocator
counters include deterministic 95% percentile-bootstrap confidence intervals.
Each resample draws whole benchmark processes, preserving the direct/common
pairing inside a ratio. These intervals quantify sampling uncertainty for this
collector; they do not correct uncontrolled power, thermal, scheduler, or
engine variation, and they do not qualify a performance gate. Block-mean and
calibration distributions intentionally have no confidence intervals because
their observations are correlated within a process. The separate process-local
block-quantile summaries use one estimate from each process. No individual-call
distribution exists here, so `individual_call_p99` remains absent. JavaScriptCore
allocation, payload-copy, and regression-gate work also remains open.

Separate processes do not isolate CPU frequency, thermal state, OS scheduling,
shared caches or background work. Callback, scoped-call, entry and scalar
workload order is counterbalanced, but that does not remove nonlinear thermal,
scheduler, cache or between-process effects. Use an otherwise idle machine and
record power/thermal conditions separately. Do not run collection alongside
builds or test suites.

The metadata records whether AC power and Low Power Mode-off requirements were
selected. A report rejects an artifact whose declared requirements conflict with
either recorded endpoint snapshot: immediately before the first benchmark
process and immediately after the last. The checks do not continuously observe
power state or prove that it remained unchanged during all benchmark processes.

Metadata records selected compiler/profile/JSC environment overrides, not the
entire environment. It also records the available macOS hardware model, memory
size and CPU topology, the active power source (`pmset -g ps`) and the power
settings currently applied (`pmset -g`) in a structured pre-process
`host_environment` record and a matching post-process completion record.
Endpoint snapshots can detect a changed condition at either end, but not a
transient change during collection.
Missing, failing, slow or undecodable optional system commands are labelled
unavailable rather than guessed or treated as fatal. Available output is stored
as reported, apart from surrounding whitespace and the masking below, and is
not validated. Names of processes holding sleep assertions (everything after
"prevented by" on a line), battery identifiers and the hibernation image path
are masked. The reader rejects a record with missing, extra or reworded fields,
and every report repeats the record with its limits. This is collection context
for comparing artifacts; it does not pin CPU/frequency, measure thermal state,
exclude background work, or qualify a performance gate. The reader accepts only
the current artifact schema, so artifacts from earlier schemas must be
re-reported with the revision that collected them. JSON artifacts and reports
use sorted keys, so re-reporting the same raw output produces byte-identical
JSON. The source fingerprint covers tracked and non-ignored untracked files;
ignored/generated inputs, symlink targets, external Cargo configuration and
system engine internals are not fully captured. OS build and SDK version
identify the system-JSC tuple, not a WebKit source revision. The binary hash
identifies the built artifact but does not prove reproducibility.

Test the runner without JSC:

```sh
python3 -B -m unittest discover -s bench -p 'test_*.py'
```

## External-buffer accounting

On macOS, collect ownership and payload-copy evidence for the direct JSC
external-buffer route:

```sh
python3 -B bench/external_buffer_accounting.py \
  --output bench/results/external-buffer-accounting-001
```

The collector builds one probe with an explicitly selected compiler, then runs
0-byte, 1-byte, 4 KiB, and 1 MiB exact `Box<[u8]>` transfers. Each run checks
the JavaScript mutation path, removes JS reachability, forces bounded GC work,
and verifies that the registered deleter receives the original allocation.

For a non-empty payload, `payload_copy_bytes: 0` is reported only if JSC's
immediate backing pointer equals the transferred allocation. That is evidence
for this named constructor and system-JSC tuple. It does not include wrapper,
GC, cache, scheduling, or platform costs. Empty payloads have no meaningful
pointer identity and report no payload-copy number. Rust allocator fields cover
only Rust-visible allocation in the transfer window; JavaScriptCore and system
allocation remain `unmeasured`.

The output directory must be outside the repository or Git-ignored. It stores
compiler/source/SDK/binary metadata, raw output per payload size, parsed
records, and a completion record after source and binary postflight checks.
This is an ownership and data-movement diagnostic, not a latency benchmark or
a general zero-copy claim for other engines or buffer modes.

## External-buffer construction profile

On macOS, collect matched construction and publication samples for the owned
external-buffer route:

```sh
python3 -B bench/external_buffer_profile.py \
  --output bench/results/external-buffer-profile-001
```

The collector builds the executable once with an explicitly selected compiler.
It runs 0-byte, 1-byte, 4 KiB, and 64 KiB payloads in twelve separate
processes per size. Process order alternates direct JSC then RustJSI, and the
reverse. Every process includes twelve warmup blocks and records 128 measured
block means. A block transfers eight pre-allocated boxed payloads.

The timer covers external `ArrayBuffer` construction and publication to a
global property. The direct path calls the system JSC constructor and property
API. The RustJSI path calls `Context::install_external_buffer`, including its
active-state, quota, observation, backing-origin, exception, and publication
work. Payload allocation, context startup, property cleanup, runtime teardown,
and deallocator verification are outside the timer.

After each block, both paths clear their profile properties and destroy the
matching context. The run fails unless every registered owner has been
reclaimed at teardown. This is deliberate: system JSC may keep an otherwise
unreachable external object alive across bounded explicit collection in a
long-lived context, so the profile does not treat immediate GC reclamation as a
per-block latency requirement.

The artifact keeps raw stdout and stderr for every process, the binary hash,
compiler/source/SDK metadata, parsed block means, and a completion record after
source and binary postflight checks. The parser rejects duplicate, incomplete,
unknown, non-finite, or wrong-order output. Results remain raw evidence, not a
performance threshold. They do not measure steady-state long-lived-context
behavior, GC latency, engine allocation, total allocation, payload copying, or
application throughput. The ownership probe above remains the only evidence
for its named backing-origin observation.

For each payload size, the collector also runs a separate allocation probe once.
It snapshots the Rust global allocator only around the same eight construction
and publication operations used by the timing profile. Payload allocation,
runtime/context setup, property cleanup, context teardown, and deallocator work
stay outside that snapshot. Its record is written to `allocation-records.json`;
the raw process output and a separate executable hash are retained alongside the
timing artifact. These counters do not observe JavaScriptCore, system, or total
process allocation, and they are not a payload-copy measurement.

## Native sampling profiles

The `js_calls` benchmark compares prepared scalar calls to a JavaScript
function through direct JSC and common backend scopes:

```sh
cargo bench -p rustjsi-backend-jsc --features experimental-jsc --bench js_calls
RUSTJSI_JS_CALL_ORDER=common,direct cargo bench -p rustjsi-backend-jsc --features experimental-jsc --bench js_calls
```

Both paths create two numbers, check callability, use the global receiver,
classify the result, and perform a strict numeric read per call. Setup and
teardown are excluded. The common path additionally validates identities and
reserves result capacity. Each workload validates 42 before and after timing.
Results are single process means, without tail or allocation measurements.
They cannot be subtracted from the host-callback benchmark to isolate dispatch
cost because its API and result-reading workload differ.

The independent boundary collector includes the same shared workloads with
alternating order, 1,000-operation block means, paired process ratios and a
separate Rust allocation probe. The standalone commands remain quick smoke
comparisons rather than saved evidence collections.

On macOS, capture one long-running callback workload with debug symbols and
Apple's sampling profiler:

```sh
python3 -B bench/callback_profile.py \
  --output bench/results/callback-profile-rustjsi-001 \
  --workload rustjsi
```

`--workload` also accepts `prepared` and `reused`.

To sample the matched scoped-call pair instead, select the `js-call` target:

```sh
python3 -B bench/callback_profile.py \
  --target js-call \
  --output bench/results/js-call-profile-common-001 \
  --workload common
```

The scoped-call target accepts `common` and `direct`. It runs the same prepared
pure-JavaScript scalar function used by the scoped-call comparator. `common`
goes through the RustJSI backend contract; `direct` calls JavaScriptCore
directly. The target prints an aggregate duration only to show that the sample
attached to a live workload. It is not timing evidence.

The runner pins the requested Rust compiler paths, disables compiler wrappers,
builds the selected profiling target with bench-profile debug information, then
attaches `/usr/bin/sample` by PID. The default 200 million callback calls and
50 million scoped calls keep their targets live for the five-second sample;
iterations, duration and sample interval are configurable.

The output directory follows the same outside-or-Git-ignored rule as the
boundary collector. It retains the selected target, workload, source/compiler/
binary metadata, raw build and workload output, the stack profile, and a
completion record. Compare captures from the same source and host tuple.
Sample counts are statistical attribution, not additive nanoseconds or a
latency distribution, and collapsed output also contains non-workload threads.
