set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

default:
    @just --list

help:
    @just --list

doctor:
    cargo xtask doctor

phase0-check:
    cargo xtask phase0-check

qemu-cmd-aarch64 initramfs="":
    cargo xtask qemu-cmd --arch aarch64 {{ if initramfs != "" { "--initramfs " + initramfs } else { "" } }}

qemu-cmd-aarch64-fault: build-user-fault
    cargo xtask build-initramfs --init target/aarch64-unknown-none-softfloat/debug/user/fault_null.elf
    cargo xtask qemu-cmd --arch aarch64 --initramfs target/aarch64-unknown-none-softfloat/debug/initramfs.cpio

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

build-user-echo:
    cargo xtask build-user-echo

build-user-cat:
    cargo xtask build-user-cat

build-user-ls:
    cargo xtask build-user-ls

build-initramfs:
    cargo xtask build-initramfs

run-aarch64-read-file log="": build-user-read-file
    cargo xtask run-aarch64 {{ if log != "" { "--log-level " + log } else { "" } }} --init target/aarch64-unknown-none-softfloat/debug/user/read_file.elf

run-aarch64 log="info":
    cargo xtask run-aarch64 --log-level {{ log }}

run-aarch64-shell log="info":
    cargo xtask run-aarch64 --log-level {{ log }}

run-aarch64-fault: build-user-fault
    cargo xtask run-aarch64 --init target/aarch64-unknown-none-softfloat/debug/user/fault_null.elf

debug-aarch64 log="debug":
    cargo xtask debug-aarch64 --log-level {{ log }}

debug-aarch64-shell log="info":
    cargo xtask debug-aarch64 --log-level {{ log }}

debug-aarch64-fault: build-user-fault
    cargo xtask debug-aarch64 --log-level debug --init target/aarch64-unknown-none-softfloat/debug/user/fault_null.elf

gdb-aarch64:
    aarch64-linux-gnu-gdb target/aarch64-unknown-none-softfloat/debug/genrt-aarch64.elf \
      -ex "target remote :1234" \
      -ex "break _start" \
      -ex "break rust_entry" \
      -ex "break kernel_main"

tree:
    cargo xtask repo-tree
