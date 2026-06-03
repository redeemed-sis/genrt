set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
    @just --list

help:
    @just --list

doctor:
    cargo xtask doctor

phase0-check:
    cargo xtask phase0-check

qemu-cmd-aarch64 user_bin="":
    cargo xtask qemu-cmd --arch aarch64 {{ if user_bin != "" { "--user-bin " + user_bin } else { "" } }}

qemu-cmd-aarch64-fault:
    cargo xtask qemu-cmd --arch aarch64 --check-fault

gdb-cmd-aarch64:
    cargo xtask gdb-cmd --arch aarch64

build-aarch64 log="":
    cargo xtask build-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }}

run-aarch64 log="" user_bin="":
    cargo xtask run-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }} {{ if user_bin != "" { "--user-bin " + user_bin } else { "" } }}

run-aarch64-fault:
    cargo xtask run-aarch64 --check-fault

debug-aarch64 log="debug" user_bin="":
    cargo xtask debug-aarch64 --log-level {{ log }} {{ if user_bin != "" { "--user-bin " + user_bin } else { "" } }}

debug-aarch64-fault:
    cargo xtask debug-aarch64 --log-level debug --check-fault

gdb-aarch64:
    aarch64-linux-gnu-gdb target/aarch64-unknown-none-softfloat/debug/genrt-aarch64.elf \
      -ex "target remote :1234" \
      -ex "break _start" \
      -ex "break rust_entry" \
      -ex "break kernel_main"

tree:
    cargo xtask repo-tree
