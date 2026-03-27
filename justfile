set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
    @just --list

help:
    @just --list

doctor:
    cargo xtask doctor

phase0-check:
    cargo xtask phase0-check

qemu-cmd-aarch64:
    cargo xtask qemu-cmd --arch aarch64

gdb-cmd-aarch64:
    cargo xtask gdb-cmd --arch aarch64

tree:
    cargo xtask repo-tree
