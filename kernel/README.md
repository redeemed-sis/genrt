# Generic kernel

`kernel/` owns architecture-neutral policy and lifecycle. CPU register details,
descriptor encodings, exception modes, and MMIO remain behind architecture ABI
hooks.

## Subsystems

| Module | Responsibility |
| --- | --- |
| `memory` | Physical map, frame allocator, heap, kernel/user VM, user copies |
| `sched`, `task`, `task_call` | Task states, preemption, block/wake, thread lifecycle |
| `time` | Monotonic counter conversion and one-shot timed events |
| `ipc` | Bounded mailbox buffers, wait queues, and timeout integration |
| `process` | Process table, address-space/image ownership, fork/exec/wait, cwd/FD access |
| `fs` | Initramfs mount, readonly ramfs index, paths, and descriptor tables |
| `loader` | Static userspace ELF validation and segment loading |
| `syscall` | Architecture-neutral syscall behavior and errno mapping |
| `console`, `log` | Allocation-free output and bounded stdin buffering |
| `arch` | Opaque live exception context and decoded syscall request facade |

Detailed ownership lives in the [memory](src/memory/README.md),
[scheduler](src/sched/README.md), and [filesystem](src/fs/README.md) guides.

## Execution model

After architecture boot, `kernel_main` initializes physical memory, switches to
runtime TTBR1 tables, mounts initramfs, bootstraps scheduler storage, and enters
the first selected trap frame. The init kernel thread spawns `/init` as a normal
user process and joins it.

The production scheduler starts with exactly two kernel tasks: the permanent
idle thread and one non-idle `kernel_init_thread`. QEMU scenario features replace
the non-idle static-task set with their bounded test coordinators; production
does not carry unrelated background workloads.

Tasks are schedulable contexts. Kernel threads have only kernel state; user
threads reference their owning process and TTBR0 address space. A process owns
the userspace resources shared across its current single main thread: loaded
ELF, stack, cwd, FDs, and terminal status.

Timer IRQs collect expired typed events and may choose a different return frame.
Blocking syscall/task-call paths record a reason, commit scheduler state, and
resume another task. Wakeup owners remain in time, IPC, process, or console
layers; the scheduler owns runnable state and queue visibility.

Architecture entry code wraps each live exception frame in one non-null,
exclusive `ActiveContext`. Generic syscall handlers receive a decoded
`SyscallRequest` and mutate return state only through that context. A temporary
crate-only scheduler bridge exposes frame words solely to existing saved-frame
copy and fork-clone code; saved scheduler storage remains a separate hardening
boundary.

## Allocation and synchronization

The active system is single-core. Local IRQ guards prevent same-core interrupt
reentrancy; they are not SMP locks. Heap use is allowed in bootstrap and normal
task context. IRQ, scheduler, timed-event, and frame-handoff paths operate on
bounded preallocated storage.

Resource cleanup occurs after ownership has been atomically removed under a
short critical section. Parsing, filesystem traversal, user copies, and heavy
destruction do not run with IRQs disabled.

## Userspace lifecycle

The process layer creates TTBR0 roots, loads static ELF segments, maps user
stacks, and initializes EL0 frames through architecture helpers. `fork` performs
bounded eager copying; `execve` stages a replacement before commit; process
exit/fault wakes one registered waiter and leaves resources reclaimable.

Lower-EL faults become `ProcessExitStatus::Faulted` when ownership is known.
Kernel/current-EL faults remain fatal. User pointers are accessed only through
the user-copy layer.

For active capabilities and limitations, see
[`memory/current-state.md`](../memory/current-state.md). Kernel changes must
preserve [`memory/invariants.md`](../memory/invariants.md).
