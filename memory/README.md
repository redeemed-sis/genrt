# Project memory

`memory/` is genrt's checked-in source of durable project knowledge. It records
the current implementation state, cross-cutting invariants, and architectural
decisions that must remain available to every contributor and automation run.

This directory is distinct from native Codex memory in `~/.codex/memories/`.
Native memory is local, generated state and may help an individual session;
it is not a project contract. Rules required by the repository belong in
`AGENTS.md`, and durable engineering decisions belong here.

## Reading project memory

1. Read [`invariants.md`](invariants.md) for cross-cutting constraints.
2. Read [`current-state.md`](current-state.md) to understand implemented
   capabilities and current boundaries.
3. Use [`decisions/README.md`](decisions/README.md) to select ADRs relevant to
   the subsystem being changed.
4. Verify documentation claims against source code and tests before relying on
   them for implementation details.

Do not load every ADR by default. Read the index, then open the decisions whose
scope intersects the task.

## Update policy

An architectural change must update the nearest module documentation,
`current-state.md` when capabilities or boundaries change, and the relevant ADR
index entry. Create a new ADR when ownership, ABI, lifecycle, determinism, or a
cross-layer boundary changes. Accepted ADRs remain historical records; use a
new decision and explicit supersession instead of rewriting history.

Backlogs and time-based plans do not belong in project memory. Active hardening
work is tracked in [`docs/roadmap/hardening.md`](../docs/roadmap/hardening.md).
