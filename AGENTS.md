# AGENTS.md

## Purpose

genrt is an experimental hard real-time operating system. Repository changes
must preserve determinism, explicit ownership, architecture boundaries, and
reproducible workflows.

The active target is AArch64 `aarch64-unknown-none-softfloat` on single-core
QEMU `virt` with GICv2. Other architecture directories are structural only
unless a task explicitly activates them.

## Context order

Before changing code or documentation, read:

1. this root `AGENTS.md`;
2. the nearest nested `AGENTS.md` for the target path;
3. the nearest relevant module `README.md`;
4. `memory/invariants.md`;
5. relevant entries selected from `memory/decisions/README.md`;
6. the actual source, callers, tests, and generated workflow help.

Do not treat README prose as stronger evidence than source code or tests.

## Non-negotiable rules

- Do not guess hardware details. Use architecture documentation, parsed firmware
  data, or the controlled platform protocol.
- Do not introduce heap allocation in interrupt context, scheduler core, frame
  handoff, or timed-event dispatch.
- Keep `unsafe` localized and document the invariant that makes each block safe.
- Keep architecture-specific registers, instruction assumptions, and MMIO
  details out of generic kernel code unless an ADR explicitly changes the
  boundary.
- Do not change the syscall ABI without an ADR, userspace header updates, and
  exact production-program contract coverage.
- Human-readable logs are diagnostics, not a test protocol.
- Test protocol code, markers, supervisors, and fixture provenance must never
  enter production artifacts.
- Preserve unrelated user changes and untracked local artifacts.
- Do not classify hypothetical defensive hardening as a blocker without a
  reachable defect or violated acceptance criterion.
- New or changed `pub` and `pub(crate)` Rust APIs must follow
  `.agents/standards/rustdoc.md`.

## Engineering workflow

- Inspect `git status --short` before edits and before closure.
- Prefer repository patterns and keep one logical change per commit.
- Use `apply_patch` for manual file edits and `git mv` for tracked moves.
- Use Conventional Commits as defined in `.agents/standards/commits.md`.
- Keep build, test, and release semantics in `xtask`; CI YAML should delegate
  to those commands instead of reimplementing them.
- Report only commands that actually ran. State failures and skipped checks.

## Commands

Run from the repository root:

- `just help`
- `just doctor`
- `cargo xtask check`
- `cargo xtask test-aarch64 --list`
- `cargo xtask test-aarch64`
- `cargo xtask ci`
- `cargo xtask dist --tag vX.Y.Z --output-dir dist`
- `just qemu-cmd-aarch64`
- `just gdb-cmd-aarch64`

Select verification by scope using the `genrt-verify` skill. Kernel, architecture,
userspace, cross-cutting, and release-sensitive changes require the applicable
QEMU gate; documentation-only work still requires link and stale-reference
audits.

## Multi-agent policy

The main agent is orchestrator and integrator. Use subagents when independent
read-heavy work materially improves discovery, test planning, or review; do not
turn delegation into a ritual for small local changes.

- `architect`, `explorer`, and `reviewer` are read-only.
- `developer` is the default production-source writer.
- `tester` may write build/test artifacts and tests, but does not edit
  production code without explicit direction.
- Developer and tester do not edit source concurrently.
- Default to one writer and one-level delegation. Separate worktrees and
  parallel source writers require an explicit integration plan.
- Subagent handoffs contain: summary, evidence, relevant invariants, risks or
  findings, recommended action, validation performed, and unverified
  assumptions.

Use the smallest workflow that fits: direct work for a typo; exploration,
implementation, and focused verification for a local bug; architecture and
independent review for cross-layer changes.

## Documentation and decisions

- Root `README.md` is a landing page, not a subsystem manual.
- Put implementation details in the nearest module README and durable current
  facts in `memory/current-state.md`.
- Put active backlog in `docs/roadmap/`, not in ADRs or project invariants.
- Add a new ADR for changes to ownership, lifecycle, ABI, architecture
  boundaries, or determinism policy.
- Never rewrite an accepted ADR to hide history. Mark explicit supersession and
  update `memory/decisions/README.md`.
- Keep all repository documentation and agent instructions in English.

## Definition of done

- Workspace builds and applicable host checks pass.
- Relevant AArch64 QEMU contracts pass; cross-cutting changes pass
  `cargo xtask ci`.
- Release changes pass `cargo xtask dist` and checksum verification.
- Runtime-sensitive changes preserve the invariants in `memory/invariants.md`.
- Documentation, ADR index entries, and relative links are synchronized.
- Commands remain reproducible from the repository root.
- `git diff --check` passes and final status contains only intended changes or
  explicitly reported local artifacts.
