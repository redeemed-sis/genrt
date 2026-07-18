#!/usr/bin/env bash
# Shared helpers for the supported host dependency installers.

setup_error() {
    printf 'error: %s\n' "$*" >&2
    return 1
}

setup_parse_args() {
    SETUP_YES=0
    SETUP_HELP=0

    local argument
    for argument in "$@"; do
        case "$argument" in
            --yes)
                # Public result consumed by the sourcing installer.
                # shellcheck disable=SC2034
                SETUP_YES=1
                ;;
            --help|-h)
                # Public result consumed by the sourcing installer.
                # shellcheck disable=SC2034
                SETUP_HELP=1
                ;;
            *)
                setup_error "unknown argument: $argument"
                return 1
                ;;
        esac
    done
}

setup_load_os_release() {
    local os_release=$1

    [[ -r "$os_release" ]] || {
        setup_error "cannot read $os_release"
        return 1
    }

    ID=''
    VERSION_ID=''
    # os-release is specified as shell-compatible variable assignments.
    # shellcheck disable=SC1090
    source "$os_release"

    [[ -n ${ID:-} ]] || {
        setup_error "$os_release does not define ID"
        return 1
    }

    # Public results consumed by the sourcing installer.
    # shellcheck disable=SC2034
    SETUP_OS_ID=$ID
    # shellcheck disable=SC2034
    SETUP_OS_VERSION_ID=${VERSION_ID:-}
}

setup_backend_for_os() {
    local id=$1
    local version_id=$2
    local architecture=$3

    case "$id" in
        arch)
            [[ "$architecture" == 'x86_64' ]] || {
                setup_error "Arch Linux is supported only on x86_64, found $architecture"
                return 1
            }
            printf '%s\n' 'install-arch-deps.sh'
            ;;
        ubuntu)
            case "$version_id" in
                24.04|26.04)
                    printf '%s\n' 'install-ubuntu-deps.sh'
                    ;;
                *)
                    setup_error "unsupported Ubuntu VERSION_ID=$version_id; supported versions are 24.04 and 26.04"
                    return 1
                    ;;
            esac
            ;;
        *)
            setup_error "unsupported distribution ID=$id; supported distributions are Arch Linux x86_64 and Ubuntu 24.04 or 26.04"
            return 1
            ;;
    esac
}

setup_privilege_mode() {
    local effective_uid=$1
    local sudo_available=$2

    if [[ "$effective_uid" == '0' ]]; then
        printf '%s\n' 'direct'
    elif [[ "$sudo_available" == 'yes' ]]; then
        printf '%s\n' 'sudo'
    else
        setup_error 'root privileges are required, but sudo is not available; rerun as root or install sudo'
        return 1
    fi
}

setup_command_available() {
    if command -v "$1" >/dev/null 2>&1; then
        printf '%s\n' 'yes'
    else
        printf '%s\n' 'no'
    fi
}

setup_run_privileged() {
    local sudo_available mode
    sudo_available=$(setup_command_available sudo)
    mode=$(setup_privilege_mode "$(id -u)" "$sudo_available") || return 1

    if [[ "$mode" == 'direct' ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

setup_arch_pacman_args() {
    local output_name=$1
    local yes=$2
    # Bash 4.3+ is available on the supported hosts.
    # shellcheck disable=SC2178
    local -n output=$output_name

    output=(-S --needed)
    if [[ "$yes" == '1' ]]; then
        output+=(--noconfirm)
    fi
}

setup_apt_install_args() {
    local output_name=$1
    local yes=$2
    # Bash 4.3+ is available on the supported hosts.
    # shellcheck disable=SC2178
    local -n output=$output_name

    output=(install)
    if [[ "$yes" == '1' ]]; then
        output+=(--yes)
    fi
}

setup_rustup_state() {
    local rustup_available=$1
    local cargo_available=$2
    local rustc_available=$3

    if [[ "$rustup_available" == 'yes' ]]; then
        printf '%s\n' 'present'
    elif [[ "$cargo_available" == 'yes' || "$rustc_available" == 'yes' ]]; then
        setup_error 'rustup is absent but cargo or rustc is already on PATH'
        return 1
    else
        printf '%s\n' 'install'
    fi
}

setup_print_rust_migration() {
    local distribution=$1 tool tool_path

    printf '%s\n' 'No package changes were made.' >&2
    case "$distribution" in
        arch)
            cat >&2 <<'MSG'
To migrate deliberately on Arch Linux, first review the owning packages, then
remove the system Rust packages yourself if appropriate. Review each command
that is actually present:
MSG
            for tool in cargo rustc; do
                tool_path=$(command -v "$tool" 2>/dev/null || true)
                if [[ -n "$tool_path" ]]; then
                    printf '  pacman -Qo %q\n' "$tool_path" >&2
                fi
            done
            cat >&2 <<'MSG'

Then remove the package names reported by pacman, for example:

  sudo pacman -Rns rust

Rerun this installer after the system Rust commands are no longer on PATH.
MSG
            ;;
        ubuntu)
            cat >&2 <<'MSG'
To migrate deliberately on Ubuntu, first review the owning packages, then
remove the system Rust packages yourself if appropriate. Review each command
that is actually present:
MSG
            for tool in cargo rustc; do
                tool_path=$(command -v "$tool" 2>/dev/null || true)
                if [[ -n "$tool_path" ]]; then
                    printf '  dpkg -S %q\n' "$tool_path" >&2
                fi
            done
            cat >&2 <<'MSG'

Then remove the package names reported by dpkg, for example:

  sudo apt-get remove rustc cargo

Rerun this installer after the system Rust commands are no longer on PATH.
MSG
            ;;
    esac
}

setup_prepare_rustup() {
    local distribution=$1
    local state

    if ! state=$(setup_rustup_state \
        "$(setup_command_available rustup)" \
        "$(setup_command_available cargo)" \
        "$(setup_command_available rustc)"); then
        setup_print_rust_migration "$distribution"
        return 1
    fi

    if [[ "$state" == 'present' ]]; then
        rustup --version >&2 || {
            setup_error 'rustup is present but failed validation'
            return 1
        }
    fi

    printf '%s\n' "$state"
}

setup_repo_root() {
    cd -- "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P
}

setup_activate_toolchain() {
    local repository_root sysroot
    repository_root=$(setup_repo_root)

    (
        cd -- "$repository_root" || exit 1
        rustup show
        cargo --version
        rustc --version
        rustfmt --version
        cargo clippy --version

        sysroot=$(rustc --print sysroot)
        [[ -f "$sysroot/lib/rustlib/src/rust/library/Cargo.toml" ]] || {
            setup_error 'rust-src is unavailable in the active declarative toolchain'
            exit 1
        }

        cargo xtask doctor
    )
}
