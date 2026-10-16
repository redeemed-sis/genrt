# ADR-0056: Bounded sleep/wakeup phase profiling

## Status

Accepted

Partially superseded by
[ADR-0057](ADR-0057-in-process-sleep-profile-reports.md) for raw UART export and
host-side analysis only. Collection ownership and the control/read ABI remain.

## Context

Userspace release latency includes sleep submission, timer delivery, scheduler
selection, context restoration, and exception return. End-to-end rtbench
samples alone cannot attribute these costs. Instrumentation must not become a
general tracing service or change wait, scheduler, or POSIX sleep semantics.

## Decision

- Add an opt-in `wake-profile` feature. Without it, instrumentation is absent
  from Rust and assembly; the diagnostic control/read numbers return ENOSYS.
- Profile only the owning thread's absolute CLOCK_MONOTONIC sleeps. Bind one
  session to a generation-bearing thread identity and immutable logical CPU.
  Other threads cannot replace, stop, or read it. Fork does not inherit it.
- Allocate a checked, budget-limited record buffer in task context before
  publishing an active session. Record capacity counts complete sleep cycles,
  not timestamp fields. Never allocate, grow, format, or output during capture.
- Associate each cycle with its exact wait registration and deadline. Preserve
  completed records on exhaustion and count lost cycles explicitly. Identify
  early return, direct idle wake, and generic selection separately.
- Retain ordered raw physical-counter observations. A full mode records all
  supported boundaries; a focused mode records a selected phase pair while
  retaining exact cycle identity and loss accounting.
- Define narrow START, STOP, DISCARD, and bounded byte-offset read operations.
  STOP freezes records. Replace/discard/terminal cleanup detach ownership under
  short exclusion and release heap storage only after protected ownership ends.
  User copies never cross profiler or scheduler locks.
- Version the binary export with fixed-width fields, layout assertions, counter
  frequency, identity, phase presence, wake path, and loss/completion metadata.
  Counter-read and trace-write calibration is diagnostic, not a subtraction
  from application latency or a bound on complete instrumentation overhead.
- Preserve the exception-frame ABI. Entry observations use assembly-owned live
  exception padding; saved scheduler contexts do not contain that padding.
  Return probes precede GPR restoration/ERET, not the hardware return itself.
- rtbench observes the first practical EL0 counter after sleep, stops before
  retrieving results, and exports after measurement. Host analysis validates
  record ordering, deadlines, correlation, and missing observations before
  calculating distributions. UART output never occurs during the measured loop.
- Keep diagnostic Cargo/ELF/FIT output separate from canonical artifacts and
  retain a diagnostic marker that ordinary release verification rejects.

## Invariants

- Profiler state is a leaf owner: no scheduler, process, timer, allocator, user
  copy, or logging callback runs while it is protected.
- Only exact current-generation waits can contribute wake phases. Unrelated
  IRQs and delayed completions cannot overwrite another cycle.
- Collection is bounded and allocation-free, with no intentional sampling.
- Scheduling policy, wait completion, timer deadlines, and context ABI remain
  owned by their existing subsystems.
- Frozen records remain stable until explicit replacement/discard or owner
  termination. Heavy cleanup occurs outside IRQ-disabled sections.

## Consequences

Diagnostic builds consume a bounded amount of heap and perturb timing through
counter reads and trace writes. Instrumented results are not a production
latency baseline. Full and focused runs, feature-enabled runs without active
collection, and feature-disabled runs must be compared on the same hardware.
QEMU establishes functional correctness, not physical-Pi performance.

## Alternatives considered

- Aggregate counters lose exact wait identity and cannot expose phase order.
- A fixed boot allocation does not satisfy configurable session capacity.
- Live UART tracing perturbs subsequent releases and may manufacture overruns.
- General-purpose tracing, PMU sampling, VFS export, and scheduler redesign are
  outside this diagnostic interface.

## Validation

Pure tests cover configuration, identity, capacity, freeze, and binary export.
Opt-in QEMU contracts cover syscall validation, consecutive waits, both wake
paths, owner isolation, lifecycle cleanup, and bounded retrieval. Ordinary
contracts remain the feature-disabled gate. Matched physical Raspberry Pi 3B+
runs are required separately; no hardware phase results are inferred from the
earlier uninstrumented rtbench summaries.

## Related decisions

- [ADR-0032](ADR-0032-typed-wait-registrations-and-external-wake-ownership.md)
- [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md)
- [ADR-0053](ADR-0053-el0-monotonic-counter-fast-path.md)
- [ADR-0055](ADR-0055-batched-timer-wakeup-and-idle-selection.md)
- [Profiling procedure](../../docs/wake-profiling.md)
