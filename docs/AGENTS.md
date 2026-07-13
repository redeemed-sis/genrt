# Documentation instructions

- Keep root `README.md` concise and route subsystem details to the nearest
  module README.
- `docs/README.md` owns the human documentation map; avoid parallel indexes.
- Keep commands executable and verify them against `cargo xtask --help` and
  `just --list`.
- Do not add Month/Week/Phase plans or transient status prose to active docs.
- Keep current implementation facts in `memory/current-state.md`, durable rules
  in `memory/invariants.md`, and backlog in `docs/roadmap/`.
- Avoid duplicating architecture truth across documents. Link to the owning
  module or ADR.
- Audit repository-relative Markdown links and stale moved paths after edits.
