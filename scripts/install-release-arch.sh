#!/usr/bin/env bash
# Install or update fcitx5-smartkey from the latest GitHub release (Arch Linux).
# Downloads the .pkg.tar.zst + .sha256 sidecar, verifies the checksum, then
# installs via `sudo pacman -U` (upgrades in place when an older version exists).
set -euo pipefail

REPO="segfawlt/smartkey"
PKGNAME="fcitx5-smartkey"
CACHEDIR="${XDG_CACHE_HOME:-$HOME/.cache}/smartkey-pkg"

FORCE=0
DRY_RUN=0
while [ $# -gt 0 ]; do
    case "$1" in
        --force|-f) FORCE=1 ;;
        --dry-run) DRY_RUN=1 ;;
        --help|-h)
            cat <<EOF
USAGE:
  $(basename "$0") [--force] [--dry-run]

Downloads the latest ${PKGNAME} Arch package from github.com/${REPO}/releases,
verifies its SHA-256 sidecar, and installs it with \`sudo pacman -U\`.

OPTIONS:
  --force     Reinstall even if the latest version is already installed
  --dry-run   Download + checksum-verify only; skip pacman
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

for cmd in curl sha256sum pacman; do
    command -v "$cmd" >/dev/null 2>&1 || { echo "error: missing dependency: $cmd" >&2; exit 1; }
done

case "$(uname -m)" in
    x86_64) ASSET_ARCH="x86_64" ;;
    aarch64) ASSET_ARCH="aarch64" ;;
    *) echo "error: unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

echo "Querying latest release from github.com/${REPO}..."
META="$(curl -fsSL --retry 3 "https://api.github.com/repos/${REPO}/releases/latest")"

TAG="$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' <<<"${META}" | head -n1)"
if [ -z "${TAG}" ]; then
    echo "error: could not parse latest release tag from GitHub API response" >&2
    exit 1
fi
LATEST_VER="${TAG#v}"

PKG_URL="$(grep -oE 'https://[^"]+' <<<"${META}" | grep -E "${ASSET_ARCH}\.pkg\.tar\.zst$" | head -n1)"
if [ -z "${PKG_URL}" ]; then
    echo "error: release ${TAG} has no ${ASSET_ARCH} .pkg.tar.zst asset" >&2
    echo "       check: https://github.com/${REPO}/releases/tag/${TAG}" >&2
    exit 1
fi
PKG_FILE="$(basename "${PKG_URL}")"

INSTALLED="$(pacman -Q "${PKGNAME}" 2>/dev/null | awk '{print $2}' || true)"
if [ -n "${INSTALLED}" ]; then
    echo "Installed: ${PKGNAME} ${INSTALLED}; latest release: ${TAG}"
    if [ "${INSTALLED%%-*}" = "${LATEST_VER}" ] && [ "${FORCE}" -ne 1 ]; then
        echo "Already up to date. Use --force to reinstall."
        exit 0
    fi
else
    echo "${PKGNAME} not installed yet; latest release: ${TAG}"
fi

mkdir -p "${CACHEDIR}"
echo "Downloading to ${CACHEDIR}..."
(
    cd "${CACHEDIR}"
    rm -f "${PKGNAME}"-*.pkg.tar.zst "${PKGNAME}"-*.pkg.tar.zst.sha256
    curl -fsSL --retry 3 -O "${PKG_URL}"
    curl -fsSL --retry 3 -O "${PKG_URL}.sha256"
    sha256sum -c "${PKG_FILE}.sha256"
)

if [ "${DRY_RUN}" -eq 1 ]; then
    echo "Dry run: verified ${CACHEDIR}/${PKG_FILE}; skipping pacman."
    exit 0
fi

echo "Installing with pacman..."
if [ "${FORCE}" -eq 1 ]; then
    sudo pacman -U "${CACHEDIR}/${PKG_FILE}"
else
    sudo pacman -U --needed "${CACHEDIR}/${PKG_FILE}"
fi

echo ""
pacman -Q "${PKGNAME}"

# Restart a running Fcitx5 so the fresh addon loads immediately
if command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon."
else
    echo "Restart Fcitx5 to activate: fcitx5 -r -d"
fi
