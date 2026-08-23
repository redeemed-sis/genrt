# Hardening backlog

This is a non-calendar backlog. Items are investigation themes, not automatic
merge blockers; prioritize them only when a task provides reachable evidence or
explicit acceptance criteria.

## High-value architecture work

1. **Scheduler and frame lifecycle**
   - measure bounded queue behavior and critical-section length;
   - define thread-group/address-space lifetime before adding multiple user
     threads to one process.
2. **Transactional process resource ownership**
   - build explicit staging, commit, and rollback ownership for spawn, fork,
     and exec now that process module boundaries are separated;
   - preserve the current ownership transfer into `Thread`, plus atomic
     consume/reclaim and rollback invariants.
3. **Interrupt API boundaries**
   - replace ad hoc architecture dispatch wiring with explicit IRQ ownership
     interfaces while keeping GIC/ESR details in AArch64 code;
   - add exception-context-aware per-CPU lock-rank tracking before enabling
     runtime lockdep, so an interrupting context does not inherit the
     interrupted context's logical lock stack;
   - measure heap/frame allocator critical-section latency before claiming a
     hard upper bound;
   - retain allocation-free bounded handlers.
4. **Secondary CPU activation**
   - initialize secondary scheduler idle/current state now that local
     exceptions, GICC/PPI, physical timers, and generic time queues are ready;
   - add IPI acknowledgement and remote wake notification on top of ADR-0036
     shared-state synchronization and ADR-0038 secondary bring-up;
   - add an IPI-backed remote timer command for prompt insertion into another
     CPU's existing per-CPU deadline queue;
   - define userspace TLB shootdown ownership before executing EL0 on another
     CPU.

## Boundary cleanup

5. Evolve the current console-owned stdin ring and wait registration into TTY
   semantics without moving line discipline into the scheduler or changing fd
   semantics prematurely.
6. Consolidate trap-frame initialization and remove interfaces no longer used by
   the established EL1/EL0 restore model.

## Maintainability

7. Audit cross-module helper duplication and move only genuinely generic
   primitives to their owning layer.
8. Make userspace program compilation scale beyond one command-shaped build
   path while retaining `user/c/programs.toml` as product composition truth.

Every hardening change needs focused regression evidence and must preserve the
real-time, architecture, user-fault, and release invariants in `memory/`.
