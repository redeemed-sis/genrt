# Automated testing

## Gates

The common setup dispatcher installs the supported host packages and `rustup`
when it is not already available. Rust version and components remain sourced
from `rust-toolchain.toml`. Run the same workflow locally from the repository
root:

```bash
./scripts/setup/install-deps.sh
cargo xtask check
cargo xtask test-aarch64
cargo xtask ci
```

Supported host platforms, package lists, and manual setup are documented in
[`development/setup.md`](development/setup.md).

`check` covers formatting, xtask tests and clippy, userspace compilation,
initramfs verification, AArch64 linking, and the `.boot.text` post-link
invariant. It applies the production marker policy to both the kernel ELF and
the production initramfs; test image builders use a separate test-only entry
point. `ci` is the canonical local and hosted merge gate.

Cases can be listed or selected explicitly:

```bash
cargo xtask test-aarch64 --list
cargo xtask test-aarch64 --case kernel-contract
cargo xtask test-aarch64 --case shell-contract --timeout-secs 30
cargo xtask test-aarch64 --case smp-boot
```

## Test layers

`kernel-contract` and `user-fault` use kernels compiled with scenario-specific
`qemu-test` features. Their coordinators test CPU0 identity, per-CPU scheduler
context ownership, scheduler/timing/mailbox behavior, and exact user-fault
classification.

`smp-boot` uses a dedicated test kernel and a four-CPU DTB/QEMU topology. CPU0
starts each secondary through PSCI; the coordinator validates unique hardware
identities and bootstrap stacks, stable logical bindings, CPU0-only device
  routing, all registered scheduler contexts online with local idle ownership,
  pinned CPU1/CPU2 barrier workers, timer round-robin of two non-yielding CPU1
  workers, targeted scheduler-IPI preemption of a busy CPU and wake from CPU3
  idle, a controlled single-edge coalescing batch, cross-CPU mailbox/join
  completion, routing isolation, absence of polling quanta, and explicit local
  timer deadlines on every CPU. It does not claim normal device IRQ or
  userspace participation on secondary CPUs.

`userspace-contract` and `shell-contract` use the production kernel with a
test-only initramfs. `/init` is a supervisor that performs
`fork -> execve -> waitpid`; only the supervisor can report terminal success.
The shell case uses the production shell ELF and test-only helper commands. A
host-generated nonce verifies UART RX, blocking stdin wakeup, argv propagation,
and command execution without depending on prompts or human-readable output.
It also feeds arrow keys, Tab, backspace, and partial/unknown escape sequences
through paced UART input. Test helpers assert resulting argv/cwd for history
recall/edit/draft restoration, duplicate suppression, ring overflow, completion,
and recovery after line/argument overflow. Completion-only `/bin` aliases,
a directory, and a text file exist solely in the controlled shell image and
carry explicit test provenance. The product invocation remains the exact
manifest-declared shell; the fixture names do not become production contracts.

`smp-userspace-contract` also uses the production kernel, with a four-CPU DTB
and a dedicated supervisor. It checks one-shot child placement, replacement and
reset, preservation across bounded fork exhaustion, exact self and child
affinity masks, affinity preservation across exec, remote exec, remote fault,
cross-CPU wait/reap, and production `taskset` placement. The single-core
`userspace-contract` checks `/init` on CPU0, invalid affinity arguments,
`taskset` parsing and lifecycle failures, and exact product invocations. It
also verifies normalized monotonic observation and resolution, relative and
absolute sleeps, conversion errors, peer progress, and `/bin/sleep` argument
handling. The SMP suite executes an absolute sleep on CPU2 and checks that
wakeup preserves affinity.

The public monotonic clock reads CNTPCT at EL0, without a syscall. Tests bracket
raw clock observations with fast readings on CPU0 and CPU2, compare resolution,
check normalized monotonic values, and isolate an invalid non-NULL fast-output
pointer in a child. Raw clock syscall tests still require EFAULT for invalid
buffers. Clock host tests substitute counter/frequency values; guest build
checks require counter instructions and forbid SVC or undefined helpers in the
complete clock object.

`kernel-contract` runs a sole EL0 probe with 32 sleeps per process lifetime.
Test-only counters require 32 direct idle handoffs and exactly two hardware
TTBR0 writes (initial activation and terminal detach), with full frame return
after each of three reaps. Pure host tests cover fixed expired-buffer transfer,
direct-wake wait consumption, stale tokens, and existing/remote peers. Existing
wait races, RR, preemption, process isolation, and SMP sleeps remain gates.

`syscall-boundaries` exercises raw, unaligned input and output buffers across
mapped page boundaries, rejects readonly and partially out-of-range buffers
before any write, and verifies unknown-syscall child termination without
breaking the parent. Kernel host tests exhaust word alignments and byte tails,
bounded copy sizes, page crossings, and destination guards. Use
`cargo xtask test-aarch64 --profile release` to run contracts with optimized
userspace and support-library code; debug uses `-O0`, release uses `-O2`.

The `support-library` API case exercises the shared freestanding string/memory,
formatting, option parsing, and decimal conversion interfaces in a child ELF.
The supervisor accepts its result only after wait/reap. Host checks also inject
short, zero-progress, and failed writes to test output behavior independently
of UART timing. Protocol formatting uses the shared implementation, but the
protocol and markers themselves are never part of the production archive.
`tests/user/shell_input.c` additionally checks the production editor's pure
state transitions, exact line/history bounds, CRLF, escape resynchronization,
completion prefixes and capacity edges, plus deterministic mixed-byte input.
These host tests run in `cargo xtask check` without AArch64 syscall transport.
`tests/user/rtbench.c` executes the actual utility measurement and reporting
with controlled public clock observations, sleep transport, and wrapped output.
It checks exact elapsed
TET/release latency, strict overrun equality, unchanged absolute releases when
multiple periods behind, final sleep and timespec boundaries, all 10000 rows,
workload results, checked sums/horizons, CLI rejection before clock observation,
malformed/backwards timestamps, and syscall/output failures. No production
test conditionals or protocol markers are required.

Dynamic production-program checks are declared in
`tests/qemu/program-contracts.toml`. xtask validates a one-to-one mapping to
`user/c/programs.toml` and generates the exact case/path/argv/exit table
compiled into each supervisor. A passing protocol case therefore names the
specific executable invocation that produced it.

Production boot logs and release initramfs contents are not test APIs. Reaching
the userspace supervisor proves the boot, scheduler, initramfs, ELF, EL0, and
syscall path. A failure before that boundary is reported as protocol timeout,
with the complete serial log retained for diagnosis.

## rtbench validation

Run the host/build gate and virtual-board contracts from the repository root:

```bash
cargo xtask check
cargo xtask test-aarch64 --case userspace-contract
cargo xtask test-aarch64 --case smp-userspace-contract
cargo xtask ci
```

The exact product invocation uses `rtbench -p 1000000 -n 3 -o 0`; the userspace
supervisor also waits for positive-workload, malformed-CLI, and arithmetic
failure exits. The SMP supervisor waits for `taskset 1 rtbench` and a separate
CPU1-pinned production exec whose immutable child affinity it can query.
Assertions use GTRT records and child exits, not report prose or a jitter bound.
The host test covers 10000 samples without flooding QEMU UART.

### Raspberry Pi procedure (user-performed)

QEMU contracts or a Pi emulator smoke run do not establish real-Pi timing
behavior. The user-reported hardware results are recorded separately below.
Use the [Pi runtime build/boot procedure](../arch/aarch64/platform/raspberrypi-3b-plus/README.md)
to boot the production kernel and initramfs on the user's Raspberry Pi 3B+.
Confirm logical CPU1 is online before the pinned run. At the production shell:

```text
rtbench -p 1000000 -n 100 -o 50000
taskset 1 rtbench -p 1000000 -n 100 -o 50000
```

Retain each UART transcript with the build identity, board/firmware setup,
clock resolution, and competing workload. Expect 100 zero-based STEP rows,
then a summary with `result=5000000`; report output begins only after sampling
and the final non-overrun sleep. Compare release latency, elapsed TET, and
overrun distributions across repeated runs and CPU placement. Record observed
values, not a fabricated expected latency or hardware performance guarantee.
Payload operation counts do not guarantee a duration, and TET includes
preemption. Physical-board measurements remain the user's responsibility.

### Raspberry Pi hardware result (user-reported)

On 2026-10-03 the user reported this run on a physical Raspberry Pi 3B+:

```text
taskset 1 rtbench -p 15000 -n 300 -o 10
```

The supplied final summary reports 300 steps on the requested 15 us timeline,
10 payload additions per step, `overruns=0`, and `result=3000`.

| Metric | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| Release latency | 7612 | 12656 |
| Elapsed TET | 1973 | 2135 |
| Overrun duration | 0 | 0 |

The user also reported this maximum-capacity run on CPU3:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 30
```

The supplied summary reports all 10000 supported steps at the same 15000 ns
period, 30 payload additions per step, `overruns=0`, and `result=300000`.

| Metric | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| Release latency | 7153 | 12188 |
| Elapsed TET | 2460 | 2656 |
| Overrun duration | 0 | 0 |

These are user-provided representative functional hardware results, not
agent-executed runs or worst-case latency guarantees. The complete per-step
UART transcript, exact build identity, firmware version, competing load, and
clock resolution were not supplied; the summaries alone are not a reproducible
performance baseline. These values are not QEMU measurements.

### Periodic wakeup optimization comparison

The optimization request supplied a later pre-change CPU3 release-latency
summary for `taskset 3 rtbench -p 15000 -n 10000 -o 30`: average 6909 ns,
maximum 11771 ns. TET and overrun counts for that specific baseline were not
supplied; do not combine them with the earlier runs above.

After rebuilding the Pi FIT, repeat both commands on the same board/firmware
and record the complete SUMMARY, image identity, and competing load:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 30
taskset 3 rtbench -p 15000 -n 10000 -o 0
```

| Metric (`-o 30`) | Before | After |
| --- | ---: | --- |
| release_latency_avg_ns | 6909 | Not measured |
| release_latency_max_ns | 11771 | Not measured |
| tet_avg_ns | Not supplied | Not measured |
| tet_max_ns | Not supplied | Not measured |
| overruns | Not supplied | Not measured |

The `-o 0` run has no recorded before/after summary yet. It separates baseline
wakeup/observation overhead from payload work; it is not a zero-cost reference.
No hardware after-result is claimed by host tests, QEMU contracts, or FIT
construction. The EL0 clock fast path also removes clock syscalls from measured
TET/observation overhead, so document that change when comparing summaries.

### Short-period hardware trace (user-reported)

On 2026-10-05 the user supplied all 1000 STEP rows for a physical Pi run:

```text
taskset 3 rtbench -p 8000 -n 1000 -o 30
```

The rows reproduce the supplied SUMMARY: release average 4878 ns, maximum
7156 ns, TET average 238 ns, maximum 313 ns, zero overruns, and `result=30000`.
Sorted release samples give minimum 219 ns, median 4938 ns, nearest-rank p95
6542 ns, and p99 6708 ns. The maximum is STEP 1, following the first sleep call;
the maximum over STEP 2..999 is 6792 ns. This is a different run from the earlier
8000 ns summary with release average 4850 ns, maximum 9448 ns, and one overrun;
no per-step trace for that earlier maximum was supplied.

The dominant pattern is a repeated ramp and reset, not isolated random spikes:

| STEP | Release latency (ns) |
| ---: | ---: |
| 3 | 3500 |
| 30 | 5208 |
| 57 | 6708 |
| 58 | 271 |
| 59 | 3260 |
| 113 | 6625 |
| 114 | 240 |

There are 21 such resets after STEP 2. Since `S[i] = R[0] + i*8000 + L[i]`,
STEP 57 to 58 spans `8000 + 271 - 6708 = 1563` ns between start observations.
The later resets similarly span 1562..1615 ns, whereas typical ramp intervals
are 8020..8125 ns. A slightly-too-long full iteration can accumulate lateness
on the fixed timeline; a shorter sleep-return path lets the loop catch up.

This is consistent with the source: a deadline already past at the syscall
check returns without blocking. A deadline still future at that check may
expire while wait registration, timer programming, and context handoff execute
with IRQ delivery masked. The IRQ then waits for the handoff's exception return.
At an 8000 ns period, a 6708 ns release delay and 209 ns TET leave only 1083 ns
for clock conversion, sample bookkeeping, and submitting the next sleep.
The trace contains no kernel phase timestamps, so this path attribution is an
inference, not a measured breakdown of IRQ entry, lock wait, or return costs.

`overrun_ns` checks payload end against the next release; it does not include
the subsequent sleep submission or prove that the call blocked. Low-latency
catch-up samples therefore do not establish a 219 ns timer-wakeup latency.
Output is buffered until measurement ends, so STEP printing itself cannot
cause this ramp. Absolute counter rounding does not accumulate deadline drift.

Source inspection found two avoidable costs in sleep submission: a software
128-bit division for ordinary counter frequencies, and timer reprogramming
when cancelling a nonexistent RR quantum. Checked native-width conversion now
handles products representable in `u64`, retaining the wide fallback and exact
ceiling semantics. Missing cancellations no longer program the timer; remote
and in-progress IRQ cancellations still defer programming to the owner.
Host tests cover conversion equivalence and timer-write behavior. These changes
follow the supplied trace; the subsequent user-reported results are below.

### Final recorded hardware profiles (user-reported)

The user subsequently supplied full physical-Pi traces after the conversion and
missing-cancellation changes, with the same sample and operation counts:

```text
taskset 3 rtbench -p 8000 -n 1000 -o 30
taskset 3 rtbench -p 15000 -n 1000 -o 30
```

All figures below are nanoseconds except the overrun count. Percentiles use
nearest rank over all 1000 samples, including startup and catch-up samples.

| Metric | Earlier 8000 ns trace | Follow-up 8000 ns | Follow-up 15000 ns |
| --- | ---: | ---: | ---: |
| Release minimum | 219 | 218 | 312 |
| Release average | 4878 | 4687 | 3259 |
| Release median | 4938 | 4885 | 3281 |
| Release p95 | 6542 | 6593 | 3333 |
| Release p99 | 6708 | 6750 | 3333 |
| Release maximum | 7156 | 11010 | 5104 |
| TET average | 238 | 238 | 243 |
| TET maximum | 313 | 365 | 365 |
| Overrun average | 0 | 3 | 0 |
| Overrun maximum | 0 | 3323 | 0 |
| Overrun count | 0 | 1 | 0 |

Integer averages match rtbench's truncated SUMMARY values; medians average
the two middle sorted samples. These are the final recorded observations for
this change set, not a kernel phase profile or a certified minimum period.
The supplied complete transcripts have these SHA-256 identities:

```text
earlier 8000 ns:  396b0ae7549e884bfc3b7d0a4966d00dcefe4f18910a6e5e73e9f546b7142ab5
follow-up 8000:   f02714af21ec0d713b17ff3018aeff9e54b0d905648e87491ce060dbcde4fae3
follow-up 15000:  66e12e0eb14835892d7269db9851fab548169d5cafa8f5b839bc4687d56a29d5
```

These identify the UART evidence, not the flashed FIT or its build inputs.

Each follow-up run reports `result=30000`. The sole 8000 ns overrun is STEP 1:
`release_latency_ns=11010`, `tet_ns=313`, and `overrun_ns=3323`. Both follow-up
release maxima occur at STEP 1. They remain part of the maximum and overrun
statistics; startup placement alone does not identify their kernel cost or
justify discarding them from a worst-case assessment.

The 8000 ns ramp persists. It has 68 low-latency resets after STEP 2, compared
with 21 in the earlier trace. Median reconstructed start-to-start spacing
within ramps is 8281 ns, compared with 8073 ns previously; the intended period
remains 8000 ns. Here a reset is a drop greater than 1000 ns after STEP 2;
ramp spacing uses adjacent samples after STEP 3 whose latencies both exceed
2000 ns and whose latency change exceeds -1000 ns. These are descriptive
filters, not evidence that an individual syscall blocked. The small decrease
in the mixed overall average is not evidence
of a reliably faster IRQ wakeup. Median and upper percentiles barely improve or
increase, and the full maximum increases in this run. The traces do not prove
whether code layout, clock policy, competing activity, or another unrecorded
condition accounts for the changed ramp spacing.

At 15000 ns there is no comparable ramp: all STEP 3..999 release samples fall
between 3177 and 3333 ns, a 156 ns observed range over 997 samples. This supports
the full-round-trip throughput explanation at 8000 ns rather than recurring
multi-microsecond random timer jitter. It does not measure individual sleep
submission, IRQ entry, scheduler acquisition, or exception-return costs. The
15000 ns control has 1000 steps, so it is not the exact 10000-step after-run
requested by the earlier comparison table.

The next cost attribution needs bounded, buffered phase timestamps for sleep
entry, timer publication, completed blocking handoff, IRQ entry, scheduler
acquisition, and return to EL0, with early-return counts retained separately.
Keep UART output outside the measured interval and distinguish instrumented
diagnostic builds from the production timing baseline. No such phase profile
is present in these traces, and no additional runtime change is inferred from
them alone.

Retain full STEP output, FIT hash, firmware, and competing workload for repeated
matched runs. The supplied traces lack those build and environment identifiers
and are not worst-case timing certifications. Hardware results above are
user-provided, not agent-executed or QEMU timing measurements.

## Guest protocol

Machine records start with ASCII Record Separator (`0x1e`):

```text
<RS>GTRT/1|producer|000001|READY|suite
<RS>GTRT/1|producer|000002|CASE_START|case
<RS>GTRT/1|producer|000003|PASS|case
<RS>GTRT/1|producer|000004|DONE|suite|PASS
```

Supported events are `READY`, `CASE_START`, `PASS`, `TERMINAL`, `FAIL`, `DONE`,
and `ABORT`. Sequence numbers are validated independently per producer. Only
the configured supervisor may emit `READY`, `TERMINAL`, and the single terminal
`DONE PASS`.
Malformed records, sequence gaps, unknown versions, `FAIL`, and `ABORT` fail a
case immediately. Case records follow `unseen -> CASE_START -> PASS`; duplicate
or reordered records fail unless a future DSL action explicitly declares an
unordered concurrent group. All ordinary UART lines are ignored by protocol
evaluation.

TOML steps send input, expect structured records, or issue a nonce challenge.
Every case automatically waits for supervisor `READY` before its steps.
Ordinary cases then require terminal `DONE PASS`. Terminal power cases require
an operation-specific `TERMINAL` record plus natural QEMU poweroff or a fresh
supervisor epoch after reset; the intent record alone never declares success.

Inside test kernels, new cases use the declarative
`kernel::test_support::scenario` runner. A suite macro maps stable protocol IDs
to ordinary rustdoc-documented scenario functions and generates the coordinator
thread. Returning `Ok(())` emits `PASS`; returning a stable error reason emits
`FAIL`. Scenario code therefore does not emit lifecycle records or manually
invoke each case. `smp-boot` uses this pattern now; existing coordinators remain
on their current form until separately migrated.

## Runner behavior

QEMU uses `-display none -monitor none -nic none -serial stdio`. Serial and
stderr are drained concurrently; the parser receives serial chunks through a
bounded channel and handles records split across reads. Step and case deadlines
plus an aggregate QEMU-runtime budget are enforced, and QEMU is always
terminated and reaped. Compilation and image preparation are intentionally
outside this runtime budget and remain governed by the outer CI job timeout.
Setup failures abort the command directly and may occur before a per-case
`result.json` exists; hosted CI still preserves command output for that class
of infrastructure failure.

Each case writes `serial.log`, `qemu-stderr.log`, and `result.json` below
`target/test-results/<case>/`; the suite writes `summary.json`. Failure output
includes the serial tail and full log path.

Test protocol implementations carry the retained
`GENRT_TEST_ARTIFACT_V1` payload. Test userspace keeps it in a dedicated
`.genrt.test_marker` ELF section; the test kernel linker folds the test-only
input section into high-linked rodata so production ELF metadata does not name
the section. Production verification rejects marker payloads/metadata and
protocol code, and also enforces provenance. Test files normally live below the
reserved `/.__genrt_test__/` namespace; shell completion also needs explicitly
marked `/bin` fixtures in its test image. All carry fixture/supervisor provenance
in generated manifests; ordinary names such as `/test` and `/fixtures` remain
available to products.
