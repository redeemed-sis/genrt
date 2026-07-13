# Kernel instructions

- Keep this subtree architecture-neutral. Use C ABI hooks at the `arch/`
  boundary for trap frames, address-space activation, and instruction-specific
  syscall restart behavior.
- Do not allocate in IRQ handlers, scheduler handoff, timed-event dispatch, or
  other fast paths. Preallocate bounded storage during bootstrap.
- Keep IRQ-disabled sections short; do not perform user copies, path traversal,
  parsing, logging bursts, or resource destruction inside them.
- Make task, thread, process, frame, address-space, and FD ownership transitions
  explicit. Generation-checked handles must reject stale references.
- Blocking operations must register state and hand off through the scheduler
  without lost-wakeup windows or polling.
- Use `kernel/src/memory/README.md`, `kernel/src/sched/README.md`, and
  `kernel/src/fs/README.md` for subsystem contracts.
- New or changed public and crate-visible APIs require complete rustdoc,
  including arguments, return values, errors, and allocation/blocking/IRQ
  behavior.
- Run `cargo xtask check` plus the targeted QEMU case for kernel changes; use
  `cargo xtask ci` for cross-subsystem behavior.
