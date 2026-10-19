# ADR-0059: Native counter sleep and active-read validation

## Status

Accepted

Partially superseded by
[ADR-0060](ADR-0060-retire-native-counter-sleep-interface.md) for the native
userspace sleep interface only. Active-read validation and batched time
publication remain in effect.

## Context

Periodic EL0 sleeps repeat user-memory validation, nanosecond conversion and
time-queue ownership. The POSIX interface remains useful and must preserve its
pointer and errno behavior. Existing immutable process/TTBR0 ownership permits
validation against the current hardware translation without a software walk.

## Decision

Add syscall 19, `genrt_sleep_until_counter(uint64_t deadline)`, with an absolute
physical-counter deadline in x0. All 64 bits carry the deadline. Zero and past
deadlines succeed immediately; future deadlines use the existing owner-local
automatic wait and timer. There is no wraparound, relative form, clock change,
remote insertion, or pointer argument. The physical counter domain and frequency
are the same ones already readable at EL0. Return is zero; no new errno policy
is introduced. Old kernels reject the unknown syscall under their existing
unknown-call policy; this is not a feature-detection API.

The public `genrt/time.h` wrapper and checked ceiling conversion are separate
from POSIX `clock_nanosleep`. rtbench opts into the native transport with `-C`;
its default, timeline and phase schema stay unchanged. Diagnostic admission
correlates both absolute sleep transports with the exact wait token.

Single-page user reads use AArch64 AT S1E0R against the active immutable root,
checking PAR_EL1.F before touching user memory. Architecture code excludes IRQs
and preserves PAR. Generic code retains range/overflow and VM user-context
eligibility checks. Cross-page reads and writes keep software permission walks.
An invalid read returns EFAULT; no exception-recovery or unchecked copy is added.

EL0 sleep removes the outgoing RR event and publishes its wait in one time-owner
transaction. That owner releases before scheduler context handoff. An early
completion restores quantum eligibility; other wait owners retain their API.

## Invariants

- Immutable address-space owner, root lifetime and usercopy eligibility remain
  mandatory; an idle-retained root alone cannot authorize usercopy.
- No queue growth, allocation, or remote timer programming occurs in fast paths.
- POSIX input validation, exact wait identities and automatic consumption remain.
- Hardware translation probes cannot authorize another page or inactive root.

## Consequences

Native applications avoid a pointer copy and kernel conversion but explicitly
depend on the counter domain. Hardware and software validation paths need QEMU
coverage for read-only, unmapped, unaligned and crossing-page buffers. Profiled
and ordinary timing remain distinct; no latency bound follows from this design.

## Alternatives considered

Unchecked user pointers violate EFAULT and fault containment. A translation
cache adds invalidation/reuse state. A new signal/fault-fixup engine or general
scheduler repartitioning would exceed this narrowly scoped optimization.

## Validation

Host tests cover queue replacement, conversion, CLI selection and failure paths.
Production QEMU contracts exercise native and POSIX sleeps, CPU-local ownership,
pointer boundaries and profiling. Physical-Pi comparisons are recorded separately
in `docs/wake-profiling.md`, without treating QEMU as a timing measurement.

## Related decisions

- [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md)
- [ADR-0053](ADR-0053-el0-monotonic-counter-fast-path.md)
- [ADR-0058](ADR-0058-bounded-sleep-and-vm-owned-usercopy.md)
