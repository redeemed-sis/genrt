# Generic kernel

`kernel/` owns architecture-neutral policy and lifecycle. CPU register details,
descriptor encodings, exception modes, and MMIO remain behind architecture ABI
hooks.

## Subsystems

| Module | Responsibility |
| --- | --- |
| `config` | Central architecture-neutral capacities, scheduler policy values, and ABI limits |
| `memory` | Physical map, frame allocator, heap, kernel/user VM, user copies |
| `cpu` | Typed logical CPU identity and bounded boot hardware registry |
| `sched` | Per-CPU thread states, controlled scheduler calls, preemption, typed waits, and lifecycle |
| `time` | Monotonic counter conversion and one-shot timed events |
| `ipc` | Bounded mailbox buffers, wait queues, and timeout integration |
| `process` | [Bounded process table and lifecycle](src/process/README.md), address-space/image ownership, fork/exec/wait, cwd/FD access |
| `fs` | Initramfs mount, readonly ramfs index, paths, and descriptor tables |
| `loader` | Static userspace ELF validation and segment loading |
| `syscall` | Architecture-neutral syscall behavior and errno mapping |
| `power` | Linux reboot validation and architecture-neutral terminal power policy |
| `console`, `log` | Allocation-free output and bounded stdin buffering |
| `arch` | Opaque live/saved context ownership and decoded syscall request facade |

Detailed ownership lives in the [memory](src/memory/README.md),
[scheduler](src/sched/README.md), and [filesystem](src/fs/README.md) guides.

## Execution model

Before `kernel_main`, AArch64 initializes CPU0's shared GIC policy and local
vectors/GICC/PPI/physical-timer state. Generic boot then registers CPU0,
validates the DTB-derived CPU count, initializes physical memory, switches to
runtime TTBR1 tables, mounts initramfs, publishes bounded scheduler storage,
and preallocates every topology CPU's time queue. CPU0 then asks AArch64 to
start each expected secondary. Each secondary completes local architecture
setup first, then enters one generic function that binds logical identity,
initializes one scheduler-owned idle/current context from CPU0-reserved storage,
and enters that saved frame. Generic code neither invokes local interrupt setup
nor controls DAIF. The scheduler marks each context online immediately before
the saved frame entry; CPU0 waits sequentially for every exact context. CPU0
then enters its first selected trap frame.
The init kernel thread spawns `/init` as a normal user process and joins it.

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

When a transition makes a thread runnable away from its immutable home CPU, it
publishes the thread to that CPU's bounded `remote_ready` ingress and requests
one coalesced architecture notification under the same scheduler lock. The
target scheduler IPI drains the ingress, requests local preemption, and uses the
normal IRQ-return handoff. A local post-exit notification provides the later
safe point that finalizes a retired stack after execution has moved away from
it. Scheduler quanta exist only while a local runnable peer exists; idle and
sole-current contexts do not poll for remote work.

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

`LocalIrqGuard` prevents interrupt re-entry on one CPU only. Shared runtime
owners use allocation-free `SpinLock` or IRQ-safe `IrqSpinLock`; CPU-local
scheduler, preemption, timer, and ready state remains owner-local. Nested
`PreemptGuard` leaves IRQs in their caller-selected state, coalesces scheduler
requests, and performs a deferred handoff only at a typed timer-return or
private sched-call checkpoint. Heap use is allowed in bootstrap and normal
thread context. IRQ, scheduler, timed-event, and frame-handoff paths operate on
bounded preallocated storage.

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

Each process, main thread, and published TTBR0 root share one immutable logical
CPU owner. Fork may select a one-shot child CPU before publication; exec keeps
the existing owner. The scheduler sees only `ThreadId`, `home_cpu`, and an
owner-bearing `AddressSpaceId`, while the process layer binds its reverse index
before runnable publication.

Lower-EL faults become `ProcessExitStatus::Faulted` when ownership is known.
Kernel/current-EL faults remain fatal. User pointers are accessed only through
the user-copy layer.

`SYS_REBOOT` validates Linux raw magic and command values before mapping
restart or power off to a narrow architecture hook. The path allocates nothing,
acquires no locks, and is valid on any online CPU. A successful backend call is
terminal; a returned rejection becomes a syscall errno. Until genrt gains
credentials, this operation is intentionally available to every process.

For active capabilities and limitations, see
[`memory/current-state.md`](../memory/current-state.md). Kernel changes must
preserve [`memory/invariants.md`](../memory/invariants.md).
