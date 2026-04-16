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

build-aarch64 log="":
    cargo xtask build-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }}

run-aarch64 log="":
    cargo xtask run-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }}

debug-aarch64 log="debug":
    cargo xtask debug-aarch64 --log-level {{ log }}

gdb-aarch64:
    aarch64-linux-gnu-gdb target/aarch64-unknown-none/debug/genrt-aarch64.elf \
      -ex "target remote :1234" \
      -ex "break _start" \
      -ex "break rust_entry" \
      -ex "break kernel_main"

tree:
    cargo xtask repo-tree
