# AArch64 QEMU contracts

The Rust-hosted runner executes declarative cases from `tests/qemu/cases/` and
stores complete evidence below `target/test-results/`.

## Test layers

- `kernel-contract`: test-enabled kernel coordinator for timer, sleep,
  preemption, and mailbox contracts.
- `user-fault`: test-enabled kernel coordinator that joins a faulting EL0
  process and verifies exact fault classification.
- `userspace-contract`: production kernel plus test supervisor and exact
  production-program invocations.
- `shell-contract`: production kernel and shell plus test-only helpers and a
  host nonce challenge for UART input and command behavior.

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

## Cases, fixtures, and programs

Case TOML declares suite/supervisor, expected structured events, bounded host
actions, and timeout. `tests/qemu/program-contracts.toml` maps every dynamic
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

The runner uses bounded UART channels and failure tails, applies step/case/suite
deadlines, drains output to EOF, reparses the complete serial log, and always
terminates and reaps QEMU.

See [`docs/testing.md`](../../docs/testing.md) and ADR-0025 for gate and release
integration.
