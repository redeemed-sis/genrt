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
  The operator also supplied physical CPU1/CPU3 userspace benchmark results;
  repeated cold boots and firmware reset/halt still require operator checks.
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
  Per-CPU active-root tracking prevents destruction before detach. Reusing the
  same installed root avoids a register write and local invalidation. Permanent
  high-half idle retains it without accessing user pointers; ordinary kernel
  threads clear it. User exit detaches before reap can reclaim its frames.
  Changing roots still invalidates locally; shared runtime TTBR1 mutation
  retains inner-shareable broadcast invalidation.
- `copy_from_user` and `copy_to_user` validate the active user address space;
  fault recovery during the actual copy is not implemented yet.
  Full-range validation precedes aligned volatile word copies with byte edges;
  C-string scans remain bytewise. Successful SVC dispatch avoids diagnostic
  frame copies and FAR reads, without changing the full exception-frame ABI.

## Scheduling and time

- Latest user-reported Pi CPU3 results after raising configured CPU frequency:
  full profiling at a 15000 ns period reports release average/maximum
  2894/9219 ns, TET 573/625 ns, 10000 complete idle cycles, no early/lost/
  incomplete cycles, no overruns, and result 3000000. Sequential interval means
  estimate 5.64 us of submission plus IRQ-to-EL0, not a paired maximum.
  Ten ordinary-build runs without `wake-profile` at a 6000 ns period report
  release averages 1319..1323 ns and overall maximum 5614 ns, with one 187 ns
  overrun across 100000 measured steps. Runtime ARM rate and deployed FIT
  identities were not supplied; different periods/builds do not quantify
  profiler overhead or prove the round-trip maximum target. See the
  [full profile](../docs/wake-profiling.md#higher-cpu-frequency-user-reported)
  and [ordinary-build summaries](../docs/testing.md#raspberry-pi-hardware-result-user-reported).
- Committed Raspberry Pi 3B+ hardware reference (reported 2026-10-16): CPU3,
  `taskset 3 rtbench -p 15000 -n 10000 -o 300 -P`, full profiling at
  19200000 Hz. Release latency averages 6169 ns, maximum 12917 ns; TET averages
  1134 ns, maximum 1198 ns. All 10000 cycles are complete, with 9999 idle wakes,
  one early return, no lost/incomplete records, no overruns, and result 3000000.
  Idle deadline-to-IRQ averages 52 ns and deadline-to-first-EL0 averages
  6065 ns. Sequential idle interval means estimate 12262 ns of work excluding
  suspension, not an observed maximum or an uninstrumented baseline. This
  user-reported result follows ADR-0058, not controlled per-change attribution.
  See the [recorded reference](../docs/wake-profiling.md#recorded-pi-reference-user-reported)
  and [active measurement target](../docs/roadmap/hardening.md#high-value-architecture-work).
- The uncommitted CPU-local scheduler/profiler, usercopy-cache, and timer
  optimization experiment was reverted. Its final user-reported release
  average/maximum was 6063/14271 ns with one 417 ns overrun; estimated idle-path
  work was 12538 ns, compared with 12262 ns for the committed reference.
  Preserve the [experimental measurements](../docs/wake-profiling.md#follow-up-pi-results-user-reported)
  as historical evidence, not current-runtime behavior or a measured speedup.
  The target remains an observed maximum below 5000 ns for submission plus
  IRQ-to-EL0 excluding intentional waiting; release latency is not that metric.
- The narrow follow-up removes trace-only queue counter reads and unused timer
  snapshots, defers RR clock sampling until a new quantum is needed, and consumes
  the staged SVC owner once at absolute-sleep admission. Scheduler/profiler locks,
  wait identity, userspace ABI, and frame layout are unchanged. The user reported
  10000 complete idle cycles with no overruns: release average/maximum
  6075/12344 ns and estimated mean work excluding suspension 12177 ns. This is
  instrumented user-reported evidence, not an observed round-trip maximum or
  controlled per-change speedup; deployed FIT identity was not supplied.
- Active wait metadata now retains one exact token while a separate phase
  changes. Clock sleep bypasses the out-of-line dispatcher for buffered handlers;
  ABI and lock ownership are unchanged. Agent-executed Pi UART checks of both
  diagnostic FITs estimated mean work excluding suspension at 12157..12181 ns
  before and 11358..11425 ns after. The [recorded comparison](../docs/wake-profiling.md#wait-layout-and-dispatch-hardware-check-agent-executed)
  retains all three runs, overruns, and uploaded FIT identities; it does not
  establish a sub-5-us observed maximum. The ordinary follow-up FIT also passed
  physical boot and CPU1/CPU2 smoke; three uninstrumented runs reported release
  means 2690..2693 ns and maxima 6927..8385 ns without overruns. No ordinary
  before-FIT comparison or combined round-trip maximum was collected.
- Agent-executed focused submission checks on 2026-10-18 used the same Pi
  diagnostic FIT and alternated full collection with `-F 0:4`. Focused
  SVC-to-handoff means were 5449..5453 ns and paired maxima 12395..12916 ns,
  versus full-mode mean estimates 5566..5575 ns. No runtime code changed:
  focused selection suppresses unselected Rust timestamps, not hook locking
  or lifecycle bookkeeping. Ordinary-kernel submission remains unmeasured.
  See the [focused evidence and limits](../docs/wake-profiling.md#focused-submission-check-agent-executed).
- EL0 sleep uses one scheduler transaction for automatic wait preparation,
  exact timer publication, and commit. Time ownership ends before handoff.
  Usercopy obtains its identity from VM-owned CPU-local installed-root and
  user-access publication, without a scheduler lookup. Idle retains root
  lifetime but not usercopy eligibility. AArch64 saved/live transfers use a
  fixed GP-register copy of the full payload, preserving idle and frame ABI.
  See [ADR-0058](decisions/ADR-0058-bounded-sleep-and-vm-owned-usercopy.md).
- POSIX sleep uses one time-owner sleep/quantum publication transaction.
  Single-page user reads use architecture EL0 permission translation with the
  VM active-owner check; crossing-page reads and writes retain software walks.
  See [ADR-0059](decisions/ADR-0059-native-counter-sleep-and-active-read-validation.md).
- The native counter-sleep syscall, public helper/header, and rtbench `-C`
  trial interface have been removed. POSIX sleep, internal counter deadlines,
  active-read validation, and batched time publication remain unchanged.
  See [ADR-0060](decisions/ADR-0060-retire-native-counter-sleep-interface.md).
- The retired 2026-10-18 native-sleep trial measured focused submission
  means of 4881..4894 ns with `-C`, versus 5298..5331 ns for POSIX on the same
  diagnostic image. POSIX maxima did not improve; the software-walk control
  retained long tails. No sub-5-us round-trip maximum is claimed. Firmware ARM
  measured-rate queries in U-Boot returned 600 MHz before/after diagnostic boots,
  not a runtime frequency trace. See the [historical evidence](../docs/wake-profiling.md#retired-counter-sleep-experiment-agent-executed).
  On the separate ordinary FIT, three alternating POSIX/native pairs returned
  release means 2643..2683 ns without overruns; these are not round-trip profiles.
- Optional `wake-profile` diagnostics correlate absolute userspace sleeps with
  exact wait identities and raw counter phase observations. Configurable
  heap storage is prepared before START; active collection stays bounded and
  allocation-free. STOP freezes owner-only byte-offset retrieval. Full and
  selected-pair modes distinguish early returns, direct idle wakes, and generic
  selection. Diagnostic artifacts stay separate from ordinary release inputs.
  After STOP, rtbench streams frozen records into fixed per-path aggregates and
  prints interval counts, averages, and maxima directly in the console. There
  is no raw UART export or separate host analyzer. Report sections separate
  execution stages from overlapping totals and deadline references.
  See [profiling procedure](../docs/wake-profiling.md) and
  [ADR-0056](decisions/ADR-0056-bounded-sleep-wakeup-phase-profiling.md).
- Public monotonic observation/resolution use an EL0 physical-counter fast
  path in the shared library; raw clock syscalls retain pointer validation.
  All runtime CPUs permit counter/frequency reads, not timer programming.
  Kernel and userspace conversions retain the same epoch and rounding.
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
  time source. Representable deadline products use checked native-width
  arithmetic rather than software 128-bit division; wider inputs retain the
  exact fallback and the same overflow and ceiling semantics.
- `kernel::time` owns one preallocated, IRQ-safe deadline queue per logical
  CPU for exact wait deadlines and scheduler quantum expiration. Timed events
  carry their CPU owner; only that CPU inserts events and programs its physical
  timer. Each CPU also owns a fixed expired-event transfer buffer allocated at
  bootstrap. IRQ dispatch releases the queue before one scheduler transaction
  handles the batch and return selection, then applies quantum intent and rearms
  after releasing the scheduler. An enabled-preemption idle/single-wait path
  skips ready-queue insertion and RR work; multi-event/peer paths retain normal
  first-wins completion and round-robin behavior. Idle contexts remain saved.
  Missing event cancellations do not reprogram the timer; remote or IRQ-batch
  cancellation still leaves programming to the owning CPU's timer transaction.
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
  The common ELF builder applies `-O0` to debug and `-O2` to release programs,
  CRT0, and library members, retaining freestanding and GP-register-only flags.
- `/bin/rtbench` uses the existing monotonic time/sleep ABI for absolute
  periodic releases that never reset after overruns. Required decimal options
  select a nonzero u64 period, 1..10000 steps, and 0..UINT32_MAX unsigned-add
  payload operations per step, preserved by a register compiler barrier.
  One process-private 240000-byte `.bss` sample array replaces heap or large
  stack storage. Release latency is S-R; elapsed TET is E-S, including
  preemption; only E greater than the next release is an overrun. Each
  non-overrun iteration includes an absolute sleep, even after the final
  sample, with preflight horizon R[0]+period*steps. No normal measurement I/O
  occurs. Checked timestamps, horizon/release arithmetic, and post-run sums
  fail rather than wrap. Summary averages/maxima/overrun count and deterministic
  result are emitted afterward; per-STEP rows require verbose `-v`. CLI errors
  exit 2 and runtime/arithmetic/output failures exit 1. Host tests execute the utility
  with controlled time/output, and existing QEMU contracts cover exact product
  execution and CPU1 placement. The [unprofiled hardware results](../docs/testing.md#raspberry-pi-hardware-result-user-reported)
  record user-supplied physical Pi CPU3 runs at 8000 ns and 15000 ns periods,
  each with 1000 steps and 30 operations. Hardware observations remain
  separate from emulator verification and certified timing bounds.
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
- A mount-aware VFS owns pathname traversal and dispatch to filesystem
  backends. Ramfs remains the archive-backed readonly root; mounts are
  kernel-controlled and cannot be dynamically unmounted.
- Processes have bounded FD tables and mount-aware cwd references. Fork shares
  open file descriptions and file/directory positions; exec preserves them.
  Backend work and final resource destruction run outside process/scheduler
  critical sections. Pending I/O uses exact waits and syscall restart rather
  than blocking beneath a borrowed live frame.
- Open provisions reusable request/staging storage per OFD, with a 4096-byte
  baseline. Read/write reuse it across calls and waits; pending call metadata is
  inline rather than separately boxed. Capacity grows only beyond the baseline
  when insufficient, without changing the 1024-byte syscall usercopy bound.
  See [ADR-0063](decisions/ADR-0063-reusable-open-description-io-storage.md).
- A test-only second backend exercises generated and writable files, independent
  mounts, pending completion, and process ownership through the same VFS
  interfaces. It is absent from production artifacts. See
  [ADR-0062](decisions/ADR-0062-mount-aware-vfs-and-shared-open-descriptions.md).
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
- `dist` additionally emits a versioned Raspberry Pi 3B+ FIT and manifest,
  covered by the shared `SHA256SUMS`. The Pi kernel receives structural,
  composition, and FIT checks, not an automated physical boot claim. Its
  initramfs is byte-identical to the QEMU release's verified production CPIO;
  no contract-tested userspace is rebuilt. Metadata records platform features,
  component hashes, hardware tools, and the verification scope. See
  [ADR-0061](decisions/ADR-0061-raspberrypi-fit-release-artifacts.md).

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
- No production writable filesystem, persistent storage driver, file metadata
  syscall set, dynamic unmount, or terminal line discipline.
- Kernel TTBR1 mutation remains limited to the current mapping granularity.
- Heap growth and comprehensive hardware latency certification are deferred.
