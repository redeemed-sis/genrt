---
name: genrt-change-workflow
description: Use for nontrivial genrt features, refactors, and hardening that require discovery, implementation, independent verification, and documentation sync. Do not use for typos or isolated local documentation edits.
---

# genrt change workflow

1. Read the root and nearest nested `AGENTS.md`, module README, project
   invariants, relevant ADR index entries, source, tests, and `git status`.
2. Delegate independent read-heavy discovery to explorer, architect, or tester
   planning only when it reduces uncertainty.
3. Consolidate scope, ownership, invariants, acceptance criteria, and the exact
   verification matrix before edits.
4. Use one developer as production-source writer. Do not run source-editing
   agents concurrently.
5. Run focused checks, then independent tester/reviewer passes appropriate to
   the risk. Feed confirmed findings back through the single writer.
6. Synchronize module docs, current state, and ADRs without rewriting history.
7. Close with intended diff, commands actually run, failures/skips, residual
   risks, and untouched local artifacts.

Require subagent handoffs to include summary, evidence, relevant invariants,
risks/findings, recommendation, validation, and unverified assumptions.
