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
- Heap allocation is permitted during bootstrap and task context, protected
  against local IRQ reentrancy.
- Scheduler and timed-event containers allocate and reserve capacity before
  entering IRQ-sensitive operation.
- Runtime TTBR1 APIs map, unmap, protect, and translate kernel regions after
  the boot tables have been replaced.
- Each user process owns an allocator-backed TTBR0 root. ELF segments and the
  user stack use 4 KiB mappings with user-specific permissions.
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
- `kernel::time` owns the preallocated deadline queue for wakeups, mailbox
  timeouts, and scheduler quantum expiration.
- Kernel thread slots, stacks, ready queues, and handles are bounded and
  generation-checked.
- Sleep, thread join, process wait, mailbox waits, and stdin waits block through
  scheduler-owned state transitions rather than polling.

## Processes and userspace

- The bounded process table owns process state, TTBR0 address spaces, loaded
  ELF segments, user stacks, cwd, file descriptors, and exit/fault status.
- `fork` eagerly clones the user address space and process resources.
- `execve` loads a static AArch64 ELF from ramfs, replaces the current user
  image, and builds bounded `argc`, `argv`, and `envp` data on the new stack.
- `waitpid` supports a specific positive child PID with options `0`.
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
