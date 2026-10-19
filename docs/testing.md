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
The deferred-wakeup setup retries at most eight times if the worker's deadline
passes before preemption exclusion is established; deferred-wakeup assertions
run only after a future deadline is observed under that guard.

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
Host and QEMU CLI checks reject the withdrawn `-C` option, including `-CC`.
The former syscall number 19 follows the unknown-call policy: its child process
is terminated without breaking the parent. Production rtbench execution and
full/focused profiling contracts use POSIX absolute sleep.

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
rtbench -p 1000000 -n 100 -o 50000 -v
taskset 1 rtbench -p 1000000 -n 100 -o 50000 -v
```

Retain each UART transcript with the build identity, board/firmware setup,
clock resolution, and competing workload. With `-v`, expect 100 zero-based STEP
rows, then a summary with `result=5000000`; report output begins only after sampling
and the final non-overrun sleep. Compare release latency, elapsed TET, and
overrun distributions across repeated runs and CPU placement. Record observed
values, not a fabricated expected latency or hardware performance guarantee.
Payload operation counts do not guarantee a duration, and TET includes
preemption. Physical-board measurements remain the user's responsibility.

### Raspberry Pi hardware result (user-reported)

After raising the configured ARM CPU frequency, the user supplied ten runs on
a physical Raspberry Pi 3B+, CPU3, using a build **without `wake-profile`**:

```text
taskset 3 rtbench -o 300 -n 10000 -p 6000
```

Every run completed 10000 steps with 300 payload additions per step and
`result=3000000`. All time values below are nanoseconds; run order matches the
supplied summaries.

| Run | Release average | Release maximum | TET average | TET maximum | Overruns | Overrun maximum |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1319 | 4312 | 570 | 625 | 0 | 0 |
| 2 | 1323 | 4781 | 573 | 573 | 0 | 0 |
| 3 | 1320 | 4469 | 569 | 625 | 0 | 0 |
| 4 | 1323 | 5614 | 572 | 573 | 1 | 187 |
| 5 | 1322 | 4885 | 557 | 573 | 0 | 0 |
| 6 | 1320 | 4729 | 571 | 573 | 0 | 0 |
| 7 | 1323 | 4885 | 572 | 625 | 0 | 0 |
| 8 | 1323 | 4729 | 559 | 573 | 0 | 0 |
| 9 | 1323 | 4834 | 559 | 573 | 0 | 0 |
| 10 | 1319 | 4572 | 569 | 625 | 0 | 0 |

Across 100000 measured steps, release averages range from 1319 to 1323 ns,
the largest release latency is 5614 ns, and one payload end exceeds the next
release by 187 ns. Every summary reports `overrun_avg_ns=0` because averages
use integer division, including the run with one overrun.

These summaries measure release latency and payload TET, not sleep submission
or complete round-trip cost. They do not count blocking versus early-return
sleeps, so neither a full timer wakeup on every step nor a round-trip maximum
below 5 or 6 us is established. The separate
[15000 ns full-profile result](wake-profiling.md#higher-cpu-frequency-user-reported)
uses a different period and instrumentation; it is not a matched overhead
comparison with these runs.

These are user-reported physical-board observations, not agent-executed runs,
QEMU timing measurements, or worst-case guarantees. The runtime ARM CPU rate,
exact FIT/build identity, firmware version, and competing workload were not
measured or supplied with the summaries.

## Sleep/wakeup phase profiling

The optional `wake-profile` build records exact absolute-sleep cycles without
heap allocation or UART output during collection. It has separate artifacts
and an opt-in functional contract; canonical production builds and releases
exclude instrumentation. See the [profiling procedure](wake-profiling.md) for
session ownership, in-process console reports, and matched hardware runs.
The ordinary-build summaries above do not measure profiling overhead.

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
