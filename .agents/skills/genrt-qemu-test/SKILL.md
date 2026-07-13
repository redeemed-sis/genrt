---
name: genrt-qemu-test
description: Use when adding, changing, debugging, or reviewing genrt AArch64 QEMU contract tests and test artifacts. Do not use for ordinary interactive QEMU demos without machine assertions.
---

# genrt QEMU contracts

- Treat `GTRT/1` records as the only machine pass/fail protocol. Ignore ordinary
  UART prose for assertions while retaining it in `serial.log`.
- Keep production kernels/programs distinct from test-only coordinators,
  supervisors, helpers, markers, and provenance.
- Use controlled fixtures and generated exact invocation plans; do not couple
  tests to mutable production sample files or directory listings.
- Bound parser memory, transcript tails, host actions, step/case/suite time, and
  UART channels. Always terminate and reap QEMU.
- Add negative coverage for malformed protocol, sequencing, timeout, process
  status, marker/provenance, or trust-boundary changes.
- Run the targeted case first, inspect `target/test-results/<case>/`, then run
  `cargo xtask ci` for cross-cutting changes.

Read `tests/qemu/README.md`, `docs/testing.md`, and ADR-0025 before changing the
runner or protocol.
