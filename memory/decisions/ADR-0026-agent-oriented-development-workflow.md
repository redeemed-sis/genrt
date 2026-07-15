# ADR-0026: Agent-oriented development workflow

## Status

Accepted

## Context

genrt has accumulated architecture decisions, subsystem documentation, build
automation, and hard real-time invariants. Keeping all of that in one root
README or in tool-local prompts makes repository work difficult to audit and
allows local agent state to become an accidental source of truth.

Codex discovers repository instructions from the root toward the current
directory, supports project-scoped custom agents in `.codex/agents`, and
discovers reusable repository skills in `.agents/skills`. Read-heavy subagent
work can run independently, while concurrent writers create avoidable source
conflicts and weaken ownership.

## Decision

- Root and nested `AGENTS.md` files define mandatory repository and subsystem
  rules. The root stays concise; the nearest applicable file adds local policy.
- `.codex/agents` defines five project roles: architect, developer, tester,
  reviewer, and explorer. Roles inherit the user-selected model. Read-oriented
  roles use read-only sandboxes; developer and tester may write the workspace.
- `.agents/skills` contains instruction-only procedures for change workflow,
  QEMU tests, verification, review, ADR lifecycle, and documentation sync.
- `memory/` is checked-in project knowledge: current state, cross-cutting
  invariants, and ADRs. Native Codex memory under `~/.codex/memories/` remains
  local generated state and is not a project contract.
- The default workflow has one production-source writer. Explorer, architect,
  tester planning, and review may run in parallel when their work is read-heavy.
  Tester and developer do not edit source concurrently.
- The main agent remains orchestrator and integrator. No recursive custom
  orchestrator is introduced; subagent nesting remains one level deep.
- The root README is a landing page. Detailed implementation ownership lives
  in module READMEs, workflow documentation, and project memory.

## Subsequent refinement

The initial decision let every role inherit the user-selected model. Project
agent files now pin role-specific models and reasoning effort: architecture and
review retain the strongest configuration, while exploration, testing, and
implementation use lower-cost models suited to their bounded responsibilities.
The exact current mapping is owned by `.codex/agents/*.toml` and summarized in
`docs/development/agent-workflow.md`.

## Invariants

- Delegation must not weaken the active `AGENTS.md` chain or repository
  determinism constraints.
- A subagent returns a compact handoff containing summary, evidence, relevant
  invariants, risks/findings, recommendation, validation, and unverified
  assumptions.
- Only one agent changes production source at a time unless an explicit plan
  isolates writers in separate worktrees and defines integration ownership.
- Agents report commands actually executed and do not treat speculative future
  hardening as a current blocker.

## Consequences

Repository knowledge is reviewable, versioned, and available to humans and
agents without relying on one local Codex profile. Scoped instructions reduce
root prompt size, and reusable skills make high-risk workflows consistent.
Multi-agent work remains optional for small changes and incurs coordination
cost only when parallel discovery or independent review is useful.

## Alternatives considered

- Keep all guidance in root `AGENTS.md`: rejected because it would exceed the
  useful instruction budget and duplicate subsystem documentation.
- Use only native Codex memory: rejected because it is local, generated, and
  not a team-controlled source of truth.
- Allow multiple default writers: rejected because source conflicts and unclear
  ownership outweigh expected speedup for the current repository size.
- Add a custom orchestrator agent: rejected because the main session already
  owns consolidation, integration, and closure.

## Validation

- Validate TOML syntax and required custom-agent fields.
- Validate skill frontmatter and repository discovery paths.
- Audit root-to-subdirectory instruction chains.
- Check all repository-relative Markdown links.

## Related decisions

- [ADR-0025](ADR-0025-automated-qemu-testing-and-tagged-releases.md)
- [Codex AGENTS.md guidance](https://developers.openai.com/codex/guides/agents-md/)
- [Codex subagents](https://developers.openai.com/codex/multi-agent/)
- [Codex skills](https://developers.openai.com/codex/skills/)
- [Codex memories](https://developers.openai.com/codex/memories/)
