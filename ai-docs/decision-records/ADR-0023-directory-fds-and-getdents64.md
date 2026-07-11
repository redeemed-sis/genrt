# ADR-0023: Directory FDs and getdents64

## Status

Accepted.

## Context

The initramfs-backed ramfs can serve readonly files through per-process file
descriptors, and the userspace shell can launch executables from `/bin`.
Userspace still lacks the minimal directory iteration needed for ordinary tools
such as `ls`.

The project does not yet need a full VFS, `stat`, `opendir`, path normalization,
permissions, symlinks, or writable filesystem support.

## Decision

Add directory file descriptors to the existing bounded FD table and expose a
Linux-like `getdents64` syscall:

- `open(path, O_RDONLY | O_DIRECTORY)` opens ramfs directories.
- `open(path, O_RDONLY)` may also open a directory for this minimal stage.
- `read(directory_fd)` returns `-EISDIR`.
- `getdents64(regular_file_fd, ...)` returns `-ENOTDIR`.
- Directory offsets are entry indexes and advance only after successfully
  copying encoded records to userspace.
- The userspace ABI uses a packed `linux_dirent64`-style record:
  `d_ino`, `d_off`, `d_reclen`, `d_type`, and NUL-terminated `d_name`, aligned
  to 8 bytes.

The ramfs mount index now synthesizes parent directories from file paths, always
contains `/`, rejects file/directory path conflicts, and stores immediate-child
entries sorted lexicographically per directory. This keeps `ls /` deterministic
without sorting in userspace.

The default initramfs stages `/bin/echo`, `/bin/cat`, and `/bin/ls`. `cat` uses
only `open/read/write/close`; `ls` uses `open/getdents64/close`. Process cwd and
relative directory lookup are added by ADR-0024.

## Invariants

- Directory metadata is built once during boot mount before scheduler entry.
- Directory FD operations do not allocate in syscall, scheduler, or IRQ paths.
- The `getdents64` syscall uses a fixed stack buffer capped by
  `MAX_USER_COPY`; larger user buffers receive at most that bounded amount per
  call.
- The ramfs remains readonly. Directory entries reference immutable mount-time
  metadata and borrowed file data from the reserved initramfs image.

## Consequences

- The shell can run `ls /`, `ls /bin`, `cat /hello.txt`, and
  `cat /etc/banner` through normal fork/exec/waitpid command execution.
- Directory iteration is sufficient for future `readdir` wrappers and simple
  tools, but not for rich metadata.
- `stat`, `fstat`, `lseek`, `dup`, writable files, symlinks, and mount tables
  remain future work. Current working directory semantics are described in
  ADR-0024.
