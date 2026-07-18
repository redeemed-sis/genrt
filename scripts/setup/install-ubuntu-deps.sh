#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=scripts/setup/lib.sh
source "$script_dir/lib.sh"

usage() {
    cat <<'MSG'
Usage: ./scripts/setup/install-ubuntu-deps.sh [--yes]

Install genrt host dependencies on Ubuntu 24.04 or 26.04.
Use --yes to enable non-interactive apt-get and add-apt-repository commands.
MSG
}

apt_get() {
    if [[ "$SETUP_YES" == '1' ]]; then
        setup_run_privileged env DEBIAN_FRONTEND=noninteractive apt-get "$@"
    else
        setup_run_privileged apt-get "$@"
    fi
}

enable_universe() {
    declare -a install_args

    printf '%s\n' 'Ubuntu universe is unavailable; enabling it with add-apt-repository.'
    setup_apt_install_args install_args "$SETUP_YES"
    apt_get "${install_args[@]}" software-properties-common
    if [[ "$SETUP_YES" == '1' ]]; then
        setup_run_privileged env DEBIAN_FRONTEND=noninteractive add-apt-repository --yes universe
    else
        setup_run_privileged add-apt-repository universe
    fi
    apt_get update
}

ubuntu_has_universe() {
    # `$(COMPONENT)` is apt's format placeholder, not a shell expression.
    # shellcheck disable=SC2016
    apt-cache indextargets --format '$(COMPONENT)' 2>/dev/null | grep -Fxq universe
}

setup_parse_args "$@"
if [[ "$SETUP_HELP" == '1' ]]; then
    usage
    exit 0
fi

setup_load_os_release /etc/os-release
setup_backend_for_os "$SETUP_OS_ID" "$SETUP_OS_VERSION_ID" "$(uname -m)" >/dev/null
[[ "$SETUP_OS_ID" == 'ubuntu' ]] || {
    setup_error "install-ubuntu-deps.sh must run on Ubuntu, found ID=$SETUP_OS_ID"
    exit 1
}

rustup_state=$(setup_prepare_rustup ubuntu)

apt_get update
if ! ubuntu_has_universe; then
    enable_universe
fi

packages=(build-essential git qemu-system-arm device-tree-compiler clang lld llvm binutils ca-certificates)
if [[ "$rustup_state" == 'install' ]]; then
    packages+=(rustup)
fi

declare -a apt_install_args
setup_apt_install_args apt_install_args "$SETUP_YES"
apt_get "${apt_install_args[@]}" "${packages[@]}"

setup_activate_toolchain

printf '%s\n' 'Dependency setup complete.'
printf '%s\n' 'Start the production image with: cargo xtask run-aarch64'
printf '%s\n' 'Run the full verification gate with: cargo xtask ci'
