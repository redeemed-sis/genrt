#!/usr/bin/env bash
set -euo pipefail

sudo apt-get update
sudo apt-get install --yes --no-install-recommends \
  binutils \
  clang \
  device-tree-compiler \
  git \
  jq \
  lld \
  llvm \
  qemu-system-arm

if ! command -v rustup >/dev/null 2>&1; then
  sudo apt-get install --yes --no-install-recommends rustup
fi

rustup show active-toolchain
rustup component add rust-src rustfmt clippy
rustup target add aarch64-unknown-none-softfloat
