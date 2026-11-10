#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

CARGO_VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -n1)"
CMAKE_VER="$(sed -n 's/^project(fcitx5-smartkey VERSION \([^ ]*\) .*)/\1/p' "${ROOT}/adapters/fcitx5-smartkey/CMakeLists.txt" | head -n1)"
FFI_HEADER_VER="$(sed -n 's/^ \* SmartKey C\/C++ FFI Header (v\([^)]*\)).*/\1/p' "${ROOT}/crates/smartkey-ffi/include/smartkey.h" | head -n1)"
PKGBUILD_VER="$(sed -n 's/^pkgver=\(.*\)/\1/p' "${ROOT}/packaging/arch/PKGBUILD" | head -n1)"

FAIL=0

check_eq() {
    local label="$1"
    local actual="$2"
    if [ "${actual}" != "${CARGO_VER}" ]; then
        echo "error: version mismatch in ${label}: expected '${CARGO_VER}' (from Cargo.toml), found '${actual}'" >&2
        FAIL=1
    fi
}

if [ -z "${CARGO_VER}" ]; then
    echo "error: could not parse workspace version from Cargo.toml" >&2
    exit 1
fi

check_eq "adapters/fcitx5-smartkey/CMakeLists.txt" "${CMAKE_VER}"
check_eq "crates/smartkey-ffi/include/smartkey.h" "${FFI_HEADER_VER}"
check_eq "packaging/arch/PKGBUILD" "${PKGBUILD_VER}"

if [ "${FAIL}" -ne 0 ]; then
    exit 1
fi

echo "Version check passed: ${CARGO_VER}"
