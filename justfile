set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
    @just --list

help:
    @just --list

doctor:
    cargo xtask doctor

phase0-check:
    cargo xtask phase0-check

qemu-cmd-aarch64 user_elf="":
    cargo xtask qemu-cmd --arch aarch64 {{ if user_elf != "" { "--user-elf " + user_elf } else { "" } }}

qemu-cmd-aarch64-fault:
    cargo xtask qemu-cmd --arch aarch64 --check-fault

gdb-cmd-aarch64:
    cargo xtask gdb-cmd --arch aarch64

build-aarch64 log="":
    cargo xtask build-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }}

build-user-hello:
    cargo xtask build-user-hello

build-user-fault:
    cargo xtask build-user-fault

build-user-read-file:
    cargo xtask build-user-read-file

build-user-shell:
    cargo xtask build-user-shell

run-aarch64 log="" user_elf="":
    cargo xtask run-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }} {{ if user_elf != "" { "--user-elf " + user_elf } else { "" } }}

run-aarch64-read-file log="":
    cargo xtask run-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }} --user-elf target/aarch64-unknown-none-softfloat/debug/user/read_file.elf

run-aarch64-shell log="info":
    cargo xtask run-aarch64 --log-level {{ log }} --user-elf target/aarch64-unknown-none-softfloat/debug/user/shell.elf

run-aarch64-fault:
    cargo xtask run-aarch64 --check-fault

debug-aarch64 log="debug" user_elf="":
    cargo xtask debug-aarch64 --log-level {{ log }} {{ if user_elf != "" { "--user-elf " + user_elf } else { "" } }}

debug-aarch64-shell log="info":
    cargo xtask debug-aarch64 --log-level {{ log }} --user-elf target/aarch64-unknown-none-softfloat/debug/user/shell.elf

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
