#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT}/dist"
PKGNAME="fcitx5-smartkey"
PKGVER="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -n1)"

HOST_UNAME="$(uname -m)"
case "${HOST_UNAME}" in
    x86_64|amd64) DEFAULT_ARCH="linux-x86_64" ;;
    aarch64|arm64) DEFAULT_ARCH="linux-aarch64" ;;
    *) DEFAULT_ARCH="linux-${HOST_UNAME}" ;;
esac

ARCH="${DEFAULT_ARCH}"
USE_CONTAINER=0
CONTAINER_IMAGE="${SMARTKEY_PORTABLE_IMAGE:-debian:12}"

while [ $# -gt 0 ]; do
    case "$1" in
        --container)
            USE_CONTAINER=1
            ;;
        --arch)
            shift
            ARCH="${1:?--arch requires a value}"
            ;;
        --help|-h)
            cat <<EOF
USAGE:
  ./scripts/package-portable.sh [--container] [--arch <linux-x86_64|linux-aarch64>]

OPTIONS:
  --container   Build inside ${CONTAINER_IMAGE} (Docker/Podman) for glibc >= 2.36 compatibility
  --arch <id>   Override archive architecture suffix (default: ${DEFAULT_ARCH})
EOF
            exit 0
            ;;
        *)
            echo "error: unknown option $1" >&2
            exit 2
            ;;
    esac
    shift
done

"${SCRIPT_DIR}/check-versions.sh"

if [ "${USE_CONTAINER}" -eq 1 ]; then
    if command -v docker >/dev/null 2>&1; then
        RUNTIME="docker"
    elif command -v podman >/dev/null 2>&1; then
        RUNTIME="podman"
    else
        echo "error: --container requires docker or podman in PATH" >&2
        exit 1
    fi
    echo "Building portable bundle inside ${CONTAINER_IMAGE} via ${RUNTIME} (glibc 2.36 ABI floor)..."
    "${RUNTIME}" run --rm \
        -v "${ROOT}:/work" \
        -w /work \
        "${CONTAINER_IMAGE}" \
        bash -euo pipefail -c '
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y -qq --no-install-recommends \
                build-essential cmake pkg-config libfcitx5core-dev libfcitx5utils-dev \
                curl ca-certificates git zstd file binutils
            if ! command -v cargo >/dev/null 2>&1; then
                curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
            fi
            export PATH="$HOME/.cargo/bin:$PATH"
            export CARGO_TARGET_DIR=/tmp/smartkey-target
            SMARTKEY_BUILD_DIR=/tmp/smartkey-cmake ./scripts/package-portable.sh --arch "'"${ARCH}"'"
        '
    exit 0
fi

STAGE_NAME="${PKGNAME}-${PKGVER}-${ARCH}"
ARCHIVE_NAME="${STAGE_NAME}.tar.zst"
CARGO_OUT_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"
ADAPTER_DIR="${ROOT}/adapters/fcitx5-smartkey"
BUILD_DIR="${SMARTKEY_BUILD_DIR:-${ADAPTER_DIR}/build-portable}"

mkdir -p "${DIST_DIR}"

echo "Building smartkey binary..."
cargo build --release --locked --manifest-path "${ROOT}/Cargo.toml" -p smartkey-cli --bin smartkey

echo "Building fcitx5-smartkey addon..."
cmake -S "${ADAPTER_DIR}" -B "${BUILD_DIR}" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="${HOME}/.local" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DSMARTKEY_CARGO_FLAGS="--locked"
cmake --build "${BUILD_DIR}" -j"$(nproc)"
ctest --test-dir "${BUILD_DIR}" --output-on-failure

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "${TMP_DIR}"' EXIT

STAGE_DIR="${TMP_DIR}/${STAGE_NAME}"
mkdir -p "${STAGE_DIR}/bin" "${STAGE_DIR}/lib/fcitx5" "${STAGE_DIR}/share/smartkey/dicts"

# Copy binary & symlink smartkeyctl
install -m 755 "${CARGO_OUT_DIR}/release/smartkey" "${STAGE_DIR}/bin/smartkey"
strip --strip-unneeded "${STAGE_DIR}/bin/smartkey" 2>/dev/null || true
ln -s smartkey "${STAGE_DIR}/bin/smartkeyctl"
install -m 755 "${BUILD_DIR}/libsmartkey.so" "${STAGE_DIR}/lib/fcitx5/libsmartkey.so"
strip --strip-unneeded "${STAGE_DIR}/lib/fcitx5/libsmartkey.so"

# Copy default English dictionary pack
install -m 644 "${ROOT}/crates/smartkey-core/assets/en_lexicon.skdict" "${STAGE_DIR}/share/smartkey/dicts/en.skdict"

# Copy docs and license
install -m 644 "${ROOT}/LICENSE" "${STAGE_DIR}/LICENSE"
install -m 644 "${ROOT}/README.md" "${STAGE_DIR}/README.md"

# Generate install.sh (withPKGVER baked in, escaping runtime shell vars)
cat > "${STAGE_DIR}/install.sh" << EOF
#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"

BIN_DIR="\${XDG_BIN_HOME:-\$HOME/.local/bin}"
LIB_DIR="\${HOME}/.local/lib/fcitx5"
CONF_DIR="\${XDG_DATA_HOME:-\$HOME/.local/share}/fcitx5/addon"
DICT_DIR="\${XDG_DATA_HOME:-\$HOME/.local/share}/smartkey/dicts"

mkdir -p "\${BIN_DIR}" "\${LIB_DIR}" "\${CONF_DIR}" "\${DICT_DIR}"

# Install smartkey & symlink smartkeyctl
install -m 755 "\${SCRIPT_DIR}/bin/smartkey" "\${BIN_DIR}/smartkey"
ln -sf smartkey "\${BIN_DIR}/smartkeyctl"

# Install default English dictionary
install -m 644 "\${SCRIPT_DIR}/share/smartkey/dicts/en.skdict" "\${DICT_DIR}/en.skdict"

# Install libsmartkey.so
install -m 755 "\${SCRIPT_DIR}/lib/fcitx5/libsmartkey.so" "\${LIB_DIR}/libsmartkey.so"

# Remove legacy smartkey binary in ~/.cargo/bin if present
if [ -f "\${HOME}/.cargo/bin/smartkey" ]; then
    rm -f "\${HOME}/.cargo/bin/smartkey"
fi

# Dynamically generate smartkey.conf pointing to user's ~/.local/lib/fcitx5/libsmartkey
cat > "\${CONF_DIR}/smartkey.conf" << CONF_EOF
[Addon]
Name=SmartKey Autocorrect
Comment=On-device PC English autocorrect, backspace learner, and fcitx5-lotus Telex bridge
Type=SharedLibrary
Library=\${LIB_DIR}/libsmartkey
Category=Module
Version=${PKGVER}
OnDemand=False
Configurable=False

[Addon/Dependencies]
0=core:5.0.20
CONF_EOF

echo "Installed fcitx5-smartkey ${PKGVER} (portable):"
echo "  Binary: \${BIN_DIR}/smartkey (aliased as smartkeyctl)"
echo "  Addon:  \${LIB_DIR}/libsmartkey.so"
echo "  Dict:   \${DICT_DIR}/en.skdict"
echo "  Config: \${CONF_DIR}/smartkey.conf"

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

echo "Compressing portable archive to ${DIST_DIR}/${ARCHIVE_NAME}..."
tar -C "${TMP_DIR}" -cf - "${STAGE_NAME}" | zstd -q -19 -f -o "${DIST_DIR}/${ARCHIVE_NAME}"
(
    cd "${DIST_DIR}"
    sha256sum "${ARCHIVE_NAME}" > "${ARCHIVE_NAME}.sha256"
)

echo ""
echo "Successfully created portable package and SHA-256 checksum:"
ls -lh "${DIST_DIR}/${ARCHIVE_NAME}" "${DIST_DIR}/${ARCHIVE_NAME}.sha256"
