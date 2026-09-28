# Current state

This document is a concise factual snapshot of genrt. Source code, tests, and
accepted ADRs remain authoritative when details differ.

## Active target

- Architecture: AArch64.
- Rust target: `aarch64-unknown-none-softfloat`.
- Machine: QEMU `virt` with GICv2.
- Execution model: CPU0-owned global initialization and device IRQs plus
  bounded high-half bring-up of every registered CPU into an active scheduler
  context. Independent userspace processes may have any online CPU as owner.
- Kernel image: low physical load with a high-half virtual runtime mapping.

## Hardware bring-up

- Hardware bring-up also targets Raspberry Pi 3B+ through installed 64-bit
  U-Boot. The operator reports rebuilding U-Boot with FIT support after the
  previous loader rejected an intact FIT. The primary documented procedure
  now uploads `target/raspberrypi-3b-plus/genrt-raspberrypi-3b-plus.itb` at
  `0x01000000` and uses `bootm`; the linked kernel base is `0x02000000`.
  `cargo xtask build-raspberrypi-3b-plus` includes production initramfs in
  the FIT. Reported RAM ends at 0x3b400000, with upper U-Boot reservations
  starting at 0x39f39000.
  The allocator is capped below 128 MiB and the first MiB is firmware-reserved.
- Pi FIT components use CRC32 to avoid requiring SHA-256 in the installed
  loader; host verification checks both component bytes and CRC32 values.
  Release SHA-256 checks remain unchanged.
- The BCM2837 boot mapper takes its peripheral window from DTB `soc/ranges`.
  The runtime UART resolves GPIO, PL011, AUX and mini-UART `reg` addresses
  through that window, then adopts enabled GPIO14/15 ALT0 PL011 or ALT5 mini
  UART without changing firmware clocks, baud divisors, or pinmux.
  The Pi runtime's shared entry drops EL2 to EL1, initializes CPU0 GPU IRQ
  routing and per-CPU ARM-local timers/mailbox IPIs,
  and starts CPU1..3 through DTB-validated spin-table words at e0/e8/f0.
  Each CPU enters the existing scheduler. Pi reset/halt uses the PM watchdog
  and firmware halt flag, not PSCI; halt does not remove board power.
  The kernel consumes the relocated /chosen initramfs extent. Executable page
  publication performs Pi cache maintenance before local or remote execution.
  Mock-MMIO tests cover both UARTs, per-CPU IRQ/mailbox routing and PM writes.
  BCM2837 emulation reached /init, ran taskset on CPU2/3, slept and powered off.
  Physical runtime, repeated cold boots and firmware reset/halt still require
  operator checks.
  See [Pi procedure](../arch/aarch64/platform/raspberrypi-3b-plus/README.md) and
  [ADR-0051](decisions/ADR-0051-raspberrypi-runtime-spin-table-and-power.md).

## Boot and platform

- A low-linked `.boot.*` trampoline parses the QEMU-provided DTB, constructs
  bootstrap translation tables, enables the MMU, and enters high-linked Rust.
- The runtime DTB supplies the enabled CPU count, hardware affinity targets,
  and PSCI HVC CPU_ON metadata. CPU0 starts secondaries sequentially only after
  runtime TTBR1 is active and publishes that root with Release ordering.
- Four fixed linker-owned 32 KiB bootstrap-stack slots bound the current QEMU
  topology. Every secondary enters low `secondary_start` through PSCI, enables
  the MMU with boot TTBR0 plus runtime TTBR1, enters high Rust, clears its local
  TTBR0, installs VBAR, and initializes local GICC/timer PPI/physical timer
  state. It then enters one generic secondary function that binds logical
  identity, initializes idle/current scheduler state, and enters that idle
  thread through its saved frame. CPU0 likewise performs its local architecture
  setup before `kernel_main`; generic code never invokes either CPU's interrupt
  setup or chooses DAIF.
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
- User address spaces are mapped through an unpublished
  `StagedUserAddressSpace`, then frozen into a process-owned
  `OwnedUserAddressSpace` with one immutable logical CPU owner. ELF segments
  and thread-owned user stacks use 4 KiB mappings with user-specific
  permissions.
- A published TTBR0 root is immutable and activates only on its owner CPU.
  Per-CPU active-root tracking prevents destruction before clear. AArch64 uses
  local invalidation for TTBR0 activation/clear while shared runtime TTBR1
  mutation retains inner-shareable broadcast invalidation.
- `copy_from_user` and `copy_to_user` validate the active user address space;
  fault recovery during the actual copy is not implemented yet.

## Scheduling and time

- The scheduler is round-robin and preemptive. A bounded registry
  maps normalized AArch64 hardware identities to CPU0-first logical `CpuId`
  values before scheduler bootstrap. AArch64 retains each logical binding in
  CPU-local `TPIDR_EL1`, making runtime current-CPU resolution O(1); unbound or
  invalid CPUs are rejected rather than treated as CPU0.
- Scheduler contexts are preallocated per logical CPU. Each owns local current,
  idle, ready queue, and initialized/online state; separate CPU-local fixed
  preemption backing keeps lock acquisition independent of shared lifecycle
  storage.
  CPU0 preflights capacity for one idle slot per expected CPU. Every registered
  CPU initializes local idle/current state and becomes online only at first
  saved-frame entry. Runtime scheduler entry points resolve the
  executing CPU once and bind a `CpuScheduler` view for the complete local operation; affinity,
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
- The same architecture counter is the userspace `CLOCK_MONOTONIC` domain.
  Checked nanosecond conversion provides normalized observation and resolution,
  relative sleeps, and drift-free `TIMER_ABSTIME` deadlines without a second
  time source.
- `kernel::time` owns one preallocated, IRQ-safe deadline queue per logical
  CPU for exact wait deadlines and scheduler quantum expiration. Timed events
  carry their CPU owner; only that CPU inserts events and programs its physical
  timer. IRQ dispatch drops the queue owner before calling the scheduler facade
  directly.
- AArch64 selects one platform interrupt-controller implementation through a
  static contract, with no IRQ-time dynamic dispatch. The common dispatcher
  handles UART, timer, and scheduler notifications; QEMU GICv2 and Pi local
  controllers classify sources and complete their own acknowledgement rules.
  QEMU register ownership lives in `arch/aarch64/src/platform/gic.rs`.
  Generic `PlatformInfo` stores a typed controller MMIO map, not GIC fields;
  the low boot handoff keeps two fixed, controller-neutral range slots.
- On QEMU, CPU0 alone initializes the shared GIC distributor and routes device SPIs.
  Architecture entry initializes each CPU's vector base, banked GICC interface,
  physical-timer PPI, and local `CNTP_*` state. CPU0 preallocates all generic
  deadline queues before PSCI startup. Generic secondary scheduler
  initialization occurs only after local setup, without a duplicate
  interrupt-readiness flag. Secondary timer IRQs are attributed to their
  logical CPU and may make local scheduler handoff.
- Reschedule requests coalesce in `kernel::sync::preempt`. Timer IRQ return and
  a private typed sched-call checkpoint may consume them only at disable depth
  zero; outermost guard release invokes that checkpoint automatically when the
  saved IRQ state is safe.
- Kernel yield under preemption exclusion returns to the same thread. Blocking
  waits and terminal thread/process transitions fail fast under a guard.
- Kernel thread slots, stacks, per-CPU ready queues, remote-ready ingress, and
  handles are bounded and generation-checked. Each thread has immutable
  `home_cpu` selected from `ThreadAttrs` before publication. A remote wake
  publishes into the home CPU's ingress and raises one coalesced targeted
  scheduler SGI. Only that CPU drains ingress into its local ready queue and
  consumes the request through the existing IRQ-return checkpoint. Idle and
  sole-current contexts do not retain a polling quantum; RR deadlines exist
  only while a local runnable peer exists. Migration and remote timer insertion
  are not implemented.
- `ThreadId { index, generation }` directly indexes an occupied bounded slot;
  free and stale generations are rejected without a second scheduler identity.
- A free slot parks its preallocated kernel stack and next wait sequence. An
  occupied `Thread` owns that stack, `SavedContext`, lifecycle/join state,
  active wait metadata, and optional userspace resources.
- After exit selection, the outgoing thread remains occupied in one CPU-local
  retired slot until the next scheduler entry proves execution moved to the new
  kernel stack. Only then can join completion or detached reap publish that
  stack in the free-slot pool. A coalesced self-targeted scheduler SGI guarantees
  that later entry without depending on a timer deadline.
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
- Blocking userspace clock sleeps use automatic completion consumption because
  EL0 resumes directly after `svc`; other waits retain explicit owner-side
  completion consumption.

## Processes and userspace

- All userspace ELF programs statically link the common freestanding support
  library. Generic string/memory, integer formatting, short-option parsing,
  and checked decimal conversion are shared by products and tests; the write
  syscall adapter remains architecture-owned. This adds no kernel syscall,
  heap runtime, TLS, file streams, or dynamic linker.
- The bounded process table owns process state, TTBR0 address spaces, loaded
  ELF segments, cwd, file descriptors, relationships, exit/fault status, and
  `main_thread: ThreadId`.
- Every live process, its main thread, and its `AddressSpaceId` share one
  immutable `CpuId`. Process binding occurs in a bounded pre-ready publication
  hook before the scheduler exposes the exact thread ID in a ready queue or
  remote ingress.
- Each process-table slot contains a generation and one identity-independent
  `Process` aggregate. `ProcessResources` groups its address-space/image bundle
  with `ProcessFileState`; operation modules remain separate without changing
  ownership or synchronization semantics.
- A user thread owns its `OwnedUserStack` and retains only a non-owning
  `AddressSpaceId`. Scheduler code stores no `ProcessId` or process metadata;
  the process table resolves the current process in O(1) through a fixed
  thread-slot reverse index that validates `ThreadId`, `ProcessId`, and
  `main_thread` generations.
- `fork` eagerly clones the user address space and process resources. By
  default the child keeps the parent's owner; `sched_setforkaffinity` may set,
  replace, or reset a process-owned one-shot CPU for the next successfully
  published child. Failed staging preserves it and children never inherit it.
- `sched_getaffinity` reports that immutable owner as exactly one bit in a
  fixed 1024-bit userspace `cpu_set_t`. PID `0` selects the caller; positive
  generation-bearing PIDs may select another unreaped process.
- `execve` loads a static AArch64 ELF from ramfs, preserves the process owner,
  replaces the current user image and thread-owned stack, and builds bounded
  `argc`, `argv`, and `envp`.
- `process_join` and `waitpid` consume process-owned status, then use ordinary
  thread join/reap before releasing stack, ELF, and address-space resources.
  `waitpid` supports a specific positive child PID with options `0`.
- Lower-EL faults terminate the attributed user process and remain joinable;
  current-EL kernel faults stay fatal.
- The syscall ABI supports `open`, `read`, `write`, `close`, `getdents64`,
  `chdir`, `getcwd`, `fork`, `execve`, `waitpid`, `sched_setforkaffinity`,
  `sched_getaffinity`, `clock_gettime`, `clock_getres`, `clock_nanosleep`,
  Linux-shaped `reboot`, and `exit` with negative errno returns. Restart and
  power off cross an allocation-free generic power
  boundary into AArch64 PSCI and are terminal on success; all processes may
  currently request them because credentials are not implemented.

## Filesystem and console

- QEMU loads a deterministic uncompressed `newc` initramfs into a reserved
  physical region. The kernel mounts it as a readonly ramfs index.
- Processes have bounded FD tables, immutable ramfs cwd identities, relative
  path traversal, readonly files, and directory iteration.
- `/init` is the freestanding shell. Product binaries are declared by
  `user/c/programs.toml` and installed under `/bin`, including `sleep`,
  `reboot`, and `poweroff`.
- The shell has a fixed 16-entry command history with draft restoration and
  first-token Tab completion for built-ins and `/bin` ELF commands. Editor
  state and directory scanning use fixed storage; the existing 256-byte line
  and 16-argument limits remain. Host editor tests and the production-shell
  QEMU contract cover history, completion, bounds, and escape recovery.
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
- Release bundles run without a source checkout through generated `run.sh`:
  one CPU by default or `-c 1..4` with matching packaged DTBs. Launcher and
  standalone guide commands share the canonical QEMU configuration. Every
  payload is hashed; tar metadata includes executable launcher permissions.

## Current boundaries

- Secondary CPUs execute permanent idle, explicitly pinned kernel threads, and
  independently owned user processes. They acknowledge/EOI local timer and
  targeted scheduler SGI interrupts; device SPIs remain CPU0-owned.
- There is no migration, CPU hotplug, remote timer insertion, shared userspace
  address space, ASID support, or userspace TLB shootdown. Shared runtime owners
  are cross-CPU locked and remote-ready publication uses targeted scheduler
  IPIs.
- No FP/SIMD context ownership; the soft-float target is intentional.
- No ASIDs, copy-on-write, demand paging, recoverable usercopy faults, signals,
  or multiple user threads within one process.
- No writable filesystem, VFS, storage driver, file metadata syscall set, or
  terminal line discipline.
- Kernel TTBR1 mutation remains limited to the current mapping granularity.
- Heap growth and comprehensive hardware latency certification are deferred.
