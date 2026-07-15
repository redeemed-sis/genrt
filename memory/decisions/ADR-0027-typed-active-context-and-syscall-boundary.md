# ADR-0027: Typed Active Context and Syscall Boundary

## Status

Accepted

## Context

The AArch64 exception layer saved a complete `TrapFrame`, but generic kernel
syscall, process, console, task-call, IPC, time, and scheduler interfaces passed
that live frame as `*mut u64`. Generic syscall dispatch decoded AArch64 `x8` and
`x0..x5` directly, wrote results to `x0`, and depended on architecture helpers
for fixed-width SVC restart and exec frame replacement.

That representation obscured the exclusive lifetime of the active exception
frame and allowed architecture register and instruction assumptions to cross
the generic kernel boundary. Scheduler-owned saved frames still require their
existing word representation and architecture clone/initialization hooks, so a
complete saved-context redesign is a separate hardening milestone.

## Decision

- `kernel::arch::ActiveContext<'a>` is the generic kernel's opaque handle to one
  live exception frame. It is neither `Copy` nor `Clone`, contains a non-null
  opaque pointer, and carries an exclusive mutable lifetime.
- The architecture entry layer constructs the context through a documented
  unsafe constructor accepting `NonNull<c_void>`. The constructor contract
  requires one live, writable frame with no competing frame access for the
  context lifetime.
- Generic live-context APIs pass `&mut ActiveContext<'_>` through syscall,
  process, console, task-call, IPC, time, and scheduler handoff paths.
- The facade owns generic operations for setting a syscall result, restarting
  the current syscall, and replacing userspace state after exec. AArch64
  implements register, ELR, instruction-width, `kernel_sp`, and `TrapFrame`
  details behind those operations.
- `SyscallRequest` carries a private syscall number and six arguments. The
  AArch64 adapter alone decodes `x8` and `x0..x5`; generic handlers consume only
  request accessors.
- AArch64 IRQ, controlled current-EL synchronous entry, and lower-EL synchronous
  entry create at most one active context for each live exception frame and
  pass it through the complete generic handling path.
- `TaskFrameStorage`, `TASK_FRAME_WORDS`, saved-frame initialization/clone hooks,
  frame-copy logic, and assembly layout remain unchanged. A documented
  crate-only bridge exposes active frame words only to low-level scheduler
  saved-frame copy and fork-clone code.

## Invariants

- Generic syscall code contains no architecture register indices, trap-frame
  dereference, resume-PC arithmetic, or `kernel_sp` knowledge.
- An `ActiveContext` never represents null and cannot outlive or be duplicated
  for its architecture-owned live frame.
- Raw live-frame words are not accepted by syscall, console, process, task-call,
  IPC, or time APIs. Temporary raw access remains confined to low-level
  scheduler saved-frame handoff and AArch64 saved-frame hooks.
- IRQ, timed-event, and scheduler handoff paths remain allocation-free and do
  not gain new unbounded or long IRQ-disabled work.
- Exec preserves the current thread's EL1 `kernel_sp`; fork still returns zero
  in the child and uses a distinct child kernel stack.
- Syscall numbers, argument order, userspace ABI, process/scheduler policy,
  synchronization, saved-frame representation, and assembly layout do not
  change.

## Consequences

- Generic kernel lifecycle code expresses exclusive access to the active return
  context without exposing the architecture frame layout.
- AArch64 owns all current syscall register decoding and live-context mutation.
- The architecture hooks behind `ActiveContext` remain a narrow link boundary
  because the architecture crate depends on the generic kernel crate.
- Saved scheduler contexts are still untyped word arrays. The temporary bridge
  is deliberately visible in scheduler copy/clone code and must be removed by a
  later saved-context hardening decision.

## Alternatives considered

- Keep raw frame words and add more C ABI helpers. Rejected because it leaves
  lifetime, nullability, and architecture leakage unchanged across generic
  APIs.
- Move `TrapFrame` into the generic kernel. Rejected because register layout,
  exception return state, and assembly offsets are architecture-owned.
- Redesign live and saved contexts together. Deferred because changing
  `TaskFrameStorage`, assembly layout, and saved-frame lifecycle would broaden
  this milestone and mix independent ownership risks.

## Validation

- Build the kernel and AArch64 architecture crate for
  `aarch64-unknown-none-softfloat`.
- Run `cargo xtask check` and the existing AArch64 QEMU contracts covering
  timer/preemption, blocking IPC, UART stdin restart, user faults, fork, exec,
  wait, and process exit.
- Audit generic syscall/process/console/task-call sources for register indices,
  raw frame dereferences, and raw live-frame pointer APIs.
- Run formatting, clippy/host workflow checks, link audits, and
  `git diff --check` through the repository verification workflow.

## Related decisions

- ADR-0003: AArch64 preemptive IRQ-return context switching.
- ADR-0016: first AArch64 EL0 process bring-up.
- ADR-0017: bounded process table and user fault policy.
- ADR-0020: UART stdin and restartable blocking read.
- ADR-0022: fork, execve, and waitpid process control.
