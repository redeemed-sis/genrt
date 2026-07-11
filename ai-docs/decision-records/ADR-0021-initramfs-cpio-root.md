# ADR-0021: CPIO Initramfs as Userspace Root

## Status

Accepted.

## Context

The first userspace milestones loaded a single ELF payload directly with QEMU's
generic loader, while the readonly ramfs files were compiled into the kernel.
That worked for smoke tests, but it kept filesystem contents in generic kernel
code and made `/init` a special loader-region artifact instead of a normal file.

The project needs a small, deterministic root image that can carry `/init` and
ordinary readonly files without adding a disk driver or a full VFS.

## Decision

QEMU now loads an uncompressed `newc` cpio archive into a permanently reserved
physical initramfs window. The kernel uses `cpio_reader` only to iterate archive
metadata at mount time, validates archive paths with genrt policy, and builds a
mounted readonly ramfs index. File data is borrowed directly from the reserved
archive memory.

`xtask` builds the archive with the `cpio` crate rather than invoking a host
`cpio` tool or carrying a handwritten writer. Archive generation is deterministic:
entries are sorted, uid/gid and mtime are zero, modes are fixed, and paths are
relative without host-specific prefixes.

The first userspace process is loaded by looking up `/init` in the mounted
initramfs and passing that exact file slice to the existing ELF loader.

## Consequences

- The default userspace payload is `initramfs.cpio`, not a direct user ELF.
- Static kernel-compiled ramfs file contents are removed.
- The initramfs physical region remains reserved for the lifetime of the kernel.
- `cpio_reader` is not the filesystem layer; normal `fs::open`, directory
  iteration, and FD-table paths continue to own lookup/read semantics.
- The model remains readonly; symlinks, device nodes, writable files, richer
  metadata, and mount tables are future work. Minimal immediate-child directory
  iteration is described in ADR-0023.

## Determinism

Runtime mount happens once during single-core boot after heap initialization and
before scheduler entry. It may allocate bounded metadata for the mounted index,
but it does not allocate in IRQ or scheduler fast paths. Archive generation is
reproducible from repository files and avoids host `cpio` dependencies.
