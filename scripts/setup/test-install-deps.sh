#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd -- "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=lib.sh
source "$script_dir/lib.sh"

fail() {
    printf 'test failure: %s\n' "$*" >&2
    exit 1
}

assert_equal() {
    local actual=$1
    local expected=$2
    local description=$3

    [[ "$actual" == "$expected" ]] || fail "$description: expected '$expected', got '$actual'"
}

assert_fails() {
    local description=$1
    shift

    if "$@" >/dev/null 2>&1; then
        fail "$description: expected failure"
    fi
}

assert_array() {
    local array_name=$1
    shift
    # shellcheck disable=SC2178
    local -n actual=$array_name
    local expected=("$@")

    [[ "${actual[*]}" == "${expected[*]}" ]] || {
        fail "$array_name: expected '${expected[*]}', got '${actual[*]}'"
    }
}

setup_parse_args --help
assert_equal "$SETUP_HELP" 1 '--help is accepted'
assert_equal "$SETUP_YES" 0 '--help does not enable --yes'
setup_parse_args --yes
assert_equal "$SETUP_YES" 1 '--yes is accepted'
assert_fails 'unknown arguments are rejected' setup_parse_args --unexpected

assert_equal "$(setup_backend_for_os arch '' x86_64)" install-arch-deps.sh 'Arch x86_64 dispatch'
assert_equal "$(setup_backend_for_os ubuntu 24.04 x86_64)" install-ubuntu-deps.sh 'Ubuntu 24.04 dispatch'
assert_equal "$(setup_backend_for_os ubuntu 26.04 aarch64)" install-ubuntu-deps.sh 'Ubuntu 26.04 dispatch'
assert_fails 'Arch non-x86_64 is rejected' setup_backend_for_os arch '' aarch64
assert_fails 'unsupported Ubuntu version is rejected' setup_backend_for_os ubuntu 25.04 x86_64
assert_fails 'unsupported distribution is rejected' setup_backend_for_os debian 12 x86_64

temporary_directory=$(mktemp -d)
trap 'rm -rf "$temporary_directory"' EXIT
printf 'ID=ubuntu\nVERSION_ID="24.04"\n' >"$temporary_directory/os-release"
setup_load_os_release "$temporary_directory/os-release"
assert_equal "$SETUP_OS_ID" ubuntu 'os-release ID is loaded'
assert_equal "$SETUP_OS_VERSION_ID" 24.04 'os-release VERSION_ID is loaded'
assert_fails 'missing os-release is rejected' \
    setup_load_os_release "$temporary_directory/missing-os-release"

declare -a pacman_default pacman_yes apt_default apt_yes
setup_arch_pacman_args pacman_default 0
setup_arch_pacman_args pacman_yes 1
setup_apt_install_args apt_default 0
setup_apt_install_args apt_yes 1
assert_array pacman_default -S --needed
assert_array pacman_yes -S --needed --noconfirm
assert_array apt_default install
assert_array apt_yes install --yes

assert_equal "$(setup_privilege_mode 0 no)" direct 'root runs directly'
assert_equal "$(setup_privilege_mode 1000 yes)" sudo 'non-root uses sudo'
assert_fails 'non-root without sudo is rejected' setup_privilege_mode 1000 no

assert_equal "$(setup_rustup_state yes yes yes)" present 'rustup wins over existing commands'
assert_equal "$(setup_rustup_state no no no)" install 'missing Rust commands permit rustup installation'
assert_fails 'system cargo blocks rustup installation' setup_rustup_state no yes no
assert_fails 'system rustc blocks rustup installation' setup_rustup_state no no yes

mkdir "$temporary_directory/migration-path"
ln -s /usr/bin/cat "$temporary_directory/migration-path/cat"
printf '#!/usr/bin/env bash\n' >"$temporary_directory/migration-path/rustc"
chmod +x "$temporary_directory/migration-path/rustc"
migration_output=$(PATH="$temporary_directory/migration-path" setup_print_rust_migration arch 2>&1)
ownership_query_count=$(printf '%s\n' "$migration_output" | grep -c '^  pacman -Qo ')
assert_equal "$ownership_query_count" 1 'migration prints one query for one detected Rust command'
[[ "$migration_output" == *"$temporary_directory/migration-path/rustc"* ]] || {
    fail 'migration ownership query does not name the detected rustc command'
}

printf '%s\n' 'setup helper tests passed'
