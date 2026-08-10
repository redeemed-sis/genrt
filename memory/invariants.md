# Cross-cutting invariants

These constraints are intentionally stable across subsystem implementations.
Changes that invalidate one require architecture review and an ADR.

## Architecture boundary

- `kernel/` remains architecture-neutral. Register encodings, exception mode
  details, MMIO addresses, and instruction-width assumptions stay in `arch/`.
- Hardware configuration comes from documented architecture behavior, the
  controlled platform protocol, or parsed firmware data. Do not guess it.
- AArch64 pre-MMU code and every dependency it reaches remain in `.boot.*`.
- Rust and assembly trap-frame layouts are one ABI and must change together.
- Generic kernel live-context APIs use an opaque, non-null, exclusive
  `ActiveContext`; syscall register decoding and live-frame mutation remain in
  the architecture layer.
- Live exception contexts and scheduler-saved contexts are distinct ownership
  domains. Each occupied scheduler slot owns exactly one inline, non-copyable
  `SavedContext`; free slots own none.
- Generic scheduler and process code neither expose raw context pointers nor
  inspect saved-frame layout. Representation casts and assembly pointers remain
  inside the documented architecture/FFI boundary.

## Real-time behavior

- Interrupt handlers, scheduler core, frame handoff, and timed-event dispatch
  do not allocate from the heap.
- Runtime structures touched by those paths are bounded and preallocated.
- Local-IRQ critical sections for IRQ-shared state are short and never contain
  unbounded parsing, user copies, filesystem traversal, or resource
  destruction. Thread-preemption sections may cover finite allocator traversal;
  their latency is not yet certified, but they do not suppress IRQ progress.
- Local IRQ exclusion protects state shared with interrupt handlers;
  thread-preemption exclusion protects thread-context-only state and must not be acquired
  from IRQ context. Nested thread-preemption exclusion preserves IRQ state and
  defers optional handoff until a depth-zero controlled scheduler checkpoint.
- Reschedule requests coalesce and are consumed only by scheduler checkpoints.
  Yield cannot bypass a guard, and blocking or terminal transitions fail fast
  while task preemption is disabled.
- Neither local IRQ nor thread-preemption exclusion provides SMP synchronization.
- Shared state uses Acquire/Release `SpinLock` or `IrqSpinLock`; the latter
  masks local IRQs before acquisition and releases before restoration. Guards
  are non-send and never cross blocking, scheduler handoff, user copy, parsing,
  heap allocation, logging bursts, or destructive cleanup. Permitted nesting is
  condition owner -> scheduler -> CPU-local time and kernel VM -> frame allocator;
  logging is terminal. CPU-registry and heap locks are standalone owners and
  are not nested with another shared lock.
- Blocking thread operations hand ownership to the scheduler; they do not poll.
- Human-readable logging is diagnostic and may perturb timing. It is never a
  functional test protocol.

## Memory ownership

- The frame allocator owns physical frames and does not imply a virtual alias.
- Boot-discovered memory metadata is immutable after initialization. Runtime
  frame free-list mutation is serialized, and no protected reference escapes
  the allocator guard.
- TTBR1 owns kernel high-half mappings; a process-owned TTBR0 root owns its EL0
  mappings.
- `OwnedUserAddressSpace` is the sole TTBR0 page-table owner. Scheduler state may
  retain only its non-owning `AddressSpaceId`, and the owner outlives every
  thread carrying that ID.
- Each user thread owns exactly one `OwnedUserStack`; the process table does not
  own user-stack frames.
- Boot-owned page tables are never reclaimed through the runtime frame
  allocator.
- Generic kernel VM entry points serialize runtime TTBR1 writers. AArch64 owns
  descriptor replacement, break-before-make where needed, inner-shareable
  TLBI, and barriers; table frames are not reclaimed before invalidation
  completes.
- Resource ownership is extracted atomically before frames, address spaces, or
  stacks are destroyed outside a critical section.
- User-copy helpers operate on the current active user address space unless an
  API explicitly states otherwise.

## Scheduling and lifecycle

- The active implementation is single-core. Local IRQ exclusion is not an SMP
  lock and must not be documented as one.
- Logical `CpuId` is the only generic-kernel CPU identity. AArch64-normalized
  hardware IDs are boot-registration keys, never scheduler indexes. Each
  registered CPU receives an architecture-owned logical binding; runtime
  current-CPU resolution reads that binding in O(1), and unbound or invalid
  CPUs never default to CPU0.
- Every CPU scheduler context owns its current thread, idle thread, ready queue,
  and initialized/online state. CPU-local preemption state is separate fixed
  storage so acquiring a shared `SpinLock` never needs the scheduler lock. A thread's
  immutable home CPU owns its ready membership and wakeup destination. A remote
  completion publishes into a bounded ingress for that CPU, and only the owner
  drains it into the local ready queue; no migration, IPI notification, or
  secondary execution exists yet.
- Every logical CPU owns one bounded deadline queue and one architected physical
  timer. Timed events carry that owner. Only the owner CPU inserts events and
  programs the timer; no time lock is retained across direct scheduler dispatch.
  Remote cancellation may leave an early interrupt, but cannot delay a remaining
  deadline. Remote insertion requires a future IPI command path.
- One local scheduler operation binds one stable `CpuId` through a
  `CpuScheduler` view. Target/home-CPU operations select their destination
  explicitly and must not silently substitute the executing CPU.
- Fixed CPU-local preemption backing is available before heap bootstrap and
  remains independent of shared scheduler storage afterward. A `PreemptGuard`
  is bound to its creating CPU and must not be dropped on another CPU. The
  current single-active-CPU assumption is not SMP exclusion.
- Thread and process handles use generations; stale handles never name a reused
  slot. `ThreadId` directly indexes one bounded `ThreadSlot`; there is no second
  schedulable identity.
- Scheduler lifecycle state, slot generation, current identity, and ready-queue
  membership change only through the scheduler transition layer. A non-idle
  `Ready` thread has exactly one current-generation queue entry, and the sole
  `Running` thread is exactly `current`.
- A free thread slot owns no `Thread`, `SavedContext`, or active wait. It parks
  the preallocated kernel stack and next wait sequence; an occupied `Thread`
  owns the stack, context, active wait, join/exit state, and optional user stack.
- Every blocking episode has one exact `WaitToken` containing a generation-aware
  `ThreadId`, immutable home `CpuId`, and checked per-slot sequence. The
  sequence allocator survives slot reuse; stale generation or sequence
  completions cannot affect a later wait.
- A thread is scheduler-blocked exactly when its inline wait metadata is
  `Blocked`. `Prepared` belongs only to the current running thread; `Completed`
  retains one first-wins cause until exact consumption.
- Condition owners publish tokens before scheduler commit and claim them before
  completion. No mailbox, process, thread, console, or time-owner lock crosses
  scheduler commit/completion, and the scheduler never calls back into those
  owners while mutating lifecycle state.
- Condition payload and loser cleanup remain owner-specific. Scheduler wait
  metadata contains no mailbox message, exit status, process result, or UART
  byte.
- Scheduler transition selection is separate from context handoff: transition
  code does not inspect architecture frame layout, and handoff code does not
  reopen thread lifecycle state.
- Scheduler code stores no `ProcessId`, process relationship, process status, or
  process-specific wait semantics. Process lookup and policy remain in the
  process layer.
- The process-owned thread-slot reverse index is updated atomically with
  `main_thread` publication and the process terminal transition. A later
  process-slot reclaim cannot clear an entry already reused by another thread
  generation. Lookup validates both handle generations; scheduler state does
  not own or inspect the index.
- Process callers use the `process` facade. The process table exclusively owns
  slots, generations, global table access, and the reverse index; scheduler
  code does not import process policy. Process address spaces/images remain
  process-owned, while user stacks remain thread-owned.
- Terminal thread and process status is single-consumer where a join/wait API
  promises one waiter.
- Wake paths make runnable work visible to the ready queue and rearm scheduling
  when the idle thread is running.
- Process cleanup reaps its main thread and releases `OwnedUserStack` before it
  frees ELF frames or destroys `OwnedUserAddressSpace`. Heavy resource
  destruction occurs after scheduler and local-IRQ exclusion.

## Userspace and ABI

- EL1 sched calls and EL0 syscalls are distinct ABIs even when both use `svc`.
- Syscall ABI changes require an ADR, userspace header changes, and contract
  tests using the exact production executable.
- Kernel code never directly trusts a user pointer; access goes through the
  bounded user-copy layer.
- Lower-EL faults are isolated to the attributed process when process ownership
  is known. Kernel faults remain fatal.

## Testing and releases

- `GTRT/1`, test artifact markers, supervisors, and fixture provenance are
  test-only and must not appear in production artifacts.
- QEMU pass/fail comes only from bounded machine protocol records, never prompt
  text, boot prose, or a hard-coded production directory listing.
- QEMU children are bounded by deadlines and are always terminated and reaped.
- Release binaries are the same production artifacts exercised by contract
  tests; packaging does not rebuild them afterward.
- Initramfs and release archives use canonical paths, sorted entries, fixed
  metadata, verified manifests, and content hashes.

## Engineering workflow

- Keep unrelated user changes and local artifacts untouched.
- Do not call hypothetical defensive hardening a blocker without a reachable
  current defect or violated acceptance criterion.
- Report only checks that actually ran, including commands that could not run.
- New or changed public and `pub(crate)` Rust APIs follow the repository rustdoc
  standard in `.agents/standards/rustdoc.md`.
