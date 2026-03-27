# AGENTS.md

## Purpose
This repository contains a hard real-time OS project. Changes must preserve determinism, explicit invariants, and reproducible workflows.

## Non-negotiable rules
- Do not guess hardware details.
- Do not introduce heap allocation in interrupt context or scheduler core.
- Keep `unsafe` localized and document its invariants.
- Do not widen architecture-specific code into generic kernel code without an ADR.

## Main commands
- `just help`
- `just doctor`
- `just phase0-check`
- `just qemu-cmd-aarch64`
- `just gdb-cmd-aarch64`

## Definition of done
- Workspace builds.
- Phase 0 checks pass.
- Commands are reproducible from repo root.
- Any architectural decision is captured in `ai-docs/decision-records/`.
