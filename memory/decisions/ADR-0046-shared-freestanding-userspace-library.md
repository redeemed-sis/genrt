# ADR-0046: Shared freestanding userspace support library

## Status

Accepted

## Context

Production utilities and QEMU userspace programs duplicate string comparisons,
length scans, output loops, and decimal parsing. More shell functionality and
command-line utilities would multiply these private implementations. genrt
needs common interfaces without importing a hosted C runtime or changing the
kernel syscall ABI.

## Decision

- Statically link a shared freestanding support archive into all userspace
  programs through the common xtask ELF builder. Production programs remain
  registered and tested through the existing product manifest.
- Put string, memory, integer formatting, and short-option parsing in generic
  userspace code. Keep raw syscall transport under AArch64. The archive
  is not linked into the kernel and does not replace kernel memory helpers.
- Use the public `write()` symbol directly, declared in the common `unistd.h`
  and implemented in common code through architecture syscall transport.
  Host tests interpose it at link
  time; no extra platform-write API is exposed. Keep `unistd.h` free of syscall
  assembly by implementing its sleep wrappers out of line.
- Expose familiar `string.h`, `printf`, `dprintf`, `snprintf`, and `getopt`
  interfaces, with an explicitly documented supported subset. Do not expose
  `FILE`, floating-point formatting, locale, allocation, TLS, or an `errno`
  object. Report library failures using genrt's direct negative errno policy.
- `snprintf` reports the required length excluding NUL even when truncated;
  a nonempty destination is NUL-terminated. Descriptor formatting uses bounded
  temporary storage and handles short writes without allocating a full output
  string. A failed write may leave an already-written prefix.
- Keep `getopt` state process-private. It is inherited by eager-copy fork and
  initialized again by exec; it is not a thread-safe shared parser. Support
  short options and required arguments without argument permutation or GNU
  long options.
- Share strict decimal conversion with caller-selected bounds. Preserve the
  existing rejection of signs, empty input, trailing characters, and overflow
  in `sleep` and `taskset` rather than inventing libc `errno` semantics.
- Compile archive members without test defines, markers, or generated contract
  headers. Test emitters remain in `tests/qemu/user` and use shared bounded
  formatting only to construct their records before writing them.
- Keep archive ordering and metadata deterministic. Test and release images
  reuse exact production ELF artifacts after static linking; packaging does
  not rebuild a different library into an already-tested executable.

## Invariants

- Library calls add no kernel ABI or kernel fast-path behavior.
- Runtime storage is bounded; formatting and parsing do not allocate.
- Standard string and memory calls require valid caller-owned objects, just
  like their C counterparts. They are not kernel usercopy validation APIs.
- Formatting never interprets application text as a format unless the caller
  explicitly supplies it as the format argument.
- Test protocol content and provenance stay outside production artifacts.
- Shell command policy, history, and completion are not library responsibilities.

## Consequences

Programs and tests share one implementation of commonly needed operations.
The public headers describe the subset rather than claiming complete libc or
POSIX conformance. Static linking preserves independent process ownership and
the existing ELF loader; no dynamic runtime discovery is required.

The archiver becomes an explicit host build dependency. Application buffer
limits remain application policy; a bounded formatter is not permission to
consume arbitrary userspace stack space.

## Alternatives considered

- Continue copying local helpers: rejected because fixes and semantics drift
  between products and tests.
- Import a full libc: deferred because hosted startup, TLS, allocation, errno,
  and file streams are outside the current runtime model.
- Put generic algorithms in `syscall.h`: rejected because implementation
  ownership and architecture-specific syscall wrappers are separate concerns.
- Use a shared dynamic library: rejected because the loader intentionally
  supports static ELF images only.

## Validation

- A userspace API case tests strings, memory overlap and boundaries,
  formatting/truncation, option-parser state, and checked decimal conversion.
- Host tests inject short, zero-progress, and failed writes at the adapter
  boundary without changing production syscall code.
- Existing production-program, shell, SMP, and terminal-power contracts run
  against statically linked executables.
- Build and release verification retain production marker checks and exact
  ELF/manifest hashes.

## Related decisions

- [ADR-0018](ADR-0018-userspace-elf-loader.md)
- [ADR-0025](ADR-0025-automated-qemu-testing-and-tagged-releases.md)
- [ADR-0045](ADR-0045-posix-monotonic-time-and-sleep-abi.md)
