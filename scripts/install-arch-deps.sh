#!/usr/bin/env bash
set -euo pipefail

cat <<'MSG'
Install base dependencies on Arch Linux:

  sudo pacman -Syu --needed \
    base-devel \
    git \
    rustup \
    just \
    cargo-binutils \
    qemu-system-aarch64 \
    qemu-system-x86 \
    qemu-system-riscv \
    gdb \
    aarch64-linux-gnu-gdb

Then initialize the Rust toolchain:

  rustup default stable
  rustup component add rust-src rustfmt clippy
  rustup target add aarch64-unknown-none x86_64-unknown-none riscv64gc-unknown-none-elf

Optional but useful later:

  sudo pacman -S --needed llvm
MSG
