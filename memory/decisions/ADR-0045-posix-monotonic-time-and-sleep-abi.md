# ADR-0045: POSIX monotonic time and sleep ABI

## Status

Accepted

Superseded in part by
[ADR-0053](ADR-0053-el0-monotonic-counter-fast-path.md) for public clock
observation transport and its non-NULL pointer boundary only.

## Context

genrt already schedules kernel sleeps and timeouts with the AArch64 generic
physical counter and one owner-local deadline queue per logical CPU. Userspace
cannot observe that time domain or request a deadline, so applications would
need a genrt-specific measurement syscall to express periodic work or measure
wakeup latency.

POSIX and Linux provide a suitable interface through `CLOCK_MONOTONIC`,
`clock_gettime`, `clock_getres`, and `clock_nanosleep`. genrt has no realtime
clock, signal delivery, libc `errno`, process CPU accounting, or timer objects.
The initial ABI must not pretend that those facilities exist.

A blocking EL1 scheduler call resumes through its suspended kernel stack and
explicitly consumes completed wait metadata. A blocking EL0 syscall instead
resumes directly after `svc`; no kernel-stack continuation remains to perform
that consumption.

## Decision

- Reserve syscall numbers 14, 15, and 16 for `clock_gettime`, `clock_getres`,
  and `clock_nanosleep`. Support only Linux-valued `CLOCK_MONOTONIC = 1` and
  `TIMER_ABSTIME = 1`.
- Define the userspace `timespec` ABI as two signed 64-bit fields. Successful
  observations and resolutions normalize `tv_nsec` to the half-open interval
  from zero to one billion nanoseconds.
- Read the existing architecture monotonic counter and frequency. Convert
  counter observations downward to the represented nanosecond instant and
  requested deadlines upward to the first counter tick that is not early.
  Reject a zero frequency or arithmetic overflow instead of saturating.
- Report one counter tick as the effective resolution, rounded upward to whole
  nanoseconds. Never report less than one nanosecond because `timespec` cannot
  represent finer resolution.
- Implement relative sleep by adding a checked, upward-rounded duration to one
  current counter observation. Implement absolute sleep by converting the
  supplied monotonic timestamp directly to one counter deadline. Past and zero
  deadlines return immediately.
- Publish sleeps through the existing exact wait and owner-local deadline path.
  A process remains on its immutable CPU, so its current CPU owns both insertion
  and physical timer programming; ordinary remote-ready notification handles a
  completion observed from another CPU if that ever becomes possible.
- Add a generic automatic wait-consumption mode for operations with no
  post-resume kernel continuation. A blocked EL0 sleep clears its completed wait
  metadata while making the thread ready. Completion before commit remains
  retained and consumed by commit. Other waits preserve explicit first-wins
  cause consumption.
- Preserve nullable `rem` parameters but do not validate or write them after a
  successful sleep. Remaining time becomes meaningful with future userspace
  signal interruption; no synthetic `EINTR` behavior is introduced now.
- Expose `<time.h>` and `<unistd.h>`. `nanosleep` delegates to relative
  `clock_nanosleep`; `sleep` and `usleep` remain userspace wrappers. Functions
  return the existing direct negative errno convention rather than setting a
  libc `errno` object.
- Register `/bin/sleep` through `user/c/programs.toml`. It accepts exactly one
  non-negative decimal whole-second argument representable as `unsigned int`.

## Invariants

- Observation, relative deadlines, and absolute deadlines use one coherent
  architecture counter domain on every CPU.
- Counter conversion cannot silently wrap or saturate. Sleep conversion rounds
  upward, and a successful sleep does not complete before its deadline.
- A sleep blocks through scheduler handoff and allocates no memory in scheduler,
  timed-event, IRQ, or context-handoff paths.
- A sleeping thread and its timed event retain the thread's immutable home CPU.
- Automatic wait consumption is limited to waits that expose no completion
  payload and have no kernel-stack continuation after resume.
- `CLOCK_REALTIME`, signal interruption, and remaining-time reporting are not
  inferred from monotonic uptime.

## Consequences

Userspace can measure elapsed time, use drift-free periodic absolute deadlines,
and compare requested with actual wakeup time through conventional interfaces.
The API is sufficient for later latency and jitter tooling without a test-only
time syscall.

The interface is source-shaped like POSIX/Linux but preserves genrt's direct
negative errno convention. `clock_getres(NULL)` succeeds after clock validation.
The `rem` pointer is ignored on successful relative sleep until signals exist.

This decision adds no wall clock, timer object, busy-wait API, migration, remote
timer insertion, or statistics framework.

## Alternatives considered

- Add millisecond and microsecond syscalls: rejected because userspace wrappers
  and one nanosecond ABI avoid duplicate kernel sleep mechanisms.
- Expose raw architecture counter ticks: rejected because applications would
  depend on a genrt-specific and architecture-shaped interface.
- Emulate `CLOCK_REALTIME` from uptime: rejected because its epoch and adjustment
  semantics would be false.
- Restart `clock_nanosleep` after wake: rejected because a relative request would
  construct a new full duration on every restart and never complete correctly.
- Retain completed sleep metadata in a process continuation: rejected because
  the wait belongs to the thread and the current scheduler state machine can
  express payload-free automatic completion directly.

## Validation

- Kernel unit tests cover normalization, upward deadline rounding, invalid
  frequency, invalid nanoseconds, overflow, and automatic wait cleanup.
- The single-CPU production-kernel contract covers clock observation and
  resolution, invalid IDs and pointers, relative and absolute sleeps, zero and
  past deadlines, periodic absolute scheduling, peer progress, `sleep`,
  `usleep`, and `/bin/sleep` parsing and execution.
- The four-CPU production-kernel contract runs an absolute sleep on CPU2,
  verifies no early return, and checks immutable affinity before and after wake.
- The canonical QEMU and product-manifest gates retain existing userspace,
  scheduler, artifact, and release-composition coverage.

## Related decisions

- [ADR-0005](ADR-0005-one-shot-timer-deadline-engine.md)
- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0037](ADR-0037-per-cpu-deadline-queues-and-direct-scheduler-dispatch.md)
- [ADR-0042](ADR-0042-smp-userspace-memory-and-fork-affinity.md)
