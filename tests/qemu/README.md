# AArch64 QEMU contracts

The Rust-hosted runner executes declarative cases from `tests/qemu/cases/` and
stores complete evidence below `target/test-results/`.

## Test layers

- `kernel-contract`: test-enabled kernel coordinator for timer, sleep,
  deferred-preemption safe points, exact wait registration/completion, mailbox,
  thread lifecycle, CPU0 identity/per-CPU scheduler context, and allocator
  ownership contracts. Its wait cases cover
  wake-before-block, duplicate completion, both event/timeout orders, late
  timeout isolation, slot reuse, and bounded mailbox loser cleanup. Its
  preemption cases use bounded task/atomic protocols and a test-only timer-IRQ
  counter without asserting scheduling jitter.
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
  status propagation, and child-reaping checks.
- `smp-userspace-contract`: four-CPU production kernel plus a test supervisor
  covering one-shot fork placement, exact affinity masks, affinity preservation
  across exec, remote process lifecycle, and production `taskset` placement on
  a secondary CPU.
- `shell-contract`: production kernel and shell plus test-only helpers and a
  host nonce challenge for UART input and command behavior.
- `poweroff-contract`: executes production `/bin/poweroff`, arms
  `TERMINAL ... POWER_OFF`, and accepts only natural successful QEMU shutdown.
- `reboot-contract`: executes production `/bin/reboot`, arms
  `TERMINAL ... RESTART`, and requires a fresh sequence-one supervisor `READY`
  epoch in the same QEMU process.
- `smp-reboot-contract`: repeats restart after production `/bin/taskset` places
  the rebooting process on CPU1.

Kernel contracts use scenario-specific Cargo features. System contracts use the
byte-identical production kernel with a controlled test initramfs.

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
