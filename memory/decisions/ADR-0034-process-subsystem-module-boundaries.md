# ADR-0034: Process subsystem module boundaries

## Status

Accepted

## Context

The process subsystem had one source file containing bounded-table ownership,
file state, image preparation, and lifecycle operations. ADR-0033 already
established that scheduler state is thread-only, user stacks are thread-owned,
and process lookup uses an O(1) reverse index. The source layout did not make
those boundaries directly auditable.

## Decision

- `process::mod` is a thin facade; external callers do not import internals.
- `table` owns bounded slots, generation lifecycle, global `UnsafeCell`, and
  the process/thread reverse index. Each slot contains only its generation and
  one identity-independent `Process` from `record`.
- `Process` owns lifecycle and relationship metadata plus `ProcessResources`.
  `ProcessResources` owns the address-space/image bundle and a
  `ProcessFileState`; `files` owns that local FD/cwd state, while `access`
  orchestrates current-process facade calls through the table and `image` owns
  `ExecArgs` and initial-stack preparation. ELF policy remains in `loader::elf`.
- Operation modules retain existing spawn, fork, exec, wait, lifecycle, and
  fault paths. Internal dependencies use concrete sibling modules, not facade.
- Process owns `OwnedUserAddressSpace`; its one main `Thread` owns
  `OwnedUserStack`. Scheduler code does not depend on process metadata.

## Invariants

- The table publishes the generation-checked O(1) `ThreadId` to `ProcessId`
  index with `main_thread`.
- `Process` does not contain its `ProcessId`. `ProcessResources` contains no
  thread-owned `OwnedUserStack`.
- Existing IRQ-lock drop points, publication order, eager fork, exec commit
  order, and reap cleanup order are unchanged.
- Heavy destruction occurs after table and scheduler exclusion.

## Consequences

The mechanical extraction makes ownership auditable without changing syscall
ABI, lifecycle semantics, synchronization, or the one-thread-per-process model.
Transactional resource staging, commit, and rollback refactoring remain future
hardening work.

## Alternatives considered

- Keep the monolith: rejected because table and operation ownership remained
  difficult to audit.
- Introduce a transactional process-image abstraction during extraction:
  rejected because it combines a structural move with lifecycle changes.

## Validation

- Compile and unit-test moved table coverage.
- Run existing AArch64 contracts without adding cases or test protocol code.
- Search scheduler/process imports and external facade bypasses.

## Related decisions

- [ADR-0017](ADR-0017-process-table-and-user-fault-policy.md)
- [ADR-0022](ADR-0022-fork-exec-waitpid-echo.md)
- [ADR-0033](ADR-0033-unified-thread-model-and-scheduler-process-separation.md)
