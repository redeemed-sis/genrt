# Process subsystem

`process` is the architecture-neutral owner of process policy. Its facade in
`mod.rs` is the only interface used by syscall, init, and AArch64 fault code.

| Module | Responsibility |
| --- | --- |
| `id`, `state` | Generation-checked identity and pure lifecycle/fault types |
| `error` | Private operational errors and errno conversion |
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

The table publishes and clears the reverse index with `main_thread`; scheduler
code never stores or resolves a `ProcessId`.

Operation modules use sibling interfaces, never the public facade. Table
critical sections cover slot/index and bounded process-local mutations;
scheduler handoff, user copies, parsing, thread join/reap, and address-space
destruction occur after the table guard is released. Transactional staging and
rollback ownership refactoring are deferred.
