#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
ROOT_DIR="$(cd "${ADAPTER_DIR}/../.." && pwd)"
BUILD_DIR="${ADAPTER_DIR}/build"
BIN_DIR="${HOME}/.local/bin"
DICT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/smartkey/dicts"

mkdir -p "${BIN_DIR}" "${DICT_DIR}"

echo "Building smartkey CLI..."
cargo build --release --manifest-path "${ROOT_DIR}/Cargo.toml" -p smartkey-cli --bin smartkey
install -m 755 "${ROOT_DIR}/target/release/smartkey" "${BIN_DIR}/smartkey"
ln -sf smartkey "${BIN_DIR}/smartkeyctl"
install -m 644 "${ROOT_DIR}/crates/smartkey-core/assets/en_lexicon.skdict" "${DICT_DIR}/en.skdict"

if [ -f "${HOME}/.cargo/bin/smartkey" ]; then
    rm -f "${HOME}/.cargo/bin/smartkey"
    echo "Removed legacy binary: ${HOME}/.cargo/bin/smartkey"
fi

echo "Building fcitx5-smartkey addon..."
if [ -f "${BUILD_DIR}/CMakeCache.txt" ] && grep -q 'CMAKE_INSTALL_PREFIX:PATH=/usr' "${BUILD_DIR}/CMakeCache.txt"; then
    rm -rf "${BUILD_DIR}"
fi
cmake -S "${ADAPTER_DIR}" -B "${BUILD_DIR}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="${HOME}/.local" \
    -DCMAKE_INSTALL_LIBDIR=lib

cmake --build "${BUILD_DIR}" -j"$(nproc)"
cmake --install "${BUILD_DIR}"

echo ""
echo "Installed fcitx5-smartkey to:"
echo "  ${BIN_DIR}/smartkey (aliased as smartkeyctl)"
echo "  ${HOME}/.local/lib/fcitx5/libsmartkey.so"
echo "  ${HOME}/.local/share/fcitx5/addon/smartkey.conf"
echo "  ${DICT_DIR}/en.skdict"

if command -v busctl >/dev/null 2>&1 && command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon in-place via DBus."
else
    echo "Restart Fcitx5 in your desktop session to activate: fcitx5 -r -d"
fi
