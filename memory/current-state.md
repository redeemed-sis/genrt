# Current state

This document is a concise factual snapshot of genrt. Source code, tests, and
accepted ADRs remain authoritative when details differ.

## Active target

- Architecture: AArch64.
- Rust target: `aarch64-unknown-none-softfloat`.
- Machine: QEMU `virt` with GICv2.
- Execution model: single core, EL1 kernel and freestanding EL0 processes.
- Kernel image: low physical load with a high-half virtual runtime mapping.

## Boot and platform

- A low-linked `.boot.*` trampoline parses the QEMU-provided DTB, constructs
  bootstrap translation tables, enables the MMU, and enters high-linked Rust.
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
  runtime physical frame allocator use thread-context-only `PreemptLock`. Its nested
  disable depth permits IRQ progress, forbids thread handoff until unlock, and is
  forbidden in IRQ paths.
- Boot-discovered physical regions and the heap range are immutable after
  initialization. Runtime free-list state is separately locked and does not
  expose references outside its guard.
- Scheduler and timed-event containers allocate and reserve capacity before
  entering IRQ-sensitive operation.
- Runtime TTBR1 APIs map, unmap, protect, and translate kernel regions after
  the boot tables have been replaced.
- Each user process owns an allocator-backed `OwnedUserAddressSpace`. ELF
  segments and thread-owned user stacks use 4 KiB mappings with user-specific
  permissions.
- `copy_from_user` and `copy_to_user` validate the active user address space;
  fault recovery during the actual copy is not implemented yet.

## Scheduling and time

- The scheduler is round-robin, preemptive, and single-core.
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
- The architected timer runs in one-shot nearest-deadline mode.
- `kernel::time` owns the preallocated deadline queue for exact wait deadlines
  and scheduler quantum expiration.
- Reschedule requests coalesce in `kernel::sync::preempt`. Timer IRQ return and
  a private typed sched-call checkpoint may consume them only at disable depth
  zero; outermost guard release invokes that checkpoint automatically when the
  saved IRQ state is safe.
- Kernel yield under preemption exclusion returns to the same thread. Blocking
  waits and terminal thread/process transitions fail fast under a guard.
- Kernel thread slots, stacks, ready queues, and handles are bounded and
  generation-checked.
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
  `ThreadId` and a checked per-slot sequence. Inline wait metadata moves through
  `Prepared`, `Blocked`, and `Completed`; the sequence survives slot reuse.
- Wait-deadline events carry the exact `WaitToken`, while scheduler-quantum
  events carry `ThreadId`. Stale generations and earlier waits by the same live
  thread cannot complete a later wait.
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

- No SMP scheduling, cross-core synchronization, or TLB shootdown.
- No FP/SIMD context ownership; the soft-float target is intentional.
- No ASIDs, copy-on-write, demand paging, recoverable usercopy faults, signals,
  or multiple user threads within one process.
- No writable filesystem, VFS, storage driver, file metadata syscall set, or
  terminal line discipline.
- Kernel TTBR1 mutation remains limited to the current mapping granularity.
- Heap growth and comprehensive hardware latency certification are deferred.
