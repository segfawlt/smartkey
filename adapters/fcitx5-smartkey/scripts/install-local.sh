#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
BUILD_DIR="${ADAPTER_DIR}/build"

cmake -S "${ADAPTER_DIR}" -B "${BUILD_DIR}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="${HOME}/.local" \
    -DCMAKE_INSTALL_LIBDIR=lib

cmake --build "${BUILD_DIR}" -j"$(nproc)"
cmake --install "${BUILD_DIR}"

echo ""
echo "Installed fcitx5-smartkey to:"
echo "  ${HOME}/.local/lib/fcitx5/libsmartkey.so"
echo "  ${HOME}/.local/share/fcitx5/addon/smartkey.conf"

if command -v busctl >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon in-place via DBus."
else
    echo "Restart Fcitx5 in your desktop session to activate: fcitx5 -r -d"
fi
