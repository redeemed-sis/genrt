# Sleep/wakeup phase profiling

`wake-profile` is an opt-in diagnostic feature for absolute CLOCK_MONOTONIC
sleeps. It is not a general tracing service and does not change scheduling or
POSIX sleep semantics. Unprofiled rtbench summaries do not constitute a kernel
phase profile.

## Build and collect

From the repository root, build an isolated diagnostic QEMU kernel:

```bash
cargo xtask run-aarch64 --cpus 4 --profile release --wake-profile
```

At the shell, collect all eligible sleep cycles on CPU3:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 30 -P
```

`-P` enables a full trace. `-F start:end` selects a numeric phase pair instead;
it is mutually exclusive with `-P`. `-n` sets both the benchmark step count and
the maximum retained cycle count (1..10000). Each step issues at most one sleep,
including the final sleep, so this capacity covers every eligible call. Catch-up
iterations that issue no sleep are not sleep cycles and use no record slots.

rtbench starts before measurement, stores userspace correlation observations
without output, stops after the final sleep, then reads frozen records one at
a time. After SUMMARY output it prints a human-readable report directly
in the console. No log file, raw UART export, or host analyzer is required.
Add `-v` to also print each STEP before the summary; the phase report does not
depend on verbose output and is available in either mode.

## Read the report

The report identifies the CPU, mode, counter frequency, and recorded, complete,
incomplete, lost, and early cycle counts. Separate tables for idle, generic,
and early-return paths show each observed interval's count, average, and maximum
in nanoseconds. Full mode shows the causal sleep/IRQ/resume intervals; focused
mode shows only the requested phase pair. Both include available deadline,
first-EL0-observation, and next-STEP correlations. The final sleep has no next
STEP and does not contribute a fabricated zero to that interval.

Full-mode idle reports follow the execution path in numbered sections:

```text
Flow: SVC -> block -> idle -> timer IRQ -> resume -> EL0
1. Sleep submission
2. Suspended interval
3. Timer IRQ: deadline completion
4. Resume selection
5. Originating IRQ: timer rearm and completion
6. Return to EL0 / next STEP
7. Totals and deadline references (overlap stages; do not add)
```

Each section retains `Interval | Count | Average (ns) | Maximum (ns)` columns.
For example, `scheduler-acquired -> timer-rearmed` and
`context-restored -> return-to-user` overlap the idle stage rows and appear only
under totals. Context restoration prepares a return frame, not actual EL0
execution; the final register restoration and ERET follow the return hook.
Generic selection may occur after the originating IRQ completes: its groups
describe separate branches, not a strict timeline. Early returns have no
blocking/IRQ sections. Focused reports show the selected phase pair, available
EL0 observations, and separate deadline references. Empty sections are omitted
and the remaining sections are numbered consecutively.

Missing phases are omitted, not zero-filled. Incomplete records contribute
only intervals whose endpoints were actually observed. Cross-branch focused
intervals and deadline-to-IRQ observations are signed. A negative cross-branch
value is an ordering relationship, not a negative execution cost. Intervals
overlap and must not be summed. Calibration values are single START-time
observations, not overhead distributions, and are not subtracted.

Before printing, rtbench validates the schema, owner generation, exact wait
sequence, presence masks, kernel/user deadlines, causal ordering, completion
counts, and userspace correlations. It handles short reads and rejects invalid
data, arithmetic overflow, and read/output errors. Aggregates use fixed storage;
reporting allocates no heap and performs no collection-time formatting.
There is no raw export option, host report command, JSON output, or percentile
calculation.

### One sleep cycle

The report covers two kernel entries, not one uninterrupted syscall execution:

```text
rtbench running in EL0
    |
    | SVC: clock_nanosleep()
    v
kernel prepares the blocked thread and an idle return context
    |
    | exception return into idle
    v
idle on the same CPU
    |
    | physical timer IRQ
    v
kernel selects rtbench and prepares its EL0 return context
    |
    | finish IRQ handling, restore registers, ERET
    v
rtbench continues after sleep in EL0
```

The wakeup does not resume a suspended `sys_clock_nanosleep()` Rust call.
The IRQ return frame is replaced with the saved user context, so execution
returns directly to the userspace instruction after SVC. See the
[sleep handoff](../kernel/src/sched/sleep.rs) and
[wait commit](../kernel/src/sched/wait.rs) implementations.

### Going to sleep

Numbers below are stable probe IDs from [Probe boundaries](#probe-boundaries),
not the consecutively numbered report sections. Each arrow corresponds to
one interval row; descriptions summarize work between observations, including
instrumentation overhead, not the exclusive cost of a single function.

```text
0 svc-entry
    | decode syscall, copy timespec, validate and convert deadline
1 validated
    | prepare exact WaitToken, insert timed event, program nearest timer
2 timer-published
    | commit the wait and select a replacement context
3 wait-committed
    | save rtbench's context, prepare idle context and address-space handoff
4 sleep-handoff
    | finish SVC return, enter idle, eventually take the timer IRQ
5 irq-entry
```

The first four arrows appear under `Sleep submission`. The final arrow is
`Suspended interval`. `timer-published` means the timer was configured, not
that it has fired. `sleep-handoff` marks completed handoff preparation, not
the actual exception return or the start of WFI.

Consequently, `sleep-handoff -> irq-entry` includes remaining SVC-return work
and any idle execution. It is not purely time spent waiting in WFI. The
deadline can already have passed during submission; an IRQ may be pending
before idle receives it.

### Direct idle wake

For a full-mode `idle` record, the return sequence is linear:

```text
5 irq-entry
    | controller acknowledges the interrupt
6 acknowledge
    | time subsystem extracts the exact expired wait
7 expired-extracted
    | acquire scheduler ownership for expired-event dispatch
8 scheduler-acquired
    | complete the wait and commit rtbench as Running/current
9 wake-selected
    | prepare its saved return frame and activate its address space
10 context-restored
    | finish timer rearm bookkeeping
11 timer-rearmed
    | finish the interrupt-controller end operation
12 interrupt-completed
    | complete the remaining kernel path to the final return hook
13 return-to-user
    | final register restoration, ERET, userspace wrapper and counter probe
   first EL0 observation
    | userspace bookkeeping and observation of the next iteration's start
   next STEP
```

These arrows map to the report sections as follows:

| Section | Consecutive intervals |
| --- | --- |
| Timer IRQ: deadline completion | `5 -> 6 -> 7 -> 8` |
| Resume selection | `8 -> 9 -> 10` |
| Originating IRQ: timer rearm and completion | `10 -> 11 -> 12` |
| Return to EL0 / next STEP | `12 -> 13 -> first EL0 -> next STEP` |

`context-restored` means the live return frame and address space are prepared;
rtbench is not executing yet. `return-to-user` is still a kernel observation
before restoring the general-purpose registers, not the ERET instruction.
The first EL0 observation is taken after the public sleep wrapper returns.
The final sleep has no next STEP, so that last interval has one fewer sample
when every iteration sleeps. See [timer dispatch](../kernel/src/time.rs),
[scheduler handling](../kernel/src/sched/preempt.rs), and
[controller dispatch](../arch/aarch64/src/interrupts.rs).

### Totals and deadline references

Rows under `Totals and deadline references` reuse endpoints from the stage
rows. They are alternative views of the same work, not additional procedures.
For the idle path, the following identities hold for one complete record in
counter ticks:

```text
scheduler-acquired -> timer-rearmed = 8 -> 9 -> 10 -> 11
context-restored -> return-to-user  = 10 -> 11 -> 12 -> 13
deadline -> first EL0 observation   = deadline -> 5 -> ... -> 13 -> EL0
```

Do not add a total to its constituent stage rows. Even for consecutive rows,
printed integer averages may differ slightly from the corresponding total
because each interval is converted and rounded separately. With missing
observations, row populations can differ. Maxima must not be added: each row's
worst sample may belong to a different sleep cycle.

`deadline` is the requested point on the counter timeline, not a function or
probe executed between report sections. It can fall inside sleep submission.
Thus `deadline -> irq-entry` may overlap the tail of submission; it does not
isolate hardware timer delivery latency. Its endpoint is the software IRQ
entry probe, not the hardware assertion of the interrupt.

### Early and generic paths

An early return is detected when the deadline has already passed at syscall
admission. There is no blocked wait or associated timer IRQ for this record:

```text
0 svc-entry -> 1 validated -> 13 return-to-user -> first EL0 -> next STEP
```

Only `Sleep submission`, `Return to EL0 / next STEP`, and available totals
remain. Focused mode intersects its selected pair with these existing probes;
it does not invent missing IRQ observations.

For a generic wake, readiness does not imply immediate selection. For example,
selection can be deferred beyond completion of the originating timer IRQ:

```text
originating timer IRQ: 5 -> 6 -> 7 -> 8 -> 11 -> 12
                                    |
                                    +-> ready ... later selection
                                                  9 -> 10 -> 13 -> EL0
```

These are logical branches of one sleep episode, not concurrent execution on
two CPUs. Selection can also happen within the original IRQ; probe IDs alone
do not order `9/10` against `11/12`. The generic report keeps originating IRQ
completion separate from eventual selection and return. In particular,
`interrupt-completed -> return-to-user` is a total, not the idle path's short
consecutive return stage. A focused `9 -> 11` interval can be negative when
selection occurs after the original rearm; it is a signed relationship between
branches, not negative execution time.

Focused output retains `Selected phase pair`, available EL0 correlations,
and separate deadline references. Its numbering changes when empty groups
are omitted; use the phase names and IDs, not section numbers, to identify
the measured endpoints.

## Functional verification

Functional verification, including full and focused rtbench reports, is opt-in:

```bash
cargo xtask test-aarch64 --case wake-profile-contract --profile release \
  --artifacts-dir target/wake-profile/test-results
```

The contract checks successful production rtbench exits through its supervisor.
Invalid data or failed reporting causes a nonzero child exit. Exact table values
and negative cases are covered by host tests, not UART prose assertions.

Ordinary CI retains its production/test compositions. To exercise the complete
matrix with instrumentation, use `cargo xtask test-aarch64 --wake-profile`.
These QEMU results are not physical timing evidence.

## Sessions and export

`<genrt/profile.h>` defines the experimental control/read ABI and versioned
export structures: a 104-byte header followed by 160-byte cycle records. The
4 MiB per-session budget permits 1..26213 cycles (capacity includes cycles,
not individual phase observations).
Allocation and initialization finish before collection becomes visible.
Allocation failure leaves an existing frozen session intact. Only the exact
owning thread on its immutable CPU can control or retrieve results.

START rejects an active or foreign session. STOP freezes records, including
incomplete ones. DISCARD releases an owned session. Termination and faults
detach and release the owner's storage; fork does not inherit a session.
Reads require STOP, accept byte offsets, return zero at EOF, and respect
MAX_USER_COPY. The feature-disabled control/read interface returns ENOSYS.

Records retain raw counter values, exact wait identity, absolute deadline,
phase-presence information, and wake-path classification. Export metadata
includes counter frequency, selected mode/pair, capacity, and incomplete/lost
counts. Missing fields are not interpreted as zero-latency phases.

## Probe boundaries

| Number | Probe |
| ---: | --- |
| 0 | SVC entry after preserving x0/x1 |
| 1 | Absolute deadline validated and converted |
| 2 | Exact wait published and nearest timer programmed |
| 3 | Blocking wait committed |
| 4 | Outgoing sleep context handoff finished |
| 5 | Originating IRQ entry after preserving x0/x1 |
| 6 | Platform acknowledgement finished |
| 7 | Exact expired deadline extracted |
| 8 | Scheduler acquired for expired-event dispatch |
| 9 | Owner committed as Running/current in the scheduler transition |
| 10 | Owner context and address space restored |
| 11 | Originating timer IRQ rearm bookkeeping finished |
| 12 | Originating interrupt controller completion finished |
| 13 | Final EL0 return hook before GPR restoration |

For example, `-F 5:8` isolates IRQ entry through scheduler acquisition.
Phase numbers are identities, not a universal timeline: on a delayed generic
resume, phases 11 and 12 precede the eventual phases 9 and 10. A focused pair
that spans these branches can have a negative signed interval; it must not be
silently interpreted as a positive cost. Early returns have only phases 0,
1 and 13, intersected with the selected pair.

Selection is observed immediately after the transition commits Running/current,
before invariant validation, outgoing context save, and incoming context restore.
Thus selection-to-restoration includes intervening scheduler bookkeeping and
the outgoing context work, not only incoming register restoration. The exit
path also includes any outgoing address-space detach before incoming activation.

Entry probes follow preservation of the first exception registers, not the
physical exception assertion. Acknowledgement is observed after the platform
controller returns its token. Context-restoration probes include the selected
address-space handoff. The return probe precedes the final GPR restore and
ERET; the userspace probe follows return from the public sleep wrapper.

The requested counter deadline and effective programmed timer comparison are
distinct. Timer assertion itself cannot be timestamped by software. Likewise,
late generic selection may resume the thread through a subsequent IRQ; it
must retain the original wait's wake evidence rather than substitute an
unrelated interrupt. Early-return calls are classified separately.

## Hardware comparison

The current runtime uses one scheduler transaction for EL0 sleep submission,
VM-owned CPU-local usercopy lookup, and fixed AArch64 full-frame copies without
generic memory-copy calls. Probe identities and timing endpoints are unchanged;
idle context saving and userspace pointer validation remain intact. See
[ADR-0058](../memory/decisions/ADR-0058-bounded-sleep-and-vm-owned-usercopy.md).
The user-reported reference below follows these changes. It is not a controlled
per-change attribution; repeat matched normal/full/focused runs before assigning
individual costs or quantifying instrumentation overhead.

### Recorded Pi reference (user-reported)

Reported on 2026-10-16 from a physical Raspberry Pi 3B+, CPU3, full profiling,
19200000 Hz counter, after the ADR-0058 changes:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 300 -P
```

| SUMMARY metric | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| Release latency | 6169 | 12917 |
| TET | 1134 | 1198 |
| Overrun | 0 | 0 |

All 10000 cycles are complete: 9999 idle wakes, one early return, zero
incomplete/lost records, zero overruns, and `result=3000000`. START calibration
reports a 52 ns counter pair and 208 ns trace write; neither is subtracted.

Idle-path intervals below each have 9999 observations, except the final row
which has 9998 because the last sleep has no next STEP. Totals overlap stage
rows; do not add totals or row maxima.

| Idle interval | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| svc-entry -> validated | 1813 | 5052 |
| validated -> timer-published | 1833 | 4739 |
| timer-published -> wait-committed | 1267 | 2343 |
| wait-committed -> sleep-handoff | 1344 | 1927 |
| sleep-handoff -> irq-entry | 1301 | 5000 |
| irq-entry -> acknowledge | 479 | 729 |
| acknowledge -> expired-extracted | 1025 | 2447 |
| expired-extracted -> scheduler-acquired | 464 | 885 |
| scheduler-acquired -> wake-selected | 1373 | 3958 |
| wake-selected -> context-restored | 901 | 1197 |
| context-restored -> timer-rearmed | 576 | 885 |
| timer-rearmed -> interrupt-completed | 292 | 312 |
| interrupt-completed -> return-to-user | 626 | 989 |
| return-to-user -> first EL0 observation | 269 | 312 |
| context-restored -> return-to-user (total) | 1495 | 2135 |
| scheduler-acquired -> timer-rearmed (total) | 2852 | 6041 |
| deadline -> irq-entry (signed reference) | 52 | 1145 |
| deadline -> first EL0 observation (total) | 6065 | 12812 |
| first EL0 observation -> next STEP | 103 | 313 |

The single early return reports 2031 ns SVC-to-validation, 677 ns
validation-to-return, 312 ns return-to-first-EL0, 104 ns first-EL0-to-next-STEP,
and 2239 ns deadline-to-first-EL0 (each average equals its maximum).

Compared with the previous supplied full run of the same command, release
latency average decreased from 9103 to 6169 ns (32.2%); its maximum decreased
from 13282 to 12917 ns. Idle deadline-to-IRQ average changed from 2912 to 52 ns,
idle deadline-to-first-EL0 from 9227 to 6065 ns, and early returns from 321 to
one. These aggregates do not correlate individual phase maxima or establish
which optimization caused each difference.

This is user-supplied instrumented hardware evidence, not an agent-executed run,
a worst-case bound, or a complete round-trip measurement. Exact FIT/build
identity, firmware, clock policy, and competing workload were not supplied.
The [sub-5-us round-trip goal](roadmap/hardening.md#high-value-architecture-work)
is a separate open target, not a reinterpretation of release latency.

### Matched collection

Build the normal and diagnostic Raspberry Pi images separately:

```bash
cargo xtask build-raspberrypi-3b-plus
cargo xtask build-raspberrypi-3b-plus --wake-profile
```

Use the [Pi bootstrap procedure](../arch/aarch64/platform/raspberrypi-3b-plus/README.md)
to deploy the selected FIT. Retain its hash, build profile, firmware, clock
frequency, CPU placement, capacity, competing workload, and complete output.
On the same board and under matched conditions, compare the normal image with
the diagnostic image without collection, full collection, and focused
collection. Keep `-o 300` when comparing with the recorded reference; the
8000 ns runs explore a different period:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 300
taskset 3 rtbench -p 8000 -n 10000 -o 300
taskset 3 rtbench -p 15000 -n 10000 -o 300 -P
taskset 3 rtbench -p 8000 -n 10000 -o 300 -P
taskset 3 rtbench -p 15000 -n 10000 -o 300 -F 5:8
taskset 3 rtbench -p 8000 -n 10000 -o 300 -F 5:8
```

Counter-read and trace-write calibration reports local probe costs. It does
not capture every cache, branch, or contention effect and must not be blindly
subtracted from measured latencies. Use matched runs to quantify complete
instrumentation overhead. QEMU distributions are functional diagnostics only.

A full-mode hardware report is an instrumented observation, not an
uninstrumented latency baseline or a measurement of total profiler overhead.
Matched runs are still needed to quantify that overhead. Earlier rtbench
summaries without phase collection must not be presented as phase profiles.

See [ADR-0056](../memory/decisions/ADR-0056-bounded-sleep-wakeup-phase-profiling.md)
for collection ownership and
[ADR-0057](../memory/decisions/ADR-0057-in-process-sleep-profile-reports.md)
for console reporting.
