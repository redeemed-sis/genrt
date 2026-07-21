# Pull request

## Summary

<!-- What changed? Keep this focused on the intended scope. -->

## Motivation

<!-- What problem does this solve, and why is the change needed now? -->

## Determinism and ownership impact

<!--
Address allocation, bounds, IRQ/preemption behavior, ownership, ABI, and
artifacts. Write "none" where appropriate.
-->

## Validation

<!-- List exact commands that ran and their outcomes. Explain skipped checks. -->

```text
command
result
```

## Documentation and decisions

<!-- Link updated documentation/ADRs, or explain why none are needed. -->

## Checklist

- [ ] The change is focused and excludes unrelated refactoring.
- [ ] I read the applicable `AGENTS.md`, invariants, module documentation,
      and accepted ADRs.
- [ ] I added or updated tests for observable behavior where applicable.
- [ ] I ran the smallest sufficient validation gate and recorded the result above.
- [ ] I synchronized documentation and relative links where applicable.
- [ ] Production artifacts contain no test protocol, supervisor, fixture, or
      marker content.
