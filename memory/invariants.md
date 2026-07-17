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
  destruction. Task-preemption sections may cover finite allocator traversal;
  their latency is not yet certified, but they do not suppress IRQ progress.
- Local IRQ exclusion protects state shared with interrupt handlers;
  task-preemption exclusion protects task-only state and must not be acquired
  from IRQ context. Nested task-preemption exclusion preserves IRQ state and
  defers optional handoff until a depth-zero controlled scheduler checkpoint.
- Reschedule requests coalesce and are consumed only by scheduler checkpoints.
  Yield cannot bypass a guard, and blocking or terminal transitions fail fast
  while task preemption is disabled.
- Neither local IRQ nor task-preemption exclusion provides SMP synchronization.
- Blocking task operations hand ownership to the scheduler; they do not poll.
- Human-readable logging is diagnostic and may perturb timing. It is never a
  functional test protocol.

## Memory ownership

- The frame allocator owns physical frames and does not imply a virtual alias.
- Boot-discovered memory metadata is immutable after initialization. Runtime
  frame free-list mutation is serialized, and no protected reference escapes
  the allocator guard.
- TTBR1 owns kernel high-half mappings; a process-owned TTBR0 root owns its EL0
  mappings.
- Boot-owned page tables are never reclaimed through the runtime frame
  allocator.
- Resource ownership is extracted atomically before frames, address spaces, or
  stacks are destroyed outside a critical section.
- User-copy helpers operate on the current active user address space unless an
  API explicitly states otherwise.

## Scheduling and lifecycle

- The active implementation is single-core. Local IRQ exclusion is not an SMP
  lock and must not be documented as one.
- Task, thread, and process handles use generations; stale handles never name a
  reused slot.
- Scheduler lifecycle state, slot generation, current identity, and ready-queue
  membership change only through the scheduler transition layer. A non-idle
  `Ready` task has exactly one current-generation queue entry, and the sole
  `Running` task is exactly `current`.
- Scheduler-owned timed wake, quantum, and timeout events carry generation
  identity; an event for a reclaimed thread cannot affect a reused slot.
- Scheduler transition selection is separate from context handoff: transition
  code does not inspect architecture frame layout, and handoff code does not
  reopen task lifecycle state.
- Terminal thread and process status is single-consumer where a join/wait API
  promises one waiter.
- Wake paths make runnable work visible to the ready queue and rearm scheduling
  when the idle task is running.

## Userspace and ABI

- EL1 task calls and EL0 syscalls are distinct ABIs even when both use `svc`.
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
