# Project memory instructions

- Treat `current-state.md` as a maintained snapshot, not as a substitute for
  source and tests.
- Keep `invariants.md` limited to durable cross-cutting constraints. Backlogs
  belong in `docs/roadmap/`.
- Do not rewrite an accepted ADR as though its historical decision never
  existed. Add a new ADR and explicit supersession when architecture changes.
- Keep `decisions/README.md` synchronized with every ADR addition, status
  change, or supersession.
- Do not invent decision dates. Recover a date from Git history only when a
  document requires one.
- Check relative links after moving or editing memory files.
