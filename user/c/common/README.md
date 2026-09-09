# Freestanding support library

This directory owns the generic implementation shared by production programs,
examples, and QEMU userspace tests. Public headers live in `../include/`.
`write.c` implements the public `write()` wrapper through the architecture's
`syscall.h`, selected by the build include path. The code here contains no
AArch64 registers or inline assembly. xtask compiles a static archive and links
it into every userspace ELF. No library initialization or dynamic loader is
needed.

The shared include directory also owns the public `sched.h`, `time.h`,
`unistd.h`, and `sys/reboot.h` interfaces. `unistd.h` contains declarations
only and uses scalar types from `sys/types.h`, so formatted output does not
pull syscall assembly into its translation unit. `common/unistd.c` implements
the `sleep`/`usleep` conversions through the existing time API. Other syscall
wrappers use `syscall.h` from the architecture include directory, rather than
containing register or instruction details themselves.

`<unistd.h>` defines `STDIN_FILENO`, `STDOUT_FILENO`, and `STDERR_FILENO`
for standard descriptors 0, 1, and 2. Use these names in descriptor-based I/O.

## String and memory

`<string.h>` provides `strlen`, `strcmp`, `strncmp`, `strchr`, `memcpy`,
`memmove`, `memcmp`, and `memset`. `strnlen` provides bounded length scans.

- String pointers designate readable NUL-terminated strings, except that
  `strncmp` inspects at most its count. `strlen` excludes the terminator.
- Comparisons return a negative value, zero, or a positive value according to
  unsigned-byte ordering; callers must not assume the nonzero values are 1.
- `strchr` returns the first matching byte, including the terminating NUL when
  requested, or `NULL`.
- Memory operations use the supplied byte count, including embedded zeroes.
  Sources must be readable and destinations writable for that count.
- `memcpy` requires nonoverlapping ranges. `memmove` permits overlap.
  Both and `memset` return the original destination. `memcmp` compares unsigned
  bytes and does not stop at NUL.

These are ordinary C pointer contracts, not kernel usercopy operations.

## Formatting

`<stdio.h>` exposes `printf`, `dprintf`, `snprintf`, and their `va_list`
variants without `FILE` or streams. The supported conversions are `%s`, `%c`,
`%d`, `%i`, `%u`, `%x`, `%X`, `%p`, and `%%`, with decimal field width, zero
padding, and integer `l`, `ll`, and `z` length modifiers. Unsupported conversion
forms fail explicitly; floating point, locale, positional arguments, and `%n`
are not provided. Use only conversion forms documented by this header.

`snprintf(buffer, size, format, ...)` returns the required byte count excluding
NUL. If `size` is nonzero, it writes at most `size - 1` bytes and terminates
them. `buffer == NULL` is valid with `size == 0` for length-only queries.
Input and output must not overlap. Caller strings must be valid, and variadic
argument types must match their conversions. Pass untrusted text as a `%s`
argument, never as a format.

`printf` writes to descriptor 1. `dprintf` takes the descriptor explicitly.
Descriptor formatting uses fixed-size stack staging, handles short writes, and
returns the formatted byte count or a negative error. It does not promise an
atomic write for an arbitrarily long message; an error may follow a written
prefix. A zero-progress backend write fails instead of looping forever.

Errors use direct negative `GENRT_E*` constants from `<genrt/errno.h>`;
there is no global `errno`. No formatter allocates memory. Runtime work is
proportional to the supplied format and data and, for descriptor output, the
number of emitted bytes. Fixed temporary storage is not a bound on the size
of caller-provided strings.

QEMU protocol code stays outside this library. It formats a complete bounded
record, checks truncation, and writes that record separately.

## Command-line parsing

`<getopt.h>` exposes short-option parsing and the process-private globals
`optarg`, `optind`, `opterr`, and `optopt`. `optind` starts at 1. Parsing accepts
clusters such as `-ab` and required arguments specified by `:` after an option
letter. Arguments can be attached (`-n42`) or separate (`-n 42`). Parsing stops
at the first non-option, a lone `-`, or `--`; it never reorders argv.

The result is the next option character, `-1` at end, or `?` for an unknown
option or missing required argument. A leading `:` in the option specification
makes a missing argument return `:` and suppresses diagnostics. `optopt`
identifies an erroneous option; `opterr = 0` suppresses diagnostics too.
Set `optind = 1` after a completed parse to start another. Setting `optind = 0`
also resets a partially consumed option cluster. The parser is not
reentrant; fork copies its state and exec starts fresh process state.

`<genrt/parse.h>` exposes `genrt_parse_u32_decimal(text, max_value, value)`.
It accepts only a nonempty sequence of ASCII decimal digits, checks arithmetic
and the caller's inclusive maximum, stores the result on success, and returns
zero. Invalid input returns `-GENRT_EINVAL`; overflow or an exceeded maximum
returns `-GENRT_ERANGE`. `text` must be a readable C string and `value` a
writable result pointer. Leading zeroes are allowed; signs, whitespace, and
suffixes are not. This explicit checked API does not pretend to implement
`strtoul` or libc errno semantics.

## Boundaries and tests

The library has no heap, TLS, floating-point, locale, file streams, or hosted
startup dependency. It is never linked into the kernel. Test defines, marker
headers, and protocol strings are not archive inputs.

The `support-library` case in `userspace-contract` checks the public interfaces
through a statically linked child process. Host tests substitute only the write
call with the host linker's `--wrap=write` to exercise short writes, errors,
and output spanning multiple staging buffers. Test diagnostics use
`__real_write` so a simulated output failure cannot swallow its own report.
The host suite covers the portable string, formatting, and parser modules;
QEMU contracts cover syscall-backed `write`, `sleep`, and `usleep`.
Existing shell, SMP, and product contracts verify migrated callers.

See [ADR-0046](../../../memory/decisions/ADR-0046-shared-freestanding-userspace-library.md)
for the durable ownership decision.
