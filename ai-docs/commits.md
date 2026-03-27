# Commit convention

We use Conventional Commits.

Examples:
- `chore(repo): bootstrap workspace`
- `docs(ai): add architecture ADR`
- `feat(arch/aarch64): add early boot entry stub`
- `test(kernel): add scheduler invariants`

Rules:
- one logical change per commit;
- explain why in the body if the change is non-trivial;
- mention determinism impact if relevant.
