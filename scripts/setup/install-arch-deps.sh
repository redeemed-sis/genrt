#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=scripts/setup/lib.sh
source "$script_dir/lib.sh"

usage() {
    cat <<'MSG'
Usage: ./scripts/setup/install-arch-deps.sh [--yes]

Install genrt host dependencies on Arch Linux x86_64.
Use --yes to add pacman's --noconfirm flag.
MSG
}

setup_parse_args "$@"
if [[ "$SETUP_HELP" == '1' ]]; then
    usage
    exit 0
fi

setup_load_os_release /etc/os-release
setup_backend_for_os "$SETUP_OS_ID" "$SETUP_OS_VERSION_ID" "$(uname -m)" >/dev/null
[[ "$SETUP_OS_ID" == 'arch' ]] || {
    setup_error "install-arch-deps.sh must run on Arch Linux, found ID=$SETUP_OS_ID"
    exit 1
}

rustup_state=$(setup_prepare_rustup arch)
packages=(base-devel git qemu-system-aarch64 dtc clang lld llvm binutils)
if [[ "$rustup_state" == 'install' ]]; then
    packages+=(rustup)
fi

declare -a pacman_args
setup_arch_pacman_args pacman_args "$SETUP_YES"
setup_run_privileged pacman "${pacman_args[@]}" "${packages[@]}"

setup_activate_toolchain

printf '%s\n' 'Dependency setup complete.'
printf '%s\n' 'Start the production image with: cargo xtask run-aarch64'
printf '%s\n' 'Run the full verification gate with: cargo xtask ci'
