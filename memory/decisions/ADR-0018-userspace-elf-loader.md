# ADR-0018: Userspace ELF Loader Only

## Status

Accepted

## Context
The first EL0 smoke path used tiny raw payloads to validate TTBR0 activation,
lower-EL syscall dispatch, and process exit/fault policy. That was useful for
bring-up, but raw flat binaries do not scale to C examples, multiple segments,
permissions derived from executable metadata, or a future initramfs/VFS loader.

## Decision
Userspace examples are now ELF64 AArch64 executables only.

- `xtask` builds freestanding user programs from `user/c/` into `.elf` files.
- QEMU generic loader still copies the selected `.elf` file into RAM with
  `force-raw=on`, but it is only a transport for bytes and does not set PC.
- The kernel parses the ELF image and maps `PT_LOAD` segments into TTBR0.
- The loader accepts only little-endian ELF64 AArch64 `ET_EXEC` images.
- Dynamic linking, interpreters, TLS, relocations, ET_DYN, and RWX segments are
  out of scope and rejected.
- Legacy raw `.bin` user payloads and build/run options are removed.

## Invariants
- The QEMU-loaded user image region remains reserved from the frame allocator.
- ELF segment backing pages are allocated from the generic physical frame
  allocator and owned by the process until `process_join()` reclaims them.
- Page-table entries receive physical addresses; PA-to-HVA conversion is used
  only while copying ELF bytes into freshly allocated frames.
- User stack setup remains in the process layer, not the ELF loader.
- The kernel has no fallback path that treats a failed ELF parse as a raw text
  blob.

## Consequences
- The default demo prints `hello from C ELF`.
- The fault demo is also an ELF image and exercises the same loader path.
- The loader is still intentionally minimal: no dynamic linker, no VFS, no
  demand paging, no ASIDs, and no general process image metadata beyond the
  fixed bring-up loader reservation.
