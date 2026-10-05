# ADR-0053: EL0 monotonic counter fast path

## Status

Accepted

Supersedes [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md) only for
public clock observation transport and its non-NULL pointer boundary. Raw
syscall ABI, clock domain, conversion, and blocking sleep semantics remain.

## Context

Periodic applications observe release and execution time through the public
`clock_gettime` API. A syscall for each observation adds exception, usercopy,
and return overhead to both the workload and its measurement. The architected
physical counter already provides one coherent, immutable-frequency domain on
QEMU and Raspberry Pi, including secondary CPUs.

## Decision

- Each architecture CPU initialization enables `CNTKCTL_EL1.EL0PCTEN` only.
  EL0 may read `CNTPCT_EL0` and `CNTFRQ_EL0`, but cannot program timers.
- Implement public `clock_gettime` and `clock_getres` in the shared static
  library. Keep counter instructions in the AArch64 include adapter. Order
  counter observations with ISB and a compiler memory barrier.
- Convert quotient and remainder in userspace with the same downward rounding
  and epoch as the kernel. Validate the architected 32-bit frequency, reject
  zero frequency or signed-seconds overflow, and normalize nanoseconds.
- Resolution is one counter tick rounded upward to integral nanoseconds.
- Preserve signatures, constants, layouts, and negative errno conventions.
  Unsupported clocks return EINVAL; NULL gettime output returns EFAULT;
  NULL getres output succeeds. Other non-NULL output pointers must designate
  writable C objects. Bad pointers can fault the calling process, as with a
  Linux vDSO, rather than receiving kernel usercopy validation.
- Keep `SYS_CLOCK_GETTIME` and `SYS_CLOCK_GETRES` unchanged as raw compatibility
  and contract surfaces, including their EFAULT behavior. Applications do not
  need a new counter or profiling API. Sleep continues through the kernel.

## Invariants

- Kernel observation, EL0 observation, and absolute deadlines share CNTPCT.
- No syscall, allocation, initialization cache, or process-specific state is
  needed for a supported userspace observation.
- Counter access is read-only on every runtime CPU; timer ownership is intact.
- A userspace output fault remains isolated by the existing process policy.

## Consequences

Existing programs, including rtbench, receive the optimization by relinking.
An observation still has counter, ordering, and conversion overhead; it is not
an instruction-level execution-time measurement or a latency certification.
The kernel syscall ABI does not change. The public bad-pointer boundary is
explicitly distinct from raw syscall pointer validation.

## Alternatives considered

- A profiling-only counter API would leave ordinary applications on the slow
  path and expose architecture units.
- Always validate the output through a syscall would defeat the fast path.
- A vDSO/shared clock page is unnecessary for this fixed-frequency physical
  counter; mutable epochs or other clock sources would need a new contract.

## Validation

- Host tests use controlled counter/frequency reads to verify integer limits,
  normalization, rounding, nullable outputs, and unchanged output on error.
- Production-kernel QEMU tests bracket raw clock observations with EL0 reads,
  compare resolution, check cross-CPU sleeps, and isolate an output fault.
- xtask inspects the clock object for physical-counter reads and rejects SVC.
- Raspberry Pi measurement remains a separate hardware check.

## Related decisions

- [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md)
- [ADR-0046](ADR-0046-shared-freestanding-userspace-library.md)
- [Linux vDSO pointer contract](https://github.com/torvalds/linux/blob/master/Documentation/ABI/stable/vdso)
