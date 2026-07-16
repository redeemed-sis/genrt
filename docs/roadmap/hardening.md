# Hardening backlog

This is a non-calendar backlog. Items are investigation themes, not automatic
merge blockers; prioritize them only when a task provides reachable evidence or
explicit acceptance criteria.

## High-value architecture work

1. **Scheduler and frame lifecycle**
   - consolidate duplicated block/wake entry points and clarify Task/Thread
     ownership without erasing the user/kernel distinction;
   - centralize scheduler state transitions and eliminate direct thread-slot
     state mutation outside that transition layer;
   - measure bounded queue behavior and critical-section length.
2. **Process decomposition**
   - separate process table/lifecycle, image loading, wait/join, FD access, and
     user-stack construction currently concentrated in `process.rs`;
   - preserve atomic consume/reclaim and rollback invariants during extraction.
3. **Interrupt API boundaries**
   - replace ad hoc architecture dispatch wiring with explicit IRQ ownership
     interfaces while keeping GIC/ESR details in AArch64 code;
   - measure heap/frame allocator critical-section latency before claiming a
     hard upper bound;
   - retain allocation-free bounded handlers.

## Boundary cleanup

4. Evolve stdin waiting into a console/TTY ownership model rather than a
   scheduler-special case, without changing fd semantics prematurely.
5. Consolidate trap-frame initialization and remove interfaces no longer used by
   the established EL1/EL0 restore model.

## Maintainability

7. Audit cross-module helper duplication and move only genuinely generic
   primitives to their owning layer.
8. Make userspace program compilation scale beyond one command-shaped build
   path while retaining `user/c/programs.toml` as product composition truth.

Every hardening change needs focused regression evidence and must preserve the
real-time, architecture, user-fault, and release invariants in `memory/`.
