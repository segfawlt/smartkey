#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

PKG_DIR="${ROOT}/packaging/arch"
DIST_DIR="${ROOT}/dist"
PKGNAME="fcitx5-smartkey"
PKGVER="0.1.1"
TARBALL="${PKG_DIR}/${PKGNAME}-${PKGVER}.tar.gz"

mkdir -p "${DIST_DIR}" "${PKG_DIR}"

echo "Packing clean source archive from git files..."
git -C "${ROOT}" ls-files | tar -C "${ROOT}" --transform "s,^,${PKGNAME}-${PKGVER}/," -czf "${TARBALL}" -T -

# Check if makepkg requires -d (nodeps) because cargo/rust is installed via rustup rather than pacman
MAKEPKG_ARGS=(-f -c)
if ! pacman -T cargo >/dev/null 2>&1 && command -v cargo >/dev/null 2>&1; then
    echo "Detected cargo via rustup/PATH instead of pacman database; skipping pacman dep-check with -d"
    MAKEPKG_ARGS+=(-d)
fi

echo "Building Arch package with makepkg..."
(
    cd "${PKG_DIR}"
    export PATH="$HOME/.cargo/bin:$PATH"
    makepkg "${MAKEPKG_ARGS[@]}"
)

echo "Moving package to ${DIST_DIR}..."
# Remove any debug packages if produced
rm -f "${PKG_DIR}"/*-debug-*.pkg.tar.zst
mv "${PKG_DIR}"/${PKGNAME}-*.pkg.tar.zst "${DIST_DIR}/"

# Clean up temporary source tarball
rm -f "${TARBALL}"

echo ""
echo "Successfully built Arch package:"
ls -lh "${DIST_DIR}"/${PKGNAME}-*.pkg.tar.zst
