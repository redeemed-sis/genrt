# Host dependency setup

The public setup command installs the host dependencies needed to build and
test genrt. It supports only these exact environments:

- Arch Linux on x86_64 (current rolling release);
- Ubuntu 24.04 LTS;
- Ubuntu 26.04 LTS.

From the repository root, run:

```bash
./scripts/setup/install-deps.sh
```

The dispatcher reads `/etc/os-release` and selects an exact backend. It rejects
other `ID` values, unsupported Ubuntu `VERSION_ID` values, and non-x86_64 Arch
hosts before invoking `sudo`, `pacman`, or `apt-get`.

## Privileges and non-interactive use

Package installation runs directly when the script is started as root. For a
normal user it uses `sudo`; if neither is available, it stops with an error.
Run the script normally rather than prefixing it with `sudo` when possible, so
the declarative Rust toolchain belongs to the invoking user.

Pass `--yes` only when non-interactive installation is intended:

```bash
./scripts/setup/install-deps.sh --yes
```

On Arch this adds `--noconfirm` to `pacman`. On Ubuntu it sets
`DEBIAN_FRONTEND=noninteractive`, adds `--yes` to `apt-get install`, and makes
the `add-apt-repository` step non-interactive if it is needed. Without `--yes`,
the package tools retain their normal confirmation prompts.

## Installed packages

The Arch backend uses `pacman -S --needed` and installs:

```text
base-devel git qemu-system-aarch64 dtc clang lld llvm binutils
```

It also installs `rustup` only when the `rustup` command is absent. It never
runs a full system upgrade, adds an AUR package, or installs distribution
`rust`, `rustc`, or `cargo` packages.

The Ubuntu backend runs `apt-get update` before installation and installs:

```text
build-essential git qemu-system-arm device-tree-compiler clang lld llvm
binutils ca-certificates
```

It likewise adds `rustup` only when needed. If `universe` is unavailable after
the initial update, the backend announces this, installs
`software-properties-common` from main, runs `add-apt-repository universe`,
and updates again. It never edits APT source files directly.

## Existing system Rust

If `rustup` is already present, the script validates it and leaves the distro
`rustup` package out of its package request. If `rustup` is absent but `cargo`
or `rustc` is already on `PATH`, the script stops before making package changes
or removing anything. This avoids replacing a system-owned or user-managed
toolchain unexpectedly.

The error message includes a migration path for each Rust command that is
actually present. Review package ownership first, then remove only the system
packages you have chosen to replace. For example:

```bash
# Arch Linux
pacman -Qo "$(command -v rustc)"
sudo pacman -Rns rust

# Ubuntu
dpkg -S "$(command -v cargo)" "$(command -v rustc)"
sudo apt-get remove rustc cargo
```

Rerun the setup command only after the old commands are no longer on `PATH`.
These commands are deliberately not run by the installer.

## Manual setup

Manual installation is useful for image builders and managed hosts. First
resolve any existing-system-Rust conflict described above. Then use the package
manager commands for the appropriate supported platform:

```bash
# Arch Linux x86_64
sudo pacman -S --needed \
  base-devel git rustup qemu-system-aarch64 dtc clang lld llvm binutils

# Ubuntu 24.04 or 26.04
sudo apt-get update
sudo apt-get install \
  build-essential git rustup qemu-system-arm device-tree-compiler clang lld \
  llvm binutils ca-certificates
```

On Ubuntu, enable `universe` through `add-apt-repository universe` and repeat
`apt-get update` if the required repository component is unavailable. Install
`software-properties-common` from main first if `add-apt-repository` is not
installed. Do not edit APT source files manually for this workflow.

From the repository root, Rustup reads `rust-toolchain.toml` declaratively:

```bash
rustup show
cargo --version
rustc --version
rustfmt --version
cargo clippy --version
cargo xtask doctor
```

This obtains the pinned toolchain, target, and components without duplicating
their version or list in shell commands. `cargo xtask doctor` verifies the
target and required host tools; the setup command runs it automatically.

## After setup and troubleshooting

Start the production image with:

```bash
cargo xtask run-aarch64
```

Run the complete verification suite only when requested:

```bash
cargo xtask ci
```

If the dispatcher reports an unsupported platform, use one of the exact
platforms above or install the dependencies manually without using this script.
If `sudo` is missing, rerun as root or arrange approved privilege access. If
`cargo xtask doctor` reports a missing tool after setup, preserve its output and
check that the selected package repository/component is enabled.

Debuggers are optional and are not installed by the setup command. Install
`gdb` plus an AArch64-capable debugger appropriate to the distribution (for
example, `gdb-multiarch` on Ubuntu or `aarch64-linux-gnu-gdb` where packaged).
See [AArch64 debugging](debugging.md) for the debug workflow.
