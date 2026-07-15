# ADR-0028: Typed saved context and scheduler ownership

## Status

Accepted

## Context

ADR-0027 introduced the lifetime-bound `ActiveContext` for a live exception
frame, but scheduler persistence still used `TaskFrameStorage`, a generic
`TASK_FRAME_WORDS` constant, raw word pointers, and manual frame copies.
Bootstrap, runtime spawn, fork, block, exit, timer preemption, and first entry
therefore shared an architecture-dependent representation inside generic
scheduler code.

The active AArch64 trap frame is 280 bytes, while exception entry reserves 288
bytes to preserve 16-byte stack alignment. Context transfer is a bounded full
frame copy and must remain allocation-free. Scheduler policy does not need to
know registers, frame width, or assembly offsets.

## Decision

- `ActiveContext<'a>` remains the exclusive borrowed handle to one live
  architecture exception frame.
- `SavedContext` is the owned persistent context retained inline by an occupied
  scheduler slot. It is neither `Copy` nor `Clone`; moving it transfers
  ownership.
- A free scheduler slot owns no saved context. Bootstrap and runtime task
  construction install exactly one context before a slot becomes runnable, and
  reclaim removes it when the slot becomes free.
- Generic scheduler code switches tasks only through typed `save_from`,
  `restore_into`, and `enter` operations. Kernel entry, userspace entry, and
  fork-child state use typed constructors.
- The generic architecture facade owns fixed, 16-byte-aligned, fully
  initialized inline storage: an exact 280-byte architecture payload plus an
  explicit initialized 8-byte alignment tail. No context operation allocates.
- The AArch64 adapter is the only Rust code that casts opaque storage or a live
  frame to `TrapFrame`. Compile-time assertions require the complete frame to
  fit and its alignment to be compatible with `SavedContext`.
- The final raw pointer is confined to architecture C ABI hooks and the existing
  assembly restore entry. The AArch64 `TrapFrame` and assembly layout do not
  change.
- Round-robin selection, scheduler states, block/wake ownership, IRQ exclusion,
  process ownership, syscall ABI, and userspace behavior remain unchanged.

## Invariants

- An occupied task slot owns exactly one initialized `SavedContext`; a free slot
  owns none.
- `ActiveContext` cannot outlive its exception entry, while `SavedContext`
  persists only through scheduler-slot ownership.
- Generic scheduler and process code cannot obtain raw context pointers or
  inspect architecture frame fields.
- Context construction, save, restore, fork derivation, and first entry are
  bounded and allocation-free.
- Every byte in `SavedContext` storage is initialized before architecture code
  writes or reads its `TrapFrame` prefix.
- Rust `TrapFrame` size/offsets and the assembly restore ABI remain one contract.

## Partial supersession

- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md):
  replaces boxed saved-frame backing only; preallocated containers and boxed
  task stacks remain unchanged.
- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md): replaces
  the temporary raw-word scheduler bridge only; the live `ActiveContext` and
  syscall boundary remain unchanged.

## Consequences

Scheduler ownership is visible in the type system instead of being implied by
a boxed word buffer. Bootstrap, spawn, fork, synchronous handoff, IRQ-return
preemption, and initial entry share one facade. AArch64 retains responsibility
for register semantics and the unsafe representation boundary.

The facade currently reserves an inline envelope sized for the active AArch64
exception-stack contract. Activating another architecture requires its adapter
to satisfy the same compile-time fit/alignment contract or deliberately revise
the envelope through a new reviewed boundary change.

Scheduler state-machine decomposition, IRQ/preemption-control separation,
FP/SIMD ownership, and changes to the assembly frame remain deferred.

## Alternatives considered

- Keep raw word storage behind scheduler helper functions: rejected because
  generic code would still own architecture width and pointer validity.
- Store `SavedContext` in a separate heap allocation: rejected because task
  slots already provide stable preallocated inline ownership and frame handoff
  must remain allocation-free.
- Put AArch64 `TrapFrame` directly in `Task`: rejected because it would make the
  generic scheduler architecture-dependent.
- Redesign the assembly frame or add FP/SIMD state now: rejected as unrelated
  ABI and ownership changes.

## Validation

- Build and post-link check the AArch64 kernel through `cargo xtask check`.
- Run all AArch64 QEMU contracts covering bootstrap, kernel spawn, block/wake,
  timer preemption, userspace entry, fork, syscall restart, and user fault exit.
- Audit generic source for `TASK_FRAME_WORDS`, `TaskFrameStorage`, frame-word
  APIs, manual saved-frame copies, and raw context pointers.
- Run formatting, host xtask tests/clippy, rustdoc, the canonical CI gate, link
  checks, and `git diff --check`.

## Related decisions

- [ADR-0003](ADR-0003-aarch64-preemptive-irq-return-switching.md)
- [ADR-0011](ADR-0011-dynamic-preallocated-scheduler-and-time-structures.md)
- [ADR-0014](ADR-0014-bounded-kernel-thread-lifecycle.md)
- [ADR-0016](ADR-0016-first-aarch64-el0-process.md)
- [ADR-0022](ADR-0022-fork-exec-waitpid-echo.md)
- [ADR-0027](ADR-0027-typed-active-context-and-syscall-boundary.md)
