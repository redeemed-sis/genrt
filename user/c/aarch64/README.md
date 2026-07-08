# AArch64 userspace startup ABI

This directory contains the AArch64-specific freestanding C runtime pieces:

- `crt0.S`: process entry stub;
- `include/syscall.h`: raw syscall wrappers and AArch64 syscall numbers.

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
