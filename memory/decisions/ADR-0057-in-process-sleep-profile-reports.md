# ADR-0057: In-process sleep-profile reports

## Status

Accepted

Partially supersedes [ADR-0056](ADR-0056-bounded-sleep-wakeup-phase-profiling.md)
for raw UART export and host-side analysis only.

## Context

Interactive hardware profiling must produce understandable results without
requiring serial capture, a file, or a separate host analysis command. Raw
hexadecimal records are not a useful primary console interface.

## Decision

- rtbench reads frozen records after STOP and computes its report in userspace.
- Print interval counts, integer averages, and maxima in nanoseconds, separated
  by idle, generic, and early-return paths. Focused mode selects one phase pair.
- Validate schema, identity, deadlines, presence, correlations, and accounting.
  Preserve signed cross-branch intervals and omit missing observations.
- Use one record staging object and fixed aggregates, without heap allocation.
- Remove raw UART export and the host analyzer; do not retain an export flag.
- Keep the kernel control/read ABI and collection ownership unchanged.

## Invariants

All record reads, aggregation, and report output follow STOP. No reporting code
enters kernel IRQ or scheduler paths. Incomplete observations never become
invented zero-duration measurements. Arithmetic and I/O failures fail the run.

## Consequences

Results are readable in the existing UART console. The report is bounded and
does not retain a second trace snapshot. Raw offline reanalysis, JSON output,
and percentile distributions are not provided.

## Alternatives considered

- Optional raw export still requires a separate transport and analysis path.
- Kernel formatting would move presentation into timing-sensitive owners.

## Validation

Host tests check exact tables, conversions, signed intervals, missing probes,
identity and accounting defects, short reads, and arithmetic/I/O failures.
The opt-in QEMU contract runs production rtbench in full and focused modes.
Hardware timing and instrumentation overhead require separate board runs.

## Related decisions

- [ADR-0056](ADR-0056-bounded-sleep-wakeup-phase-profiling.md)
- [Profiling procedure](../../docs/wake-profiling.md)
