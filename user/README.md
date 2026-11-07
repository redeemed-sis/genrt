# Freestanding userspace

genrt userspace consists of statically linked freestanding AArch64 C programs.
Programs share a small freestanding support library, not a full libc. There is
no dynamic linker, TLS runtime, ELF interpreter, or hosted process startup
environment.

## Product programs

`user/c/programs.toml` is the sole production program registry. Each entry
declares source, initramfs install path, contract role, and dynamic case. `xtask`
uses it for compilation, contract staging, release staging, identity checks, and
coverage reporting.

Current products are `/init`, `/bin/shell`, `/bin/echo`, `/bin/cat`, `/bin/ls`,
`/bin/pwd`, `/bin/taskset`, `/bin/sleep`, `/bin/rtbench`, `/bin/reboot`, and
`/bin/poweroff`.
Other example C files may be buildable without entering production composition.

`taskset <cpu> <program> [args...]` launches one new process on a decimal
logical CPU. A program containing `/` is passed to `execve` unchanged;
otherwise `taskset` uses `/bin/<program>`. Unlike Linux `taskset`, this utility
does not accept masks or modify an existing process: it applies the existing
one-shot fork placement policy, and the child's affinity becomes immutable at
publication.

To add a production binary:

1. add freestanding C source under `user/c/`;
2. register it in `user/c/programs.toml`;
3. add an exact invocation to `tests/qemu/program-contracts.toml`;
4. update the owning supervisor contract and run the targeted QEMU case;
5. run `cargo xtask ci` and release composition checks when applicable.

## Interactive shell

`/init` is a separate initializer. It reads and validates the complete
`/etc/fstab` (at most 16 KiB and 16 entries), mounts automatic entries in order,
then execs `/bin/shell` without changing process identity. Missing or empty
configuration starts the shell with initramfs only. A failed mount prints a
warning to stderr and does not skip later entries or shell startup, even if all
mounts fail. Malformed or unreadable configuration still stops initialization;
no fallback disk is selected. See [storage](../docs/storage.md#boot-mount-configuration).

External commands support stdout redirection:

```text
echo first > /mnt/disk/output.txt
echo second >> /mnt/disk/output.txt
cat /mnt/disk/output.txt
```

`>` creates or truncates; `>>` creates or appends. The child opens the target
and uses dup2 before exec, so the parent retains UART input/output. Redirection
of builtins, malformed syntax, and opening failures produce explicit errors.
Only one stdout destination is supported; there are no pipelines, quoting,
input redirection, or stderr redirection. The target must be on a writable
mount; the initramfs root remains readonly. See [storage](../docs/storage.md).

The shell keeps the last 16 submitted non-empty commands in a fixed ring.
Whitespace-only and consecutively identical lines are not added; overflow
discards the oldest entry. Up/Down (CSI or SS3 arrows) recall commands into an
independent editing buffer. Down past the newest entry restores the draft
saved by the first Up, including its cursor and line-overflow state. Editing a
recalled command never modifies the stored entry. History is process-private
and is not persisted; no history files are created.

Left/Right (CSI or SS3 arrows) move a byte-oriented cursor within the line.
Printable bytes insert at the cursor; Backspace deletes immediately before it
without changing the suffix. Alt+Backspace (`ESC DEL` or `ESC BS`) skips
preceding spaces, tabs, `/` and `>`, then deletes the preceding word or path
component. Enter submits the whole line regardless of cursor position. History
recall places the cursor at the end; Down restores the saved draft position.

For a bare first command token without `/`, Tab candidates remain `cd`, `exit`,
and regular ELF `ET_EXEC` entries under `/bin`. The directory ABI has no mode
field; completion checks ELF identification/type, while `execve` still validates
the complete image. Built-ins take precedence over same-named files. Candidate
names must be printable ASCII without unsupported shell separators or quoting;
command names fit the existing 128-byte execution path buffer.

Other tokens use filesystem completion, including command paths, arguments and
stdout redirection destinations. Absolute and relative paths, `./`, `../`, and
mounted filesystems use ordinary `open()` and `getdents64()` with no `stat()`.
Completion preserves the directory prefix and text after the cursor. Inside
a component, candidates must match both the typed prefix and existing suffix;
only missing bytes between them are inserted. A unique directory gets `/`;
a unique file gets a space only at line end when there is room. Multiple
matches extend the longest common prefix; otherwise candidates are displayed
and the input/cursor is restored. No match leaves the line unchanged. There
is no PATH lookup, quoting, scripting, or UTF-8 grapheme-aware editing.

Input retains the 256-byte line limit and 16-argument limit. An overlong
submitted line is rejected rather than executing its truncated prefix. ANSI
redraw reserves wrapped rows before drawing and refreshes the saved origin
after any scrolling; prefix replay restores the cursor. An initial nonblocking
ANSI cursor-position report discovers columns without a TTY ioctl; headless or
nonresponding terminals use 80 columns. Reports arriving after editing starts
are ignored. Listings and Enter first move to the full line's end.
Unknown CSI/SS3 sequences are discarded through their final byte; unsupported
OSC/DCS strings are discarded through BEL or ST. Newline, backspace, and Tab
resynchronize partial sequences without copying control payloads into commands.
CRLF submits once.

Editor/history state and directory scan storage are fixed-size; no shell heap
allocation is introduced. Completion streams the selected directory, so work
is linear in its entries without a candidate-count truncation.
At most one directory fd and one candidate-file fd are open during a scan.
The shell-private [input helpers](c/shell_input.h) are shared only with host
tests, not exposed as support-library API. `shell-contract` exercises the exact
production shell using controlled test commands, not prompt assertions.

## Periodic timing benchmark

Optional `-P` (full) or `-F start:end` (focused) collects diagnostic sleep phases
only on a `wake-profile` kernel. `-n` sets both the step count and profile record
capacity; no separate capacity option is needed. The normal invocation remains
unchanged. Collection adds fixed process-private correlation storage. After
STOP, rtbench prints per-path phase interval counts, averages, and maxima in
nanoseconds directly in the console. Numbered sections separate submission,
waiting, IRQ handling, resume selection, and EL0 return; overlapping totals and
deadline references are listed separately. No raw export or host analyzer is
needed.
See the [profiling guide](../docs/wake-profiling.md) for isolated builds, phase
IDs, report interpretation, and overhead runs.

`rtbench -p period_ns -n steps -o operations` measures periodic userspace work
through the existing `CLOCK_MONOTONIC` API. All three options are required,
exactly once. Period is a nonzero decimal u64; steps are decimal 1..10000;
operations are decimal 0..4294967295. Signs, whitespace, suffixes, overflow,
unknown options, and extra operands are rejected before observing the clock.
The shared `getopt` parser accepts attached or separate short-option arguments.

The initial observation defines `R[0]`. Each sample observes start `S`, executes
the payload, and observes end `E`. Releases advance only by
`R[i+1] = R[i] + period`, including after overruns; no periods are skipped and
the origin is never reset. Release latency is `S - R[i]`; elapsed task execution
time (TET) is `E - S`, including preemption and clock-call overhead, not CPU
accounting time. An overrun occurs strictly when `E > R[i+1]`, with duration
`E - R[i+1]`. Equality is not an overrun. Every non-overrun iteration sleeps
using `TIMER_ABSTIME` until `R[i+1]`, including the final iteration.

There are exactly `steps` samples, at releases `R[0]` through `R[steps-1]`.
The final sleep targets `R[steps]`; it is not an additional sample. Both
`period * steps` and the initial absolute horizon `R[0] + period * steps` are
checked before payload execution. Timespec normalization, conversion,
monotonic ordering, and release arithmetic are checked as well.

The baseline `.bss` array holds 10000 samples of three u64 fields (240000 bytes).
There is no heap allocation, large stack array, or normal output during the
measurement loop or its final sleep. Each step executes exactly `operations`
unsigned integer additions, with an empty register compiler barrier after
each add to prevent elimination. Loop control is not payload work. Zero is
accepted; there are no payload multipliers, floating-point, or SIMD operations.
The accumulator starts at zero, carries across steps, and is consumed in the
report as `result = steps * operations`.

After measurement, checked sums are accumulated. By default only the final
`SUMMARY` and any enabled phase profile are printed. Add `-v` for verbose
output: each zero-based `STEP` row then reports `release_latency_ns`, `tet_ns`,
and `overrun_ns` before the summary. Verbosity does not change measurement,
statistics, or profiling. `SUMMARY` reports configuration,
`release_latency_avg_ns`,
`release_latency_max_ns`, `tet_avg_ns`, `tet_max_ns`, `overrun_avg_ns`,
`overrun_max_ns`, `overruns`, and `result`. Averages are integer-truncated
over all requested samples, including zero overruns. Output is diagnostic,
not a machine test protocol. Exit status is 0 for success, 2 for CLI errors,
and 1 for runtime, timestamp, arithmetic, statistics, or output failure
(including failure to print a CLI diagnostic). Failed runs need not emit a
complete report; an output failure can leave a printed prefix.

For a production shell session:

```text
rtbench -p 1000000 -n 100 -o 50000
taskset 1 rtbench -p 1000000 -n 100 -o 50000
```

The second command requires logical CPU1 online and uses `taskset`'s immutable
child placement. Period is 1 ms; workload counts are not a duration guarantee.
QEMU contracts establish functionality, not hardware latency or a worst-case
execution bound. See the [validation procedure](../docs/testing.md#rtbench-validation)
for host, virtual-board, and user-performed Raspberry Pi checks.

## Build and startup

The common linker script creates ELF64 little-endian AArch64 `ET_EXEC` images
with static `PT_LOAD` segments. `user/c/aarch64/crt0.S` reads the kernel-built
initial stack, calls `main(argc, argv)`, and exits through the syscall ABI.

The initial stack and syscall conventions are documented in
[`user/c/aarch64/README.md`](c/aarch64/README.md). Raw syscall transport and
numbers stay in `user/c/aarch64/include/syscall.h`. Public headers, including
`sched.h`, `time.h`, `unistd.h`, and `sys/reboot.h`, live in `user/c/include/`
and use the architecture adapter through the configured include paths.

Common [string/memory, formatting, and parsing code](c/common/README.md) is
statically linked into every production, example, and QEMU userspace ELF.
Products and tests use the same implementations; protocol emitters remain
test-only. See [ADR-0046](../memory/decisions/ADR-0046-shared-freestanding-userspace-library.md)
for the library and architecture boundary. Existing build commands require no
per-program library configuration.

## Initramfs

`user/initramfs/` contains product data files. `xtask` stages registered ELF
programs and this data into a deterministic readonly `newc` archive. `/init` is
the system initializer by default; the shell resides at `/bin/shell`.
`user/initramfs/etc/fstab` requests read-write vfat mounts of `/dev/disk0p1`
at `/mnt/disk` and `/dev/disk0p2` at `/mnt/disk/DATA`. Diskless boot continues
with warnings; use an empty or comments-only fstab to omit these attempts.
The kernel parses the archive and loads ELF segments; QEMU
does not set the userspace PC.

## ABI boundaries

- Syscall number in `x8`, arguments in `x0..x5`, return in `x0`.
- Errors are negative errno values.
- The architecture `syscall.h` exposes `dup` and `dup2`. All 32 slots, including
  0, 1, and 2, can be closed, reused, and replaced. Open/dup use the lowest free
  number; duplication shares the underlying VFS position or I/O endpoint.
- Pathnames, argv/envp, I/O, and directory buffers are bounded.
- `fork` eagerly copies process memory; `execve` replaces it from ramfs.
- Forked descriptors share open file descriptions and positions; independent
  opens have independent positions. Exec preserves descriptors and mount-aware
  cwd. The production VFS root remains readonly.
- An optional QEMU FAT32 mount supports ordinary writable file I/O and
  `fsync(fd)` through `syscall.h`. File requests are not capped at 1024 bytes;
  short results still require the usual retry loop. See [storage](../docs/storage.md).
- `<sys/mount.h>` exposes the Linux-shaped genrt subset: partition selectors,
  absolute existing targets, `vfat`/`fat32`, flags zero or `MS_RDONLY`, NULL data.
  It returns direct negative errno, not libc `errno`. Mount changes are global;
  no credentials/privilege enforcement or unmount is implemented.
- `<sched.h>` exposes fixed CPU sets and immutable process-affinity queries.
- `<sys/reboot.h>` exposes Linux-compatible restart and power-off operations;
  the wrapper hides raw reboot magic values.
- `<time.h>` and `<unistd.h>` expose monotonic clock observation, relative and
  absolute sleeps, and conventional `sleep`/`usleep` wrappers.
  Public `clock_gettime`/`clock_getres` read the physical counter at EL0 without
  SVC. Non-NULL outputs must be writable C objects; raw clock syscalls retain
  kernel pointer validation and EFAULT. Sleep still enters the kernel.
- Shell line editing and command policy are userspace responsibilities.

Do not add libc assumptions or raw flat-binary loading paths. Syscall changes
require an ADR, synchronized kernel/header updates, and exact contract coverage.
