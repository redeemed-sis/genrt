# ADR-0017: Bounded Process Table And User Fault Policy

## Status

Accepted

## Context
The first EL0 milestone proved that a user thread can run through TTBR0, call
`write`, and exit. That smoke path still treated the user program mostly as a
thread with an address space. The kernel needs a minimal process object so user
exit/fault status belongs to the process, not to the scheduler thread slot, and
so bad EL0 code does not automatically become a kernel fatal exception.

## Decision
Introduce a small bounded process table in `kernel::process`:

- `ProcessId { index, generation }` prevents stale process handles from
  accidentally naming a reused slot.
- Each process slot owns the user TTBR0 address space, main user thread handle,
  user stack frames, process state, exit status, and at most one joiner.
- The main user thread is detached from the thread lifecycle perspective; the
  process table owns the status and resource cleanup.
- Kernel threads wait for process completion through a dedicated process-join
  task-call and `BlockReason::ProcessJoin`.
- `sys_exit` stores `ProcessExitStatus::Exited(code)` and terminates the current
  user thread.
- Unknown syscalls and lower-EL sync faults store
  `ProcessExitStatus::Faulted(UserFault)` and terminate the current user thread.

Current-EL kernel exceptions remain fatal. Only lower-EL user faults are routed
through the process-fault policy.

## Invariants
- Process table storage is bounded and statically allocated for the current
  milestone.
- Process table mutations happen on one core in short IRQ-disabled sections or
  in the synchronous lower-EL trap path for the current task.
- Scheduler threads are schedulable execution entities; processes own TTBR0
  lifetime and user exit/fault status.
- User image frames loaded by QEMU are reserved loader memory and are not freed
  during process reclaim. User stack frames and TTBR0 page-table frames are
  reclaimed after process join.
- Bad EL0 code may terminate its process, but must not turn into a kernel panic
  unless the kernel cannot attribute the fault to a current user process.

## Consequences
- The kernel init thread now joins a process object and receives
  `ProcessExitStatus`.
- Fault diagnostics include process/thread identity and ESR/FAR/ELR/SPSR data.
- This is still not a POSIX process model: no fork, exec, waitpid, file
  descriptor table, ASIDs, or multi-process scheduling policy exists yet.
