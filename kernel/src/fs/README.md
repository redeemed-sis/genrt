# Readonly filesystem and descriptors

The current filesystem is a deliberately small readonly ramfs backed by a
QEMU-loaded deterministic `newc` initramfs. It is not yet a VFS.

## Mount and index

`fs::initramfs` parses the reserved archive once during boot. `ramfs` builds
immutable file and directory indexes, rejects duplicate/conflicting paths, and
sorts immediate children deterministically. File bytes remain in the mounted
archive; indexes and path metadata are kernel-owned allocations created before
normal userspace operation.

Directories are explicit archive entries. Root exists by filesystem contract;
normal lookup does not synthesize arbitrary missing intermediate directories.

## Path traversal and cwd

Each process stores a stable ramfs directory index as cwd. Fork inherits it and
exec preserves it. `resolve_existing_path` traverses each input component from
root or cwd, so a later `..` cannot erase a missing component or a regular file
used as a directory. Traversal never escapes root.

Paths are bounded by `GENRT_PATH_MAX`; there is no writable rename/create
normalization model yet.

## File descriptors

Each process owns a fixed 32-entry descriptor table. Descriptors 0, 1, and 2
are reserved for stdin/stdout/stderr; ordinary opens choose the lowest free
descriptor from 3. Handles store immutable ramfs indexes plus independent file
or directory offsets. Current fork copies handles, so parent and child offsets
advance independently.

Regular files support readonly sequential `read`. Directories support
Linux-like `getdents64` records and advance by entry index. Close clears the
bounded slot. Process exit/fault closes all descriptors before resources become
zombies.

## Current syscall semantics

- `open`: existing files/directories, readonly flags only.
- `read`: ramfs files and fd 0 console input.
- `write`: UART stdout/stderr; regular-file writes are unsupported.
- `getdents64`: immediate directory entries with bounded record construction.
- `chdir`/`getcwd`: immutable ramfs directory cwd.
- `close`: releases non-stdio descriptor slots.

## Boundaries

There are no writable nodes, mounts, symlinks, metadata syscalls, permissions,
shared open-file descriptions, storage drivers, or page cache. Future VFS work
must preserve bounded syscall behavior and process ownership rather than growing
this ramfs index into an implicit general VFS.

Related decisions: ADR-0019 and ADR-0021 through ADR-0024.
