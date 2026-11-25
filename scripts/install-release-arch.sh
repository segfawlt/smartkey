#!/usr/bin/env bash
# Backwards-compatible Arch release installer wrapper.
# Delegates to the unified installer (`scripts/install.sh --method pacman`).
set -euo pipefail

REPO="segfawlt/smartkey"

if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
    if [ -x "${SCRIPT_DIR}/install.sh" ] || [ -f "${SCRIPT_DIR}/install.sh" ]; then
        exec bash "${SCRIPT_DIR}/install.sh" --method pacman "$@"
    fi
elif [ -f "${PWD}/scripts/install.sh" ] && grep -q 'segfawlt/smartkey' "${PWD}/scripts/install.sh" 2>/dev/null; then
    exec bash "${PWD}/scripts/install.sh" --method pacman "$@"
fi

TMP_INSTALLER="$(mktemp)"
trap 'rm -f "${TMP_INSTALLER}"' EXIT
curl -fsSL --retry 3 "https://github.com/${REPO}/releases/latest/download/install.sh" -o "${TMP_INSTALLER}"
exec bash "${TMP_INSTALLER}" --method pacman "$@"
