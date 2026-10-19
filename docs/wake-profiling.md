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

Use `-F 0:4` to observe `svc-entry -> sleep-handoff` directly, including its
paired maximum rather than a sum of phase maxima. Focused mode suppresses
unselected Rust timestamp reads and writes, not hook calls, leaf locking,
record initialization, or exact-wait bookkeeping. Fixed assembly entry probes
also remain. This is reduced timestamp collection, not a two-hook kernel or
an uninstrumented submission baseline.

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

The agreed target is the observed maximum of submission plus IRQ-to-EL0,
excluding intentional timer waiting, below 5000 ns on an agreed physical run.
Release latency alone is not that metric. The latest user-reported results
after the CPU-frequency configuration change are below; older references and
reverted experiments follow separately.

### Higher CPU frequency (user-reported)

After raising the configured ARM CPU frequency, the user supplied this
physical Raspberry Pi 3B+ run on CPU3, with `wake-profile` and full collection:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 300 -P
```

| SUMMARY metric | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| Release latency | 2894 | 9219 |
| TET | 573 | 625 |
| Overrun | 0 | 0 |

All 10000 cycles are complete idle wakes, with zero early returns, incomplete
or lost records, zero overruns, and `result=3000000`. The physical counter is
19200000 Hz; this is not the ARM CPU frequency. Single-START calibration
reports a 0 ns counter pair and 208 ns trace write; neither is subtracted.

Every interval below has 10000 observations except the final next-STEP row,
which has 9999. Totals and deadline references overlap stage rows; maxima
must not be added.

| Idle interval | Average (ns) | Maximum (ns) |
| --- | ---: | ---: |
| svc-entry -> validated | 718 | 3020 |
| validated -> timer-published | 933 | 4114 |
| timer-published -> wait-committed | 583 | 1979 |
| wait-committed -> sleep-handoff | 560 | 1250 |
| sleep-handoff -> irq-entry | 8626 | 8802 |
| irq-entry -> acknowledge | 260 | 520 |
| acknowledge -> expired-extracted | 497 | 2343 |
| expired-extracted -> scheduler-acquired | 225 | 625 |
| scheduler-acquired -> wake-selected | 569 | 3333 |
| wake-selected -> context-restored | 442 | 625 |
| context-restored -> timer-rearmed | 260 | 677 |
| timer-rearmed -> interrupt-completed | 135 | 156 |
| interrupt-completed -> return-to-user | 322 | 468 |
| return-to-user -> first EL0 observation | 131 | 364 |
| context-restored -> return-to-user (total) | 719 | 1302 |
| scheduler-acquired -> timer-rearmed (total) | 1274 | 4635 |
| deadline -> irq-entry (signed reference) | 0 | 52 |
| deadline -> first EL0 observation (total) | 2849 | 9166 |
| first EL0 observation -> next STEP | 44 | 729 |

Sequential interval means estimate 2794 ns for submission and 2841 ns from
IRQ entry to first EL0 observation, about 5.64 us combined without suspension.
This sums rounded interval means, not paired per-cycle maxima, and retains
instrumentation overhead. It does not establish the sub-5-us maximum target.

The user subsequently supplied ten runs at a 6000 ns period from a build
without `wake-profile`: release averages 1319..1323 ns, overall release
maximum 5614 ns, and one 187 ns overrun across 100000 steps. The
[complete ordinary-build results](testing.md#raspberry-pi-hardware-result-user-reported)
are separate from this diagnostic profile. Different periods and builds do
not isolate instrumentation overhead or establish an ordinary-build round-trip
maximum.

These are user-reported hardware results, not agent-executed or QEMU timing
measurements. Actual runtime ARM CPU frequency, exact FIT/build identities,
firmware version, and competing workload were not measured or supplied with
these reports; no controlled speedup ratio or worst-case bound is claimed.

### Earlier measurements

The current narrow follow-up removes trace-only counter reads from timer queue
maintenance, samples RR time only when a new quantum is needed, removes unused
architecture timer snapshots, and reuses the current SVC's staged profiler
owner. It preserves locks, wait identity, and probe layout. No hardware result
is inferred from its host or QEMU checks. The user subsequently reported
10000 complete idle cycles, no early/lost/incomplete cycles, no overruns,
and `result=3000000` for the same CPU3 command below. Release latency averaged
6075 ns with a 12344 ns maximum; TET averaged 1134 ns with a 1198 ns maximum.
Sequential interval means estimate 6256 ns for submission and 5921 ns from
IRQ entry to first EL0 observation: 12177 ns excluding suspension, versus
12262 ns in the committed reference. This roughly 0.7% difference is not
controlled attribution or an observed round-trip maximum. START calibration
was 156 ns for the counter pair and 52 ns for the trace write, not subtracted.
Exact deployed FIT identity and environment details were not supplied.

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

### Follow-up Pi results (user-reported)

The user supplied these later full-profile runs of the same command on CPU3,
with a 19200000 Hz counter. They belong to the now-reverted CPU-local scheduler,
profiler, usercopy-cache, and timer experiment. The refinement precomputed
runtime scheduler facades and removed repeated per-lock IRQ setup. Both
experimental runs retained 9999 complete records, zero incomplete/lost records,
and `result=3000000`.

All times are nanoseconds; average/maximum pairs are reported directly.
Submission and IRQ work estimates sum only sequential idle-path interval means.

| Metric | Committed reference | Reverted experiment | Reverted refinement |
| --- | ---: | ---: | ---: |
| Release latency average / maximum | 6169 / 12917 | 9283 / 17657 | 6063 / 14271 |
| TET average / maximum | 1134 / 1198 | 1162 / 1198 | 1137 / 1198 |
| Overrun duration average / maximum | 0 / 0 | 0 / 3854 | 0 / 417 |
| Overrun count | 0 | 1 | 1 |
| Idle / early cycles | 9999 / 1 | 9661 / 338 | 9999 / 0 |
| SVC entry -> sleep handoff, mean estimate | 6257 | 6892 | 6627 |
| IRQ entry -> first EL0 observation, mean estimate | 6005 | 6368 | 5911 |
| Combined work excluding suspension, mean estimate | 12262 | 13260 | 12538 |
| Deadline -> IRQ entry, signed average / maximum | 52 / 1145 | 3085 / 6875 | 53 / 2500 |
| Deadline -> first EL0 observation, average / maximum | 6065 / 12812 | 9463 / 17552 | 5972 / 14114 |

Counter-pair and trace-write START observations are both 208 ns in the
regressed run and both 52 ns in the latest run; neither is subtracted.
Release average decreased 34.7%, but the dominant difference is the mean
deadline-to-IRQ delay returning near one counter tick. Estimated sequential
work decreased only 722 ns. The mixed release means also cover different
idle/early populations, so they are not equivalent round-trip costs.

The refinement did not improve the committed reference overall: its estimated
mean work was 2.3% higher, release maximum was higher, and one overrun remained.
The uncommitted implementation and its ownership changes were reverted at the
user's request. These observations are retained as historical evidence, not
current-source results or proof of an independently isolated cause.

Summed means are approximate because interval means are truncated. They exclude
the suspended interval and assembly outside the documented probes; maxima
cannot be reconstructed by adding row maxima. The instrumented mean estimate
alone already exceeds the sub-5-us target, but does not establish the normal
kernel's timing or its observed maximum. Exact deployed FIT/build identities,
clock policy, firmware, and competing workload were not supplied. These are
user-reported hardware observations, not controlled per-change attribution or
agent-executed/QEMU performance measurements.

### Wait layout and dispatch hardware check (agent-executed)

On 2026-10-17 the agent loaded both diagnostic FITs through picocom/YMODEM
into RAM on the same Raspberry Pi 3B+, using U-Boot 2026.07, 115200 baud,
and the 19200000 Hz counter. Each image ran the CPU3 command above three
times, in order, without changing microSD or the saved firmware environment.
The follow-up keeps active wait identity stationary across phase changes and
outlines other syscall handlers away from clock sleep. Release disassembly
shows no token-copy `memcpy` in wait commit and a 48-byte clock-sleep dispatcher
frame, down from 1344 bytes. The syscall ABI and owner locks are unchanged.

| FIT / run | Release average (ns) | Release maximum (ns) | Estimated mean work excluding suspension (ns) | Overruns |
| --- | ---: | ---: | ---: | ---: |
| Before / 1 | 6045 | 15052 | 12181 | 1 |
| Before / 2 | 6043 | 13229 | 12176 | 0 |
| Before / 3 | 6039 | 13125 | 12157 | 0 |
| After / 1 | 5960 | 14635 | 11358 | 1 |
| After / 2 | 6026 | 12396 | 11425 | 0 |
| After / 3 | 5957 | 12240 | 11384 | 0 |

Estimated submission means changed from 6272..6287 ns to 5568..5601 ns.
Every retained record was complete, none was lost, and each run returned
`result=3000000`. First-run overruns remain included. These ordered runs do
not isolate the two changes or measure a worst-case bound; CPU clock policy
and temperature were not captured. Work estimates sum sequential idle means,
not row maxima, and exclude assembly outside the probes. The sub-5-us target
is not established. Local UART evidence is retained under
`target/hardware/raspberrypi-3b-plus/wait-dispatch/` (not versioned).

The uploaded FIT SHA-256 identities are:

```text
before: aa6df4030760e3cb76b5127773222b972992ab949d20d07258f4ed65488b469c
after:  64026fffcac1d74a5e4ba0355bf79cff352bbf8bb8da50b0303600d2ada3d9a7
```

The ordinary follow-up FIT, rebuilt without `wake-profile`, also reached the
shell through the same RAM-only UART boot. Three runs without `-P` reported
release average/maximum pairs of 2691/7760, 2693/6927, and 2690/8385 ns, with
zero overruns and `result=3000000`. CPU1 echo, CPU2 sleep, and initramfs reads
also completed. Its FIT SHA-256 is
`690ccb8a8f4f32cd51b31ad965fd8144f5f0ea69875a6b3a66a480da9bc760bd`.
No matching ordinary before-FIT was tested, so these results do not quantify
the changes' uninstrumented speedup. Release latency still excludes submission;
neither ordinary SUMMARY nor the full-profile table provides the observed
maximum of combined work. UART was disconnected with the new ordinary kernel
left at its shell; no persistent boot files or environment were changed.

### Focused submission check (agent-executed)

On 2026-10-18 the agent reloaded the same diagnostic FIT (`64026fff...a3d9a7`
above) through UART into RAM. Firmware identified Raspberry Pi 3 Model B Plus
Rev 1.3, U-Boot 2026.07; the counter remained 19200000 Hz. No runtime source
changes were needed. On one boot, three alternating pairs ran:

```text
taskset 3 rtbench -p 15000 -n 10000 -o 300 -P
taskset 3 rtbench -p 15000 -n 10000 -o 300 -F 0:4
```

All times below are nanoseconds. Full-mode means sum its four sequential
submission rows; focused maxima measure the actual paired timestamps.

| Pair | Full estimated submission mean | Focused submission mean | Focused submission maximum | Focused idle samples |
| --- | ---: | ---: | ---: | ---: |
| 1 | 5569 | 5450 | 12395 | 10000 |
| 2 | 5575 | 5449 | 12916 | 9999 |
| 3 | 5566 | 5453 | 12447 | 10000 |

All retained records were complete, with no lost/incomplete records. Every
run returned `result=3000000`; focused runs had no overruns. The first full
run had one overrun (1614 ns), retained in the evidence. The second focused
run had one early return, which has no sleep-handoff endpoint and contributes
no submission pair. Focused release average/maximum pairs were 5379/11823,
5328/12813, and 5383/11927 ns, not the submission intervals above.

The observed reduction in submission means was only 113..126 ns, about 2%.
This removes intermediate Rust timestamp reads/writes, not all diagnostic
overhead: hook calls, locks, record initialization, identity correlation,
and assembly probes remain. It does not measure ordinary-kernel submission,
attribute the paired maxima to a cause or iteration, or establish a sub-5-us
round-trip bound. CPU clock policy and temperature were not captured.

Three subsequent runs on this diagnostic image without `-P`/`-F` reported
release average/maximum pairs of 4731/10521, 4805/9636, and 4785/10052 ns,
without overruns. The diagnostic hooks remain even with collection inactive;
these are not feature-disabled kernel measurements.

UART evidence is local and not versioned:
`target/hardware/raspberrypi-3b-plus/submission-focused-20261018/serial.log`.
Its SHA-256 is
`6bfe0328ca761e29fadc7d9e58b0deb2a754a1e4ffdc30f21eec1c3d4f55cbca`.
UART was disconnected at the diagnostic kernel's shell; microSD and saved
U-Boot environment were unchanged. The focused host test and release QEMU
`wake-profile-contract` passed separately from the physical measurements.

### Retired counter-sleep experiment (agent-executed)

These are historical measurements of the withdrawn native-sleep trial, not
the current CLI or a post-removal performance baseline. The native syscall,
public wrapper, conversion helper, and `-C` option were removed by
[ADR-0060](../memory/decisions/ADR-0060-retire-native-counter-sleep-interface.md).

On 2026-10-18 the same physical Pi/U-Boot and CPU3 ran three repetitions of
`rtbench -p 15000 -n 10000 -o 300`, with full `-P` and focused `-F 0:4`
collection. Native trial runs used `-C`; all results equal 3000000. Values below
are nanoseconds, with ranges across the three runs, not confidence intervals.

| Diagnostic image / transport | Focused submission mean | Focused paired maximum | Full release mean | Full release maximum | Full overruns (total) |
| --- | ---: | ---: | ---: | ---: | ---: |
| Before / POSIX | 5455..5484 | 12812..13125 | 5959..5984 | 12656..13229 | 0 |
| Batched publication + AT / POSIX | 5298..5331 | 13281..14947 | 5781..5821 | 13906..16927 | 3 |
| Same image / native `-C` | 4881..4894 | 11770..12083 | 5790..5797 | 12187..12604 | 0 |
| Software-walk control / POSIX | 5331..5394 | 13541..14583 | 5897..5906 | 13125..15469 | 2 |

Native admission removed about 0.4 us from focused submission versus POSIX on
the same image. Nanosecond conversion still happens in userspace before SVC.
The combined POSIX mean improves slightly, but its maximum does not. The
software-walk control retains long tails too: these runs establish neither an
independent timing win for AT nor the cause of the tails. Focused maxima cover
submission only; do not add row maxima or claim a sub-5-us round-trip maximum.
No record is lost or incomplete; an overrun can skip a sleep and reduce the
recorded count to 9999. Instrumented results are not ordinary-kernel timings.

A RAM-only U-Boot probe queried firmware tag `0x00030047` (ARM clock ID 3)
before and after these diagnostic boots: both successful responses returned
`0x23c44000`, or 600000000 Hz. The
[firmware interface](https://github.com/raspberrypi/firmware/wiki/Mailbox-property-interface#get-clock-rate-measured)
defines this as the measured, rather than requested, clock rate. This is not a
frequency trace during rtbench. No clock, voltage, firmware, microSD file, or
saved U-Boot environment was changed. The counter remained 19200000 Hz.

Local evidence is under
`target/hardware/raspberrypi-3b-plus/sleep-next-20261018/`, including `serial.log`,
the read-only clock probe, and these measured FIT identities:

```text
before.itb:        64026fffcac1d74a5e4ba0355bf79cff352bbf8bb8da50b0303600d2ada3d9a7
after.itb:         3f3ebc1c815b1a5b3d203cf6cf4cc11997927181281f0426753431301744274b
software-walk.itb: 1b5a662eedcfa8d89520f58bbe8bf74786f907ce6d9295017673c8ad3289e409
```

The ordinary follow-up FIT (without `wake-profile`) was booted separately.
Three alternating POSIX/native pairs at the same period/count/workload gave:

| Transport | Release means (ns) | Release maxima (ns) | Overruns |
| --- | --- | --- | ---: |
| POSIX | 2645, 2683, 2651 | 9115, 7448, 7448 | 0 |
| Native `-C` | 2669, 2645, 2643 | 7604, 7292, 7604 | 0 |

These are release observations, not submission or round-trip profiles; no
ordinary before-FIT was measured in this session. CPU1 echo and CPU2 sleep also
passed. The ordinary FIT SHA-256 is
`a2578f48890afadb888f22ea988338604af484facc6ee2310f1d4aacdf8f8c43`.
The complete UART log SHA-256 is
`e83164b741382aee22073c449237292fac8a33fdb713b2a0bfbb76709c7067a8`.
The board was left at the ordinary kernel shell and picocom was closed.

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
