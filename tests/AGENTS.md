# Test instructions

- Machine assertions use versioned `GTRT/1` records only. Never match prompts,
  greetings, boot prose, or arbitrary production filesystem listings.
- Keep test kernels, supervisors, helpers, markers, fixtures, and provenance
  isolated from production artifacts.
- Use controlled initramfs fixtures for stable filesystem contracts.
- Every case has bounded step, case, and suite behavior; QEMU must be killed and
  reaped on failure or timeout while complete serial logs remain available.
- Negative cases must be reproducible and distinguish product failure from
  runner/infrastructure failure.
- Update `tests/qemu/README.md`, case TOML, and program invocation contracts
  together. Run the changed case before the full suite.
