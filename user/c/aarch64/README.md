# AArch64 userspace startup ABI

This directory contains the AArch64-specific freestanding C runtime pieces:

- `crt0.S`: process entry stub;
- `include/syscall.h`: raw syscall wrappers and AArch64 syscall numbers.

Architecture-neutral [support APIs](../common/README.md) live in
`../include/` and `../common/`. xtask supplies both include directories and
statically links the common archive; architecture wrappers keep ownership of
the syscall ABI.

The public `sched.h`, `time.h`, `unistd.h`, and `sys/reboot.h` headers live in
`../include/`. They define architecture-neutral interfaces. `unistd.h` contains
declarations only; other inline syscall wrappers delegate transport to
`syscall.h`, selected through the architecture include path.

## `execve` initial stack

When the kernel commits `execve(path, argv, envp)`, it creates a fresh EL0 stack
and enters the new image at the ELF entry point with `SP_EL0` pointing at the
argument table below. The stack pointer is 16-byte aligned.

```text
lower VA

SP_EL0 ->  argc: u64
           argv[0]: u64  -> "program\0"
           argv[1]: u64  -> "arg\0"
           ...
           argv[argc - 1]: u64
           0: u64
           envp[0]: u64  -> "KEY=value\0"
           ...
           0: u64

           padding/alignment
           argument and environment strings

higher VA
```

`argc` is the number of `argv` entries before the first NULL pointer. The kernel
copies both `argv` and `envp` strings onto the new stack. The current `crt0.S`
passes only `argc` and `argv` to `main(argc, argv)`, but `envp` is already present
after the `argv` NULL terminator for future runtime support.

The copy is bounded by the fixed user stack size. There is no separate arbitrary
argc limit: the pointer table plus all NUL-terminated strings must fit into the
initial stack, otherwise `execve` fails with `-E2BIG`.

## Entry convention

`crt0.S` expects:

```text
x0..x7   unspecified
SP_EL0   initial stack described above
ELR_EL1  ELF entry point
SPSR_EL1 EL0t
```

The stub loads `argc` from `[sp]`, computes `argv = sp + 8`, calls `main`, then
terminates the process with `SYS_EXIT`.

## Path and cwd syscall ABI

The AArch64 syscall wrappers use `x8` for the syscall number and `x0..x2` for
arguments. `chdir(path)` changes the current process cwd. `getcwd(buf, size)`
invokes a kernel ABI that returns the byte count including the terminating NUL;
the C wrapper translates success to `buf` and any negative errno to `NULL`.

The initial process cwd is `/`. Forked children inherit the parent's stable cwd
directory identity, and successful `execve` preserves it. Relative `open`,
`chdir`, and `execve` paths are canonicalized against cwd. The pathname ABI is
bounded by `GENRT_PATH_MAX = 4096` bytes excluding NUL.

## Fork placement extension

`sched_setforkaffinity(int cpu)` is a genrt extension using syscall number 11:

```text
x0 = logical CPU index, or -1 to reset
x8 = SYS_SCHED_SETFORKAFFINITY
return = 0 on success, negative errno on error
```

A nonnegative CPU must be registered and scheduler-online. The setting replaces
any earlier value and applies only to the next successfully published child of
the calling process. `-1` restores deterministic default placement; values
below `-1` are invalid. A failed pre-publication `fork()` preserves the setting,
the child does not inherit it, and neither this syscall nor `execve()` changes
the calling process's immutable CPU owner.

The production `/bin/taskset` utility exposes this policy as
`taskset <cpu> <program> [args...]`. It sets placement for its next child,
forks, executes either the supplied path or `/bin/<program>`, waits for that
exact child, and propagates a normal child exit status. This is a launch-only,
single-numeric-CPU interface; it does not implement Linux masks, migration, or
runtime affinity changes.

## Process affinity query

`sched_getaffinity(pid, cpusetsize, mask)` uses syscall number 12 and reports
the process's actual immutable CPU owner:

```text
x0 = 0 for the current process, or a positive generation-bearing PID
x1 = userspace CPU-set storage size in bytes
x2 = writable cpu_set_t pointer
x8 = SYS_SCHED_GETAFFINITY
return = 0 on success, negative errno on error
```

`<sched.h>` defines a stable 1024-bit `cpu_set_t`, `CPU_SETSIZE`, `CPU_ZERO`,
`CPU_SET`, `CPU_CLR`, and `CPU_ISSET`. This ABI capacity is independent of the
kernel's configured CPU capacity. `cpusetsize` must be at least
`sizeof(cpu_set_t)`; larger buffers are accepted, but the kernel writes exactly
one `cpu_set_t`. The result contains exactly one bit because process migration
and multi-CPU address spaces are unsupported.

PID `0` selects the caller. A positive PID may identify another live or zombie
process that has not been reaped. Invalid, stale, absent, or negative PIDs fail
with `-ESRCH`; a short mask fails with `-EINVAL`, and an invalid writable range
fails with `-EFAULT`. The API exposes no mutable `sched_setaffinity()` operation.

## System power control

`reboot(int operation)` uses syscall number 13 and accepts `RB_AUTOBOOT` or
`RB_POWER_OFF`. Its public `<sys/reboot.h>` wrapper owns the Linux raw magic
values and passes the four-argument `reboot(magic, magic2, cmd, arg)` ABI
through `syscall.h`.

Successful operations never return. Invalid magic or unsupported commands fail
with `-EINVAL`; a returned architecture rejection is reported as `-ENOTSUP`,
`-EPERM`, or `-EIO`. The ABI exposes no PSCI details. `/bin/reboot` and
`/bin/poweroff` use only this wrapper. The current kernel has no credentials,
so every process may request terminal power control.

## Monotonic time and sleep

`clock_gettime`, `clock_getres`, and `clock_nanosleep` use syscall numbers 14,
15, and 16. `<time.h>` exposes only `CLOCK_MONOTONIC`; its epoch is unspecified,
and all CPUs observe the same architecture counter domain. `struct timespec`
uses signed 64-bit `time_t`/`long` fields and successful observations are
normalized to `0 <= tv_nsec < 1000000000`.

`clock_nanosleep` accepts relative mode or `TIMER_ABSTIME`. Both forms use the
existing blocking owner-local deadline path and round counter conversion upward.
Absolute timestamps must come from the same monotonic clock. Past and zero
deadlines return immediately. `nanosleep` delegates to relative
`clock_nanosleep`; `sleep` and `usleep` are userspace-only wrappers and add no
kernel syscall.

The freestanding runtime returns negative errno values directly instead of
setting libc `errno`. Unsupported clocks, flags, or malformed timespec values
return `-EINVAL`; invalid user pointers return `-EFAULT`; unrepresentable time
conversion returns `-EOVERFLOW`. `clock_getres` accepts a null result pointer.
The nullable `rem` argument is preserved but not written because genrt has no
userspace signal interruption yet. `/bin/sleep` accepts one bounded decimal
whole-second operand and uses the public `sleep()` wrapper.
