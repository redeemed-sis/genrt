# Userspace instructions

- Programs are freestanding, statically linked AArch64 ELF64 `ET_EXEC` images;
  do not assume libc, a dynamic linker, TLS, or hosted C startup behavior.
- Keep AArch64 startup and syscall-number details under `user/c/aarch64/`.
- Preserve the syscall ABI: `x8` is the number, `x0..x5` are arguments, and
  `x0` is the result with negative errno on failure.
- Register production programs in `user/c/programs.toml`; build, contract, and
  release composition must remain manifest-driven.
- Bound argv, envp, pathname, directory-record, and I/O buffers. Do not hide
  unsupported behavior behind libc-like assumptions.
- Update exact invocation contracts when product behavior changes and run the
  relevant production-program QEMU case.
