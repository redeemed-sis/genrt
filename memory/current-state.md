# Current state

This document is a concise factual snapshot of genrt. Source code, tests, and
accepted ADRs remain authoritative when details differ.

## Active target

- Architecture: AArch64.
- Rust target: `aarch64-unknown-none-softfloat`.
- Machine: QEMU `virt` with GICv2.
- Execution model: CPU0-only scheduling plus bounded high-half bring-up of
  registered secondary CPUs into a scheduler-offline parked state.
- Kernel image: low physical load with a high-half virtual runtime mapping.

## Boot and platform

- A low-linked `.boot.*` trampoline parses the QEMU-provided DTB, constructs
  bootstrap translation tables, enables the MMU, and enters high-linked Rust.
- The runtime DTB supplies the enabled CPU count, hardware affinity targets,
  and PSCI HVC CPU_ON metadata. CPU0 starts secondaries sequentially only after
  runtime TTBR1 is active and publishes that root with Release ordering.
- Four fixed linker-owned 32 KiB bootstrap-stack slots bound the current QEMU
  topology. Every secondary enters low `secondary_start` through PSCI, enables
  the MMU with boot TTBR0 plus runtime TTBR1, enters high Rust, clears its local
  TTBR0, registers in generic kernel code, and parks with asynchronous
  exceptions masked.
- `.boot.text`, `.boot.rodata`, and `.boot.bss` are autonomous before MMU
  enable. `xtask` checks the linked image for relocations, runtime thunks,
  high-VA operands, and branches outside `.boot.*`.
- TTBR1 owns kernel high-half RAM and MMIO mappings. Temporary TTBR0 identity
  mappings are removed after allocator-owned runtime kernel tables are active.
- PL011, GICv2, timer, RAM, and reserved loader ranges come from the controlled
  QEMU protocol and DTB, with an AArch64 QEMU emergency fallback for early
  diagnostics.

## Memory

- The physical frame allocator is generic kernel code and returns physical
  frame ranges.
- A fixed 16 MiB bootstrap heap is allocated from physical frames and exposed
  through the high direct map.
- Heap allocation is permitted during bootstrap and thread context. The heap and
  runtime physical frame allocator use Acquire/Release `SpinLock`; it owns
  nested preemption exclusion, is forbidden in IRQ paths, and releases before a
  possible scheduler checkpoint.
- Boot-discovered physical regions and the heap range are immutable after
  initialization. Runtime free-list state is separately locked and does not
  expose references outside its guard.
- Scheduler and timed-event containers allocate and reserve capacity before
  entering IRQ-sensitive operation.
- Runtime TTBR1 APIs map, unmap, protect, and translate kernel regions after
  the boot tables have been replaced. The generic VM layer serializes writers;
  AArch64 reads live TTBR roots from registers, performs break-before-make and
  inner-shareable TLBI, and defers table-frame reclaim until invalidation
  completes.
- Each user process owns an allocator-backed `OwnedUserAddressSpace`. ELF
  segments and thread-owned user stacks use 4 KiB mappings with user-specific
  permissions.
- `copy_from_user` and `copy_to_user` validate the active user address space;
  fault recovery during the actual copy is not implemented yet.

## Scheduling and time

- The scheduler is round-robin, preemptive, and CPU0-only. A bounded registry
  maps normalized AArch64 hardware identities to CPU0-first logical `CpuId`
  values before scheduler bootstrap. AArch64 retains each logical binding in
  CPU-local `TPIDR_EL1`, making runtime current-CPU resolution O(1); unbound or
  invalid CPUs are rejected rather than treated as CPU0.
- Scheduler contexts are preallocated per logical CPU. Each owns local current,
  idle, ready queue, and initialized/online state; separate CPU-local fixed
  preemption backing keeps lock acquisition independent of shared lifecycle
  storage.
  CPU0 alone is initialized and becomes online at first thread entry; registered
  secondary contexts remain offline while their CPUs stay in the architecture
  park loop. Runtime scheduler entry points resolve the executing CPU once and
  bind a `CpuScheduler` view for the complete local operation; affinity,
  home-CPU wakeup, bootstrap, and validation retain explicit target selection.
- Production bootstrap starts only the permanent idle thread and one kernel init
  thread; the latter launches and joins userspace `/init`.
- Context switching replaces the saved trap frame selected for IRQ or syscall
  return rather than using a normal function-call switch.
- Architecture entry owns each live exception frame through one non-null,
  exclusive `ActiveContext`. Generic syscall dispatch consumes a decoded
  six-argument request and has no AArch64 register-layout knowledge.
- Each occupied scheduler slot owns one inline, non-copyable `SavedContext`;
  free slots own none. Generic scheduling uses typed save, restore, entry, and
  fork construction without frame-word or register-layout knowledge.
- Raw context pointers and `TrapFrame` casts are confined to the AArch64 facade
  and assembly entry boundary. Context switching remains bounded and
  allocation-free.
- The architected timer runs in per-CPU one-shot nearest-deadline mode.
- `kernel::time` owns one preallocated, IRQ-safe deadline queue per logical
  CPU for exact wait deadlines and scheduler quantum expiration. Timed events
  carry their CPU owner; only that CPU inserts events and programs its physical
  timer. IRQ dispatch drops the queue owner before calling the scheduler facade
  directly.
- Reschedule requests coalesce in `kernel::sync::preempt`. Timer IRQ return and
  a private typed sched-call checkpoint may consume them only at disable depth
  zero; outermost guard release invokes that checkpoint automatically when the
  saved IRQ state is safe.
- Kernel yield under preemption exclusion returns to the same thread. Blocking
  waits and terminal thread/process transitions fail fast under a guard.
- Kernel thread slots, stacks, per-CPU ready queues, remote-ready ingress, and
  handles are bounded and generation-checked. Each thread has immutable
  `home_cpu` selected from `ThreadAttrs` before publication. A remote wake
  publishes into the home CPU's ingress, which only that CPU drains into its
  local ready queue; migration and IPI notification are not implemented.
- `ThreadId { index, generation }` directly indexes an occupied bounded slot;
  free and stale generations are rejected without a second scheduler identity.
- A free slot parks its preallocated kernel stack and next wait sequence. An
  occupied `Thread` owns that stack, `SavedContext`, lifecycle/join state,
  active wait metadata, and optional userspace resources.
- The private scheduler transition layer exclusively mutates thread state, slot
  generation, current identity, and ready-queue membership. Ready entries carry
  complete `ThreadId` generations; debug and QEMU-test builds run a bounded
  invariant validator after lifecycle transitions.
- Each blocking episode has a scheduler-owned `WaitToken` containing a complete
  `ThreadId`, immutable home `CpuId`, and a checked per-slot sequence. Inline
  wait metadata moves through `Prepared`, `Blocked`, and `Completed`; the
  sequence survives slot reuse.
- Wait-deadline events carry the exact `WaitToken`, while scheduler-quantum
  events carry `ThreadId`; both carry their queue-owning CPU. Stale generations
  and earlier waits by the same live thread cannot complete a later wait.
- Transition selection returns a context-free switch outcome. Context
  save/restore, TTBR0 activation, and switch logging remain in handoff code.
- Sleep, thread join, process wait, mailbox, and stdin condition owners publish
  and complete exact tokens through one prepare/publish/commit protocol. They
  retain condition payload and cleanup ownership; the scheduler owns only wait
  lifecycle and runnable visibility.

## Processes and userspace

- The bounded process table owns process state, TTBR0 address spaces, loaded
  ELF segments, cwd, file descriptors, relationships, exit/fault status, and
  `main_thread: ThreadId`.
- Each process-table slot contains a generation and one identity-independent
  `Process` aggregate. `ProcessResources` groups its address-space/image bundle
  with `ProcessFileState`; operation modules remain separate without changing
  ownership or synchronization semantics.
- A user thread owns its `OwnedUserStack` and retains only a non-owning
  `AddressSpaceId`. Scheduler code stores no `ProcessId` or process metadata;
  the process table resolves the current process in O(1) through a fixed
  thread-slot reverse index that validates `ThreadId`, `ProcessId`, and
  `main_thread` generations.
- `fork` eagerly clones the user address space and process resources.
- `execve` loads a static AArch64 ELF from ramfs, replaces the current user
  image and thread-owned stack, and builds bounded `argc`, `argv`, and `envp`.
- `process_join` and `waitpid` consume process-owned status, then use ordinary
  thread join/reap before releasing stack, ELF, and address-space resources.
  `waitpid` supports a specific positive child PID with options `0`.
- Lower-EL faults terminate the attributed user process and remain joinable;
  current-EL kernel faults stay fatal.
- The syscall ABI supports `open`, `read`, `write`, `close`, `getdents64`,
  `chdir`, `getcwd`, `fork`, `execve`, `waitpid`, and `exit` with negative errno
  returns.

## Filesystem and console

- QEMU loads a deterministic uncompressed `newc` initramfs into a reserved
  physical region. The kernel mounts it as a readonly ramfs index.
- Processes have bounded FD tables, immutable ramfs cwd identities, relative
  path traversal, readonly files, and directory iteration.
- `/init` is the freestanding shell. Product binaries are declared by
  `user/c/programs.toml` and installed under `/bin`.
- PL011 RX interrupts feed a bounded kernel stdin ring. `read(0)` blocks and is
  restarted after input; line editing and command policy live in userspace.
  Stdin, mailbox, and process owner state use IRQ-safe SMP locks; completion
  occurs after the owner is released. Logging enqueues complete records into a
  bounded allocation-free TX ring and one drainer performs UART polling outside
  the lock. Panic and test-abort output use a non-waiting emergency path.

## Verification and releases

- `cargo xtask check` is the host/build gate.
- `cargo xtask test-aarch64` runs declarative QEMU contracts.
- `cargo xtask ci` is the canonical merge gate.
- Machine assertions use the test-only `GTRT/1` protocol; human UART logs are
  diagnostic only.
- Test markers, protocol code, and test provenance are rejected from production
  kernel and initramfs artifacts.
- Release packaging dynamically tests exact production executables in
  controlled contract images, structurally verifies the release initramfs, and
  emits deterministic archives and checksums.

## Current boundaries

- Secondary CPUs reach generic registration and a controlled parked state, but
  have no local GIC CPU-interface/timer initialization and perform no normal
  scheduling, kernel-thread, or userspace execution.
- No IPI/remote reschedule or remote timer-insertion notification, migration,
  CPU hotplug, or userspace TLB shootdown. Shared runtime owners are already
  cross-CPU locked.
- No FP/SIMD context ownership; the soft-float target is intentional.
- No ASIDs, copy-on-write, demand paging, recoverable usercopy faults, signals,
  or multiple user threads within one process.
- No writable filesystem, VFS, storage driver, file metadata syscall set, or
  terminal line discipline.
- Kernel TTBR1 mutation remains limited to the current mapping granularity.
- Heap growth and comprehensive hardware latency certification are deferred.
