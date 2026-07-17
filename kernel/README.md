# Generic kernel

`kernel/` owns architecture-neutral policy and lifecycle. CPU register details,
descriptor encodings, exception modes, and MMIO remain behind architecture ABI
hooks.

## Subsystems

| Module | Responsibility |
| --- | --- |
| `memory` | Physical map, frame allocator, heap, kernel/user VM, user copies |
| `sched` | Thread states, controlled scheduler calls, preemption, typed waits, and lifecycle |
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

The production scheduler starts with exactly two kernel threads: the permanent
idle thread and one non-idle `kernel_init_thread`. QEMU scenario features replace
the non-idle static-thread set with their bounded test coordinators; production
does not carry unrelated background workloads.

`Thread` is the only schedulable entity. `ThreadId { index, generation }`
directly addresses one bounded slot. Kernel threads own kernel execution state;
user threads additionally own one `OwnedUserStack` and retain only an opaque,
non-owning `AddressSpaceId` for TTBR0 activation. A process owns the TTBR0 root,
loaded ELF, cwd, FDs, relationships, and terminal status, and records its
joinable `main_thread`. A fixed process-owned reverse index resolves the current
thread to that process in O(1) without adding process metadata to the scheduler.

Timer IRQs collect expired typed events and may choose a different return frame.
Each blocking episode has an exact `WaitToken` containing a generation-aware
thread identity and per-slot sequence. Condition owners in time, IPC, process,
or console code publish and later complete that token; the scheduler owns only
wait lifecycle, runnable state, and ready-queue visibility.

The private scheduler transition layer is the sole writer of lifecycle state,
slot generation, current-thread identity, and ready-queue membership. It returns
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
shared with interrupt handlers. `PreemptLock` identifies thread-context-only state such
as the fixed heap and runtime frame allocator. Its nested `PreemptGuard` leaves
IRQs in their caller-selected state, coalesces scheduler requests, and performs
a deferred handoff only at a typed timer-return or private sched-call checkpoint.
Neither domain is an SMP lock. Heap use is allowed in bootstrap and normal thread
context. IRQ, scheduler, timed-event, and frame-handoff paths operate on bounded
preallocated storage.

Blocking and terminal thread transitions are forbidden under `PreemptGuard`.
Kernel yield under a guard records a pending reschedule and returns to the same
thread; release of the outermost guard triggers the controlled checkpoint when
the saved IRQ state permits it.

Resource cleanup occurs after ownership has been atomically removed under a
short critical section. Parsing, filesystem traversal, user copies, and heavy
destruction do not run with IRQs disabled.

## Userspace lifecycle

The process layer creates TTBR0 roots, loads static ELF segments, prepares user
stacks, and transfers each stack into a normal joinable user thread. `fork`
performs bounded eager copying; `execve` stages a replacement before atomically
swapping process and current-thread resources; process exit/fault wakes its
registered waiter and then uses ordinary thread exit/join/reap. Reap releases
the user stack before the process destroys its ELF frames and address space.

Lower-EL faults become `ProcessExitStatus::Faulted` when ownership is known.
Kernel/current-EL faults remain fatal. User pointers are accessed only through
the user-copy layer.

For active capabilities and limitations, see
[`memory/current-state.md`](../memory/current-state.md). Kernel changes must
preserve [`memory/invariants.md`](../memory/invariants.md).
