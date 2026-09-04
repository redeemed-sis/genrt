# Commit convention

We use Conventional Commits.

Examples:
- `chore(repo): bootstrap workspace`
- `docs(memory): add architecture ADR`
- `feat(arch/aarch64): add early boot entry stub`
- `test(kernel): add scheduler invariants`

Rules:
- one logical change per commit;
- explain why in the body if the change is non-trivial;
- wrap every commit body line at 72 characters or fewer, including
  `Determinism impact` paragraphs;
- only an indivisible URL or identifier may exceed the body line limit;
- mention determinism impact if relevant.
