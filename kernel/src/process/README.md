# Process subsystem

`process` is the architecture-neutral owner of process policy. Its facade in
`mod.rs` is the only interface used by syscall, init, and AArch64 fault code.

| Module | Responsibility |
| --- | --- |
| `id`, `state` | Generation-checked identity and pure lifecycle/fault types |
| `error` | Private operational errors and errno conversion |
| `affinity` | Immutable owner lookup for current or explicit process IDs |
| `record` | Identity-independent `Process` aggregate and process metadata |
| `table` | Generation-plus-process slots, global table, and O(1) `ThreadId` reverse index |
| `resources` | Process-owned image/address-space bundle and `ProcessFileState` |
| `files`, `image` | Process-local FD/cwd state and argv/envp/stack preparation |
| `access` | Current-process FD/cwd facade orchestration over the table |
| operation modules | Existing spawn, fork, exec, wait, lifecycle, and fault paths |

`ProcessSlot` contains only its generation and one identity-independent
`Process`. That aggregate owns `ProcessState`, `ProcessResources`, parent and
main-thread relationships, terminal status, and process wait/consumer metadata.
`ProcessResources` owns the address space, ELF image, and
`ProcessFileState { FdTable, cwd }`; it never contains a user stack. The
corresponding `Thread` owns its `OwnedUserStack` and retains only a non-owning
`AddressSpaceId`.

Every live process has one immutable logical CPU owner. Before main-thread
publication, process code assigns that same owner to the completed address
space and the thread's `home_cpu`. Publication holds process-table ownership
while a bounded scheduler pre-ready hook binds the exact `ThreadId`; only then
can the scheduler expose the thread through a local ready queue or remote
ingress. The scheduler remains unaware of `ProcessId`.

`sched_setforkaffinity(cpu)` stores a process-owned one-shot override for the
next successfully published child. A later call replaces it, `-1` resets it,
and failed pre-publication staging preserves it. A child starts without an
override. Default fork placement uses the parent's immutable owner; `execve`
preserves that owner.

`sched_getaffinity(pid, cpusetsize, mask)` reads the same immutable process
owner under short process-table ownership and copies a one-bit, fixed userspace
mask only after releasing that lock. PID `0` resolves through the current
thread reverse index; a positive generation-bearing PID may name another
unreaped process. The query does not expose `CpuId`, kernel CPU capacity, or a
runtime migration operation.

The table publishes and clears the reverse index with `main_thread`; scheduler
code never stores or resolves a `ProcessId`. Its IRQ-safe SMP lock covers only
bounded slot/index mutation. It is released before waiter completion,
scheduler handoff, user copy, parsing, or resource cleanup.

Operation modules use sibling interfaces, never the public facade. Table
critical sections cover slot/index and bounded process-local mutations;
scheduler handoff, user copies, parsing, thread join/reap, and address-space
destruction occur after the table guard is released. Transactional staging and
rollback ownership refactoring are deferred.
