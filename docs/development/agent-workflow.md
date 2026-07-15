# Agent-oriented development workflow

genrt uses repository instructions, project-scoped roles, reusable skills, and
checked-in project memory to make engineering work repeatable for humans and
agents. ADR-0026 records the architectural decision; this document describes
the operating workflow.

## Information layers

- `AGENTS.md`: mandatory root and subsystem rules.
- `.codex/agents/`: optional specialized subagent roles.
- `.agents/skills/`: reusable procedures selected by task trigger.
- `.agents/standards/`: commit and rustdoc contracts.
- `memory/`: durable current state, invariants, and decisions.
- module `README.md`: local implementation ownership.
- `docs/`: development, test, release, and backlog guides.

Local Codex memory under `~/.codex/memories/` is not a repository source of
truth.

## Default workflow

1. **Discovery**: explorer traces real code; architect identifies boundaries
   and decisions; tester may propose a test matrix. These are read-heavy and may
   run in parallel.
2. **Consolidation**: the main agent fixes scope, invariants, acceptance
   criteria, and verification before edits.
3. **Implementation**: one developer writes production source.
4. **Independent verification**: tester runs focused/regression checks;
   reviewer examines the complete diff; architect checks cross-layer invariants
   when relevant.
5. **Fix loop**: confirmed findings return to the single writer, then tests are
   rerun.
6. **Closure**: synchronize docs/ADR, audit the final diff, and report exact
   commands and residual risks.

Developer and tester must not edit source concurrently. Multiple worktrees and
parallel writers are outside the default workflow.

## Choosing roles

| Task | Minimum workflow |
| --- | --- |
| Typo or isolated docs correction | Main agent |
| Small local bug | Explorer, developer, tester |
| Kernel feature | Architect/explorer, developer, tester/reviewer |
| Cross-layer refactor | Architect/explorer/test plan, developer, tester/reviewer |
| Patch review | Explorer, tester, reviewer |
| ADR-only change | Architect or explorer, ADR skill, reviewer |

Every subagent handoff includes summary, evidence, relevant invariants, risks or
findings, recommended action, validation performed, and unverified assumptions.

## Model allocation

Project roles pin models according to the cost and reasoning demands of their
work. Exact settings live in `.codex/agents/*.toml`.

| Role | Model | Reasoning | Rationale |
| --- | --- | --- | --- |
| Architect | `gpt-5.6-sol` | High | Cross-layer design and invariant analysis |
| Reviewer | `gpt-5.6-sol` | High | Independent correctness and safety review |
| Developer | `gpt-5.6-terra` | High | Focused implementation after scope consolidation |
| Tester | `gpt-5.6-terra` | Medium | Test execution and artifact diagnosis |
| Explorer | `gpt-5.6-luna` | Medium | Fast read-heavy tracing and evidence gathering |

This keeps the strongest model on architecture and review while avoiding the
cost of inheriting the main session model for routine exploration, test runs,
and bounded implementation work.

## Skills

- `genrt-change-workflow`: nontrivial implementation lifecycle.
- `genrt-qemu-test`: machine protocol and QEMU contract changes.
- `genrt-verify`: risk-based verification matrix.
- `genrt-review`: findings-first review.
- `genrt-adr`: decision lifecycle and supersession.
- `genrt-docs-sync`: documentation ownership and stale-link audit.

Small tasks should not invoke every role. Delegation is useful when independent
evidence improves quality, not as a ceremonial checklist.
