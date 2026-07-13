# ADR-0022: Minimal fork/execve/waitpid Process Control

## Status

Accepted

## Context

The first initramfs milestone could launch `/init` as one EL0 process and expose
basic file-descriptor syscalls. The shell could read UART stdin and print files
from ramfs, but it could not launch ordinary programs from the initramfs.

The next userspace step needs process-control primitives without committing to
copy-on-write, signals, a dynamic linker, PATH lookup, or a full POSIX wait
subsystem.

## Decision

The kernel implements a minimal POSIX-like process-control path:

- `fork()` creates a child process slot with a generation-encoded user-visible
  pid and eagerly copies the current ELF segments plus the user stack into a new
  TTBR0 address space.
- `execve(path, argv, envp)` loads a static ELF from the mounted initramfs,
  replaces the current process image while preserving PID, parent, waiter, and
  FD table, and builds an initial user stack containing `argc`, `argv`, and
  bounded `envp`.
- `waitpid(pid, status, 0)` waits only for a specific child. If the child is
  still running, the syscall registers a waiter and blocks using the same
  fixed-width AArch64 SVC restart model as blocking `read(0)`.
- `/bin/echo` is built as a freestanding userspace ELF and staged into the
  default cpio initramfs. The shell resolves commands without slashes as
  `/bin/<command>` and runs them with `fork -> execve -> waitpid`.

The fork implementation deliberately does not share open-file descriptions:
the current FD table is copied by value, so offsets are independent after fork.

## Consequences

- Userspace can launch a normal child executable from initramfs.
- Child exit/fault status is consumed once by its parent through `waitpid`.
- The process table remains bounded and generation-checked.
- User address-space cloning is eager and limited to known process-owned ELF
  segments plus the fixed user stack; there is no `mmap` enumeration yet.
- `execve` supports bounded `argv` and `envp` strings on the initial user stack.
- There is no PATH environment, shell quoting, pipes, redirection, signals,
  COW, `waitpid(-1)`, or close-on-exec policy.

## Determinism

All process table and scheduler wake/block transitions remain bounded
single-core critical sections. `fork` and `execve` may allocate and copy user
memory in task/syscall context only; they do not allocate in IRQ or scheduler
fast paths. Scheduler task stacks are still preallocated, but their heap backing
is zeroed in place to avoid large temporary stack objects during bootstrap.
