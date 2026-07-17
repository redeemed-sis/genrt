# Generic kernel

`kernel/` owns architecture-neutral policy and lifecycle. CPU register details,
descriptor encodings, exception modes, and MMIO remain behind architecture ABI
hooks.

## Subsystems

| Module | Responsibility |
| --- | --- |
| `memory` | Physical map, frame allocator, heap, kernel/user VM, user copies |
| `sched`, `task`, `task_call` | Task states, preemption, typed waits, thread lifecycle |
| `time` | Monotonic counter conversion and one-shot timed events |
| `ipc` | Bounded mailbox buffers, wait queues, and timeout integration |
| `process` | Process table, address-space/image ownership, fork/exec/wait, cwd/FD access |
| `fs` | Initramfs mount, readonly ramfs index, paths, and descriptor tables |
| `loader` | Static userspace ELF validation and segment loading |
| `syscall` | Architecture-neutral syscall behavior and errno mapping |
| `console`, `log` | Allocation-free output and bounded stdin buffering |
| `arch` | Opaque live/saved context ownership and decoded syscall request facade |

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
Each blocking episode has an exact `WaitToken` containing a generation-aware
thread identity and per-slot sequence. Condition owners in time, IPC, process,
or console code publish and later complete that token; the scheduler owns only
wait lifecycle, runnable state, and ready-queue visibility.

The private scheduler transition layer is the sole writer of lifecycle state,
slot generation, current-task identity, and ready-queue membership. It returns
context-free switch outcomes; architecture-neutral handoff code separately
saves and restores typed contexts and activates the selected address space.

Architecture entry code wraps each live exception frame in one non-null,
exclusive `ActiveContext`. Generic syscall handlers receive a decoded
`SyscallRequest` and mutate return state only through that context. Each
occupied scheduler slot owns one inline, non-copyable `SavedContext`; bootstrap,
spawn, fork, block, preemption, and first entry use typed facade operations.
Only the architecture adapter interprets saved storage as a concrete trap
frame.

## Allocation and synchronization

The active system is single-core. `LocalIrqLock`/`LocalIrqGuard` protect state
shared with interrupt handlers. `PreemptLock` identifies task-only state such
as the fixed heap and runtime frame allocator. Its nested `PreemptGuard` leaves
IRQs in their caller-selected state, coalesces scheduler requests, and performs
a deferred handoff only at a typed timer-return or private task-call checkpoint.
Neither domain is an SMP lock. Heap use is allowed in bootstrap and normal task
context. IRQ, scheduler, timed-event, and frame-handoff paths operate on bounded
preallocated storage.

Blocking and terminal task transitions are forbidden under `PreemptGuard`.
Kernel yield under a guard records a pending reschedule and returns to the same
task; release of the outermost guard triggers the controlled checkpoint when
the saved IRQ state permits it.

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
