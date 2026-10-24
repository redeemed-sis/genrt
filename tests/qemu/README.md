# AArch64 QEMU contracts

The Rust-hosted runner executes declarative cases from `tests/qemu/cases/` and
stores complete evidence below `target/test-results/`.

## Test layers

`wake-profile-contract` is opt-in: ordinary CI excludes it. It runs a diagnostic
kernel and test-only supervisor on four CPUs, covering exact consecutive waits,
full/focused masks, idle/generic/deferred selection, ownership, chunked export,
fault/exit cleanup, heap-pressure ENOMEM and frozen-result preservation. It also
executes the production rtbench in full and focused modes and checks successful
reporting through child exit status, including focused mode with verbose STEP
output. Host tests check default/verbose output, grouped console tables, and
invalid record rejection; no raw UART export or host analyzer is used. Select
it explicitly or use
`--wake-profile` for an instrumented regression matrix; see the
[profiling procedure](../../docs/wake-profiling.md). Ordinary release gates
remain uninstrumented and reject diagnostic kernel markers.

- `kernel-contract`: test-enabled kernel coordinator for timer, sleep,
  deferred-preemption safe points, exact wait registration/completion, mailbox,
  thread lifecycle, CPU0 identity/per-CPU scheduler context, and allocator
  ownership contracts. Its wait cases cover
  wake-before-block, duplicate completion, both event/timeout orders, late
  timeout isolation, slot reuse, and bounded mailbox loser cleanup. Its
  preemption cases use bounded task/atomic protocols and a test-only timer-IRQ
  counter without asserting scheduling jitter.
  `user-root-idle-retention` joins a sole EL0 probe over three process lifetimes.
  Each runs 32 sleeps with exactly 32 direct idle wakes and two actual TTBR0
  writes (activation and exit detach), then verifies frame return after reap.
  The same lifecycle checks VM usercopy eligibility against the selected
  context, including kernel execution with a lazily retained root.
  `saved-context-transfer` exercises the real AArch64 save/restore hooks with
  distinct register patterns, alignment/exception padding, and guard words.
- `vfs-contract`: four-CPU `qemu-test-vfs` kernel with normal init-process
  startup and a controlled C supervisor. Test-only mounts exercise generated
  and writable files, mount-root `..`, cwd, bounded transfers, shared fork
  offsets, exec persistence, cleanup, concurrent access, and pending completion.
  Pending reads remain blocked until the controller writes `1` to the fixture's
  `pending-control` file (`0` rearms an idle gate). The scenario waits beyond
  the former 10 ms completion window before release, without requiring peer
  preparation to finish inside that window. Notifications use exact tokens
  outside fixture locks; host step/case deadlines still bound the scenario.
  Deferred mutations verify peer progress and exactly-once effects across SVC
  restart; backend callback assertions check IRQ-enabled task context.
  Host tests additionally exercise kernel-only mount/seek/sync/metadata APIs.
  No test backend or mount fixture is included in production artifacts.
- `user-fault`: test-enabled kernel coordinator that joins a faulting EL0
  process and verifies exact fault classification.
- `smp-boot`: four-CPU test kernel that validates PSCI secondary high-half
  entry, unique logical/hardware/stack ownership, architecture-owned per-CPU
  exception/GICC/PPI/SGI setup, CPU0-only device routing, online secondary
  scheduler contexts, pinned kernel workers, timer round-robin, targeted busy
  preemption and idle wakeup, scheduler-IPI routing isolation, a controlled
  single-edge coalescing batch, cross-CPU mailbox/join completion, and explicit
  local timer deadlines without a polling quantum.
- `userspace-contract`: production kernel plus test supervisor, exact
  production-program invocations, and production `taskset` argument, failure,
  status propagation, child-reaping, monotonic time, relative/absolute sleep,
  production `/bin/sleep`, shared support-library boundary checks, and exact
  production `/bin/rtbench` execution plus positive-workload and invalid-CLI
  wait/exit contracts, including verbose output and duplicate verbosity rejection.
  The registered rtbench invocation uses three 1 ms steps with zero payload
  operations and POSIX absolute sleep. CLI scenarios reject the withdrawn
  `-C` option. Large sample-capacity tests stay on host.
  The `syscall-boundaries` case covers unaligned copies across mapped pages,
  readonly/out-of-range destinations, no partial write at the stack boundary,
  and isolated child termination for unknown syscalls, including the withdrawn
  counter-sleep number 19.
  Clock tests bracket raw syscall observations with fast EL0 reads, compare
  exact resolution, isolate a bad fast-output pointer, and retain raw EFAULT
  checks. Concurrent processes keep independent writable data across sleeps.
  Single-page reads cover read-only input, every alignment, and an unmapped
  user address.
- `smp-userspace-contract`: four-CPU production kernel plus a test supervisor
  covering one-shot fork placement, exact affinity masks, affinity preservation
  across exec, remote process lifecycle, production `taskset` placement, and
  owner-local absolute sleep on a secondary CPU, and a short production
  `taskset 1 rtbench` run. A separate CPU1-pinned rtbench child lets the
  supervisor assert its immutable affinity before wait/reap without adding
  any probe code to the production utility.
- `shell-contract`: production kernel and shell plus test-only helpers and a
  host nonce challenge for UART input and command behavior. Arrow/Tab input
  exercises the 16-entry history ring, editing and draft restoration,
  duplicate suppression, overflow, completion, and escape recovery.
- `poweroff-contract`: executes production `/bin/poweroff`, arms
  `TERMINAL ... POWER_OFF`, and accepts only natural successful QEMU shutdown.
- `reboot-contract`: executes production `/bin/reboot`, arms
  `TERMINAL ... RESTART`, and requires a fresh sequence-one supervisor `READY`
  epoch in the same QEMU process.
- `smp-reboot-contract`: repeats restart after production `/bin/taskset` places
  the rebooting process on CPU1.

Kernel contracts use scenario-specific Cargo features. System contracts use the
byte-identical production kernel with a controlled test initramfs.
The VFS case is explicitly test-enabled, not evidence that a writable backend
is shipped in the ordinary production kernel.

## Machine protocol

Records begin with ASCII RS (`0x1e`):

```text
<RS>GTRT/1|producer|000001|READY|suite
<RS>GTRT/1|producer|000002|CASE_START|case
<RS>GTRT/1|producer|000003|PASS|case
<RS>GTRT/1|producer|000004|DONE|suite|PASS
```

Sequence numbers are independent per producer. Only the configured supervisor
may announce readiness and terminal success. Malformed records, unknown
versions, gaps, duplicates, `FAIL`, or `ABORT` fail the case. Human UART output
is retained but never evaluated as an assertion.

Terminal power cases add `TERMINAL|case|RESTART` or `POWER_OFF` after
`CASE_START`. This record only arms the expected host lifecycle transition.
QEMU runs those cases with reset and shutdown as distinct actions, so a reset
cannot satisfy poweroff and an exit cannot satisfy reboot.

## Cases, fixtures, and programs

Case TOML declares suite/supervisor, CPU count, expected structured events,
bounded host actions, and timeout. CPU count defaults to one and cannot exceed
the kernel's fixed topology capacity. `tests/qemu/program-contracts.toml` maps every dynamic
production product to one exact executable path, argv, expected status, and
case. `xtask` generates supervisor invocation tables from that plan.

Filesystem tests use `tests/qemu/fixtures/initramfs/`, not mutable production
sample contents. Test helpers and supervisors live in `tests/qemu/user/`, carry
test markers/provenance, and are rejected by production artifact policy.
The shell image additionally installs marked probe aliases under `/bin` for
unique/common-prefix/list completion, plus directory/text decoys and a
same-named builtin collision. No production sample names or directory listing
text are assertions: probe argv/cwd records prove the editing outcome.
Repeated Tab scanning also checks that completion does not exhaust the FD table.

## Adding or changing a case

1. Select production kernel versus a scenario-specific test feature.
2. Define stable protocol case IDs and controlled fixtures.
3. Add negative coverage for malformed status, unexpected exit, timeout, or
   trust-boundary behavior where applicable.
4. Keep each action bounded and avoid assertions on prompts or logs.
5. Run the targeted case and inspect its `serial.log`, `qemu-stderr.log`, and
   `result.json`.
6. Run `cargo xtask ci` for cross-cutting protocol or artifact changes.

Test-kernel suites declare their ordered cases through the reusable
`kernel::test_support::scenario` layer. Each scenario is an ordinary
rustdoc-documented `fn() -> ScenarioResult`; returning `Ok(())` reports `PASS`,
while a stable `Err(reason)` reports `FAIL`. The suite macro owns the coordinator
thread and all `READY`, `CASE_START`, `PASS`, and `DONE` records. New kernel
scenarios must use this pattern. `smp-boot` is the first converted suite;
migration of the older coordinators is intentionally separate work.

The runner uses bounded UART channels and failure tails, applies step/case/suite
deadlines, drains output to EOF, reparses the complete serial log, and always
terminates and reaps QEMU. Host input is delivered with fixed per-byte pacing so
a pipe cannot overrun the emulated PL011 RX FIFO when QEMU runs more slowly than
the host writer. This transport pacing is bounded by the existing step and case
deadlines; prompts and echoed input remain outside the assertion contract.

See [`docs/testing.md`](../../docs/testing.md) and ADR-0025 for gate and release
integration.

rtbench report text is retained only for diagnosis. GTRT supervisor records
and validated child exits decide QEMU pass/fail; no jitter threshold or UART
report parsing establishes performance. Controlled host tests execute the
actual measurement loop to verify exact rows, sums, releases, and sleep targets.
