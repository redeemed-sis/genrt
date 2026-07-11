# ADR-0024: Process cwd and canonical path resolution

## Status

Accepted.

## Context

The readonly initramfs-backed ramfs has stable directory indexes, but relative
paths are still interpreted from `/`. The userspace shell needs process-wide
cwd semantics so a builtin `cd` affects later fork/exec commands.

## Decision

- Each running user process stores a ramfs directory index as its cwd. `/init`
  starts at `/`, fork copies the index, exec preserves it, and slot reclaim
  resets it.
- This identity is valid because the mounted ramfs is immutable. A future
  writable VFS must replace it with a refcounted directory vnode/dentry handle.
- A central ramfs-aware resolver traverses components from the root or cwd
  directory index, collapses repeated separators and `.`, and applies `..`
  without escaping root. Every ordinary component must exist, and traversal
  cannot continue through a regular file; a later `..` therefore cannot erase
  an earlier `ENOENT` or `ENOTDIR` condition.
- Resolution returns both the canonical path and the existing file/directory
  node index, so syscall code does not repeat whole-path lookups.
- `open`, `execve`, and `chdir` use the resolver. Empty paths fail with
  `ENOENT`; canonical paths longer than `GENRT_PATH_MAX` fail with
  `ENAMETOOLONG`.
- `chdir(path)` updates only the current process. `getcwd(buf, size)` returns
  the byte count including NUL; the C wrapper returns `buf` or `NULL`.
- `getcwd` uses a path-sized user-copy bound rather than the smaller ordinary
  syscall copy bound. User pointers remain confined to the user-copy layer.

## Determinism

Cwd reads and writes use short IRQ-disabled process-table sections without
allocation or path traversal. Path traversal allocates only a result buffer
sized from the actual cwd/input bound in syscall task context. Userspace
C-string copying grows in small fallible chunks; both operations remain bounded
by `GENRT_PATH_MAX`. Ramfs directory identities and canonical cwd strings remain
immutable after boot mount.

## Consequences

The shell implements `cd` as a builtin and launches `/bin/pwd` as a normal
fork/exec child. After `cd /etc`, external `pwd`, `ls`, and `cat banner` observe
the inherited `/etc` cwd. Symlinks, `fchdir`, `chroot`, `HOME`, `CDPATH`, and
writable-directory rename semantics remain out of scope.
