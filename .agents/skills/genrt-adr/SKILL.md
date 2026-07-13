---
name: genrt-adr
description: Use when creating, migrating, updating, or superseding a genrt architecture decision. Do not use for local implementation notes or backlog items that do not change durable ownership, ABI, lifecycle, or determinism policy.
---

# genrt ADR lifecycle

- Read `memory/AGENTS.md`, the decision index, template, and related ADRs.
- Preserve accepted historical Context and Decision text. Record new
  architecture in a new ADR and add explicit `Supersedes`/`Superseded by`
  relationships where replacement is real.
- Do not mark an ADR superseded merely because later work adds capabilities.
- Do not invent dates; use Git history only when a date is required.
- State invariants, consequences, alternatives, validation, and related
  decisions.
- Update `memory/decisions/README.md` and `memory/current-state.md` when the
  active implementation changes.
- Check all relative links after moves or edits.
