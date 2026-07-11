# ADR-0019: Readonly ramfs and POSIX-like FD table

## Status
Accepted

Superseded for filesystem backing by
[`ADR-0021`](ADR-0021-initramfs-cpio-root.md). Extended for directory handles and
`getdents64` by [`ADR-0023`](ADR-0023-directory-fds-and-getdents64.md). The FD
table and syscall ABI from this ADR remain active; the file data source moved
from compiled-in entries to a mounted initramfs index.

## Context
The first EL0 userspace path can load ELF images, dispatch lower-EL syscalls,
copy user buffers, and track process lifecycle. The next userspace milestone
needs a small file descriptor model so user programs can use POSIX-like
`open`, `read`, `write`, and `close` without introducing VFS, storage drivers,
or writable filesystems.

## Decision
Add a bounded, per-process FD table and a readonly ramfs.

- File descriptors `0`, `1`, and `2` are reserved for stdin/stdout/stderr.
- `open()` allocates the lowest free descriptor starting at `3`.
- Regular files are represented as `FileHandle::RamFile { file_index, offset }`.
- `read()` copies from ramfs file data to userspace and advances the handle
  offset after `copy_to_user()` succeeds.
- `write()` is fd-based: `1` and `2` write to UART; regular-file writes are not
  implemented yet.
- `close()` frees non-stdio descriptors.
- The process table owns each process FD table and closes it on exit/fault.

Pathnames are copied from userspace as bounded C strings. `GENRT_PATH_MAX` is
4096 bytes excluding the terminating NUL. The kernel scans at most
`GENRT_PATH_MAX + 1` bytes and validates user mappings per chunk/page instead of
walking page tables for every byte.

## Invariants
- FD table storage is fixed-size and does not grow at runtime.
- FD table mutations are short critical sections; user memory copying is done
  outside those sections.
- The ramfs is readonly. Its original compiled-in backing was replaced by the
  initramfs-backed mount described in ADR-0021.
- Syscalls return non-negative success values and negative errno values.
- Unsupported writable-file operations fail instead of silently mutating ramfs.

## Consequences
- The default userspace demo can open `/hello.txt`, read it, print it through
  stdout, close the descriptor, and exit.
- This establishes the userspace ABI shape for future VFS/initramfs work while
  keeping the current implementation small and deterministic.
- Path normalization, writable files, `lseek`, `dup`, `stat`, and richer
  directory APIs remain out of scope. Minimal directory FD iteration is covered
  by ADR-0023.
