---
name: genrt-verify
description: Use before completing a genrt implementation or review to select and run evidence-based checks. Do not claim checks that were not executed or substitute this matrix for task-specific acceptance tests.
---

# genrt verification

Select the smallest sufficient matrix:

- Documentation only: stale path/status audit, relative-link check,
  `git diff --check`.
- xtask: `cargo fmt --all -- --check`, `cargo test -p xtask --locked`, clippy
  with `-D warnings`, and `cargo xtask check`.
- Kernel, AArch64, or userspace: host checks plus the targeted QEMU case and
  post-link checks.
- Cross-cutting or release-sensitive: `cargo xtask ci`.
- Release workflow: `cargo xtask dist --tag <label> --output-dir <dir>` and
  verify `SHA256SUMS`.

Record exact commands and outcomes. Distinguish a product failure from a missing
host tool or runner failure. Report skipped checks with the reason.
