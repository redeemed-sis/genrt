---
name: genrt-review
description: Use for reviewing a genrt patch, branch, PR, or uncommitted diff for current defects and test gaps. Do not use as a broad redesign or backlog-generation exercise.
---

# genrt review

1. Establish the correct base and inspect staged, unstaged, and relevant
   untracked changes.
2. Read active instructions, module docs, invariants, and affected ADRs.
3. Trace changed execution paths, ownership transitions, error cleanup, bounded
   behavior, and architecture/generic boundaries.
4. Check tests and reproduce suspicious behavior when practical.
5. Lead with discrete actionable findings and exact file/line references.

Severity:

- blocker: reproducible current break, explicit acceptance failure, or
  production invariant violation;
- major: reachable defect with substantial correctness, safety, or determinism
  impact;
- follow-up: defensive hardening, hypothetical misuse, maintainability, or
  future scaling.

Do not report style-only comments or elevate hypothetical future configurations.
If no actionable findings remain, say so and identify residual test risk.
