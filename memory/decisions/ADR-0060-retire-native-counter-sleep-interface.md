# ADR-0060: Retire native counter-sleep interface

## Status

Accepted. Partially supersedes
[ADR-0059](ADR-0059-native-counter-sleep-and-active-read-validation.md) for the
native userspace sleep interface only.

## Context

The native trial avoided timespec usercopy and kernel conversion but added a
second sleep ABI and benchmark mode. Measurements showed a modest submission
benefit, not achievement of the round-trip maximum target. The user chose to
remove this interface and retain POSIX sleep.

## Decision

Remove syscall 19's handler and public constant, `genrt/time.h` with its native
sleep wrapper and conversion helper, and rtbench's `-C` option. Do not renumber
syscalls 0 through 18 or add a replacement interface. Number 19 follows the
existing unknown-syscall policy, which terminates the calling process; it is
not an ENOSYS feature-detection endpoint.

Keep `clock_nanosleep`, internal counter-deadline helpers, EL0 clock reads,
active hardware read validation, and batched sleep/quantum publication.
Historical native measurements remain identified as a retired experiment,
not a current CLI or post-removal performance baseline.

## Invariants

- POSIX pointer validation, errno behavior, and exact wait ownership remain.
- Sleep submission and IRQ handling remain bounded and allocation-free.
- No native sleep syscall or public counter-sleep helper remains active.

## Consequences

Applications use the existing POSIX sleep interface. Trial users of `-C` receive
a CLI error. Trial binaries invoking number 19 follow unknown-call isolation.
This removal makes no new hardware latency claim.

## Alternatives considered

Keeping a deprecated wrapper or forwarding syscall would preserve the extra
interface rather than remove it. Changing unknown-call policy is unrelated.

## Validation

Host and production QEMU CLI tests reject `-C` and `-CC`. A raw number-19 call
with deadline zero must terminate only its test child, detecting a surviving
native handler. Production rtbench and full/focused profiling contracts use
POSIX absolute sleep; pointer and secondary-CPU sleep contracts remain.

## Related decisions

- [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md)
- [ADR-0059](ADR-0059-native-counter-sleep-and-active-read-validation.md)
