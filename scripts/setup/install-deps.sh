#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

usage() {
    cat <<'MSG'
Usage: ./scripts/setup/install-deps.sh [--yes]

Install genrt host dependencies on supported systems:
  - Arch Linux x86_64
  - Ubuntu 24.04 or 26.04

Use --yes to enable non-interactive package installation.
MSG
}

setup_parse_args "$@"
if [[ "$SETUP_HELP" == '1' ]]; then
    usage
    exit 0
fi

setup_load_os_release /etc/os-release
backend=$(setup_backend_for_os "$SETUP_OS_ID" "$SETUP_OS_VERSION_ID" "$(uname -m)")

exec "$script_dir/$backend" "$@"
