#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT}/dist"
PKGNAME="fcitx5-smartkey"
PKGVER="0.1.1"
ARCH="linux-x86_64"
STAGE_NAME="${PKGNAME}-${PKGVER}-${ARCH}"

mkdir -p "${DIST_DIR}"

echo "Building smartkey binary..."
cargo build --release --manifest-path "${ROOT}/Cargo.toml" -p smartkey-cli --bin smartkey

echo "Building fcitx5-smartkey addon..."
ADAPTER_DIR="${ROOT}/adapters/fcitx5-smartkey"
BUILD_DIR="${ADAPTER_DIR}/build"
cmake -S "${ADAPTER_DIR}" -B "${BUILD_DIR}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="${HOME}/.local" \
    -DCMAKE_INSTALL_LIBDIR=lib
cmake --build "${BUILD_DIR}" -j"$(nproc)"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

STAGE_DIR="${TMP_DIR}/${STAGE_NAME}"
mkdir -p "${STAGE_DIR}/bin" "${STAGE_DIR}/lib/fcitx5" "${STAGE_DIR}/share/smartkey/dicts"

# Copy binary & symlink smartkeyctl
install -m 755 "${ROOT}/target/release/smartkey" "${STAGE_DIR}/bin/smartkey"
ln -s smartkey "${STAGE_DIR}/bin/smartkeyctl"
install -m 755 "${BUILD_DIR}/libsmartkey.so" "${STAGE_DIR}/lib/fcitx5/libsmartkey.so"
strip --strip-unneeded "${STAGE_DIR}/lib/fcitx5/libsmartkey.so"

# Copy default English dictionary pack
install -m 644 "${ROOT}/crates/smartkey-core/assets/en_lexicon.skdict" "${STAGE_DIR}/share/smartkey/dicts/en.skdict"

# Copy docs and license
install -m 644 "${ROOT}/LICENSE" "${STAGE_DIR}/LICENSE"
install -m 644 "${ROOT}/README.md" "${STAGE_DIR}/README.md"

# Generate install.sh
cat > "${STAGE_DIR}/install.sh" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
LIB_DIR="${HOME}/.local/lib/fcitx5"
CONF_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/addon"
DICT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/smartkey/dicts"

mkdir -p "${BIN_DIR}" "${LIB_DIR}" "${CONF_DIR}" "${DICT_DIR}"

# Install smartkey & symlink smartkeyctl
install -m 755 "${SCRIPT_DIR}/bin/smartkey" "${BIN_DIR}/smartkey"
ln -sf smartkey "${BIN_DIR}/smartkeyctl"

# Install default English dictionary
install -m 644 "${SCRIPT_DIR}/share/smartkey/dicts/en.skdict" "${DICT_DIR}/en.skdict"

# Install libsmartkey.so
install -m 755 "${SCRIPT_DIR}/lib/fcitx5/libsmartkey.so" "${LIB_DIR}/libsmartkey.so"

# Remove legacy smartkey binary in ~/.cargo/bin if present
if [ -f "${HOME}/.cargo/bin/smartkey" ]; then
    rm -f "${HOME}/.cargo/bin/smartkey"
fi

# Dynamically generate smartkey.conf pointing to user's ~/.local/lib/fcitx5/libsmartkey
cat > "${CONF_DIR}/smartkey.conf" << CONF_EOF
[Addon]
Name=SmartKey Autocorrect
Comment=On-device PC English autocorrect and deterministic backspace learner
Type=SharedLibrary
Library=${LIB_DIR}/libsmartkey
Category=Module
Version=0.1.1
OnDemand=False
Configurable=False

[Dependencies]
0=core:5.0.0
CONF_EOF

echo "Installed fcitx5-smartkey (portable):"
echo "  Binary: ${BIN_DIR}/smartkey (aliased as smartkeyctl)"
echo "  Addon:  ${LIB_DIR}/libsmartkey.so"
echo "  Dict:   ${DICT_DIR}/en.skdict"
echo "  Config: ${CONF_DIR}/smartkey.conf"

if command -v busctl >/dev/null 2>&1 && command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon in-place via DBus."
else
    echo "Restart Fcitx5 in your desktop session to activate: fcitx5 -r -d"
fi
EOF
chmod 755 "${STAGE_DIR}/install.sh"

# Generate uninstall.sh
cat > "${STAGE_DIR}/uninstall.sh" << 'EOF'
#!/usr/bin/env bash
set -euo pipefail

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
LIB_DIR="${HOME}/.local/lib/fcitx5"
CONF_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/addon"
DICT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/smartkey/dicts"

rm -f "${BIN_DIR}/smartkey" "${BIN_DIR}/smartkeyctl"
rm -f "${LIB_DIR}/libsmartkey.so"
rm -f "${CONF_DIR}/smartkey.conf"
rm -f "${DICT_DIR}/en.skdict"

echo "Removed fcitx5-smartkey from ~/.local."

if command -v busctl >/dev/null 2>&1 && command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon in-place via DBus."
else
    echo "Restart Fcitx5 in your desktop session to unload: fcitx5 -r -d"
fi
EOF
chmod 755 "${STAGE_DIR}/uninstall.sh"

echo "Compressing portable archive to ${DIST_DIR}/${STAGE_NAME}.tar.zst..."
tar --zstd -cf "${DIST_DIR}/${STAGE_NAME}.tar.zst" -C "${TMP_DIR}" "${STAGE_NAME}"

echo ""
echo "Successfully created portable package:"
ls -lh "${DIST_DIR}/${STAGE_NAME}.tar.zst"
