---
name: genrt-docs-sync
description: Use when behavior or documentation changes require README, project-memory, command, and link synchronization. Do not use to duplicate deep subsystem details into the root README.
---

# genrt documentation sync

1. Identify the owning document: root landing page, nearest module README,
   development/testing/release guide, current-state snapshot, invariant, or ADR.
2. Verify behavior against source/tests and commands against `cargo xtask
   --help` and `just --list`.
3. Keep root README under 200 lines and link to detailed owners.
4. Remove stale status claims, time-based plans, and duplicate architecture
   descriptions.
5. Update `docs/README.md` and the ADR index only when their inventories change.
6. Audit old paths and all repository-relative Markdown links.

Do not rewrite historical ADR Context to match the present; update
`memory/current-state.md` instead.
