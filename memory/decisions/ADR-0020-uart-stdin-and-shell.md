# ADR-0020: UART stdin and interactive userspace shell

## Status

Accepted

## Context

The first EL0 userspace path already supports ELF loading, process lifecycle,
`copy_from_user` / `copy_to_user`, readonly ramfs, and POSIX-like
`open/read/write/close/exit`. File reads work for ramfs descriptors, but fd `0`
is only reserved and has no real input source.

For the first interactive workflow, QEMU `virt` provides PL011 UART input through
`-serial mon:stdio`. The kernel needs to expose that input through normal
`read(0)` without spinning in the syscall path.

## Decision

Use an interrupt-driven UART stdin path:

- The AArch64 PL011 driver enables `RXIM` and `RTIM`, drains RX FIFO on UART IRQ,
  and clears RX/timeout interrupt status.
- The GICv2 setup routes the QEMU UART0 SPI to CPU0 and dispatches INTID 33 to
  the PL011 RX handler.
- `kernel::console` owns a fixed 256-byte stdin RX ring. Overflow uses
  drop-newest policy and increments an overflow counter for diagnostics.
- `read(0)` first drains available ring bytes and copies them to userspace with
  `copy_to_user`.
- If the ring is empty, `read(0)` registers the current thread as the stdin
  waiter, rewinds the lower-EL AArch64 `svc #0` return address by 4 bytes, and
  blocks the thread on `BlockReason::StdinRead`.
- The UART IRQ wakes the registered waiter. When the thread runs again, it
  restarts the same `read(0)` syscall and observes the buffered byte.
- Terminal policy stays in userspace. The shell owns echo, backspace, Enter
  handling, and path interpretation.

The first implementation supports one stdin waiter, matching the current
single-process/single-user-thread milestone. The syscall ABI remains ordinary
POSIX-like `read(0, buf, count)`.

## Consequences

- There is no busy-wait loop in `read(0)` and no temporary stdin-specific
  syscall.
- The UART IRQ path performs only bounded work and does not allocate.
- The scheduler gains a typed `BlockReason::StdinRead`, keeping wakeup ownership
  auditable.
- The restart mechanism is AArch64-specific and intentionally lives behind an
  architecture C ABI helper. It is used only before any bytes are copied to the
  user buffer.
- Future TTY work can replace the raw byte ring without changing the userspace
  fd ABI.

## Non-goals

- terminal canonical mode;
- echo/backspace policy in kernel;
- Ctrl-C/signals;
- shell history or command execution;
- poll/select;
- multi-waiter stdin fairness;
- SMP-safe input queues.
