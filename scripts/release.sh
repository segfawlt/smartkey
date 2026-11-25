#!/usr/bin/env bash
set -euo pipefail

export PATH="$HOME/.cargo/bin:$PATH"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT}/dist"
PKGNAME="fcitx5-smartkey"
REPO="segfawlt/smartkey"

BUMP_SPEC="patch"
BUMP_SET=0
SUBTITLE=""
CUSTOM_NOTES=""
NOTES_SOURCE_FILE=""
NO_LOCAL_INSTALL=0
DRY_RUN=0

usage() {
    cat <<'EOF'
USAGE:
  ./scripts/release.sh [patch|minor|major|<X.Y.Z>|v<X.Y.Z>] [OPTIONS]

Automates version bumping, Rust/C++ verification, tagging, portable + Arch
packaging, local Fcitx5 install sync, pushing `main` + tag, and publishing a
GitHub release.

ARGUMENTS:
  patch|minor|major|<X.Y.Z>   Version bump level or explicit SemVer (default: patch)

OPTIONS:
  --title <subtitle>          Release title suffix (`SmartKey vX.Y.Z — <subtitle>`)
  --notes <markdown>          Custom release highlights markdown
  --notes-file <path>         Read custom release highlights markdown from file
  --no-local-install          Skip `./adapters/fcitx5-smartkey/scripts/install-local.sh`
  --dry-run                   Validate, compute version/notes, and print plan without mutating git or GitHub
  -h, --help                  Show this help message

EXAMPLES:
  ./scripts/release.sh
  ./scripts/release.sh 0.2.2 --title "DBus & SurroundingText Delete Guards"
  ./scripts/release.sh minor --dry-run
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --title)
            shift
            [ $# -gt 0 ] || { echo "error: --title requires a value" >&2; exit 2; }
            SUBTITLE="$1"
            ;;
        --notes)
            shift
            [ $# -gt 0 ] || { echo "error: --notes requires a value" >&2; exit 2; }
            CUSTOM_NOTES="$1"
            ;;
        --notes-file)
            shift
            [ $# -gt 0 ] || { echo "error: --notes-file requires a path" >&2; exit 2; }
            NOTES_SOURCE_FILE="$1"
            ;;
        --no-local-install)
            NO_LOCAL_INSTALL=1
            ;;
        --dry-run)
            DRY_RUN=1
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        -*)
            echo "error: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
        *)
            if [ "${BUMP_SET}" -ne 0 ]; then
                echo "error: unexpected extra argument: $1" >&2
                usage >&2
                exit 2
            fi
            BUMP_SPEC="$1"
            BUMP_SET=1
            ;;
    esac
    shift
done

if [ -n "${CUSTOM_NOTES}" ] && [ -n "${NOTES_SOURCE_FILE}" ]; then
    echo "error: --notes and --notes-file are mutually exclusive" >&2
    exit 2
fi

if [ -n "${NOTES_SOURCE_FILE}" ] && [ ! -f "${NOTES_SOURCE_FILE}" ]; then
    echo "error: notes file not found: ${NOTES_SOURCE_FILE}" >&2
    exit 1
fi

for cmd in git cargo cmake ctest zstd sha256sum makepkg gh; do
    command -v "$cmd" >/dev/null 2>&1 || {
        echo "error: required command not found in PATH: $cmd" >&2
        exit 1
    }
done

BRANCH="$(git -C "${ROOT}" branch --show-current)"
if [ "${BRANCH}" != "main" ]; then
    echo "error: releases must be cut from 'main' (current branch: '${BRANCH:-detached}')" >&2
    exit 1
fi

if [ -n "$(git -C "${ROOT}" status --porcelain --untracked-files=no)" ]; then
    echo "error: tracked working tree changes present; commit or stash before releasing" >&2
    git -C "${ROOT}" status --short --untracked-files=no >&2
    exit 1
fi

if [ "${DRY_RUN}" -eq 0 ]; then
    if ! gh auth status >/dev/null 2>&1; then
        echo "error: gh CLI is not authenticated; run 'gh auth login' first" >&2
        exit 1
    fi
fi

CURRENT_VER="$(sed -n 's/^version = "\(.*\)"/\1/p' "${ROOT}/Cargo.toml" | head -n1)"
if [[ ! "${CURRENT_VER}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "error: could not parse SemVer version from Cargo.toml (found '${CURRENT_VER}')" >&2
    exit 1
fi
CUR_MAJOR="${BASH_REMATCH[1]}"
CUR_MINOR="${BASH_REMATCH[2]}"
CUR_PATCH="${BASH_REMATCH[3]}"

case "${BUMP_SPEC}" in
    patch)
        NEW_VER="${CUR_MAJOR}.${CUR_MINOR}.$((CUR_PATCH + 1))"
        ;;
    minor)
        NEW_VER="${CUR_MAJOR}.$((CUR_MINOR + 1)).0"
        ;;
    major)
        NEW_VER="$((CUR_MAJOR + 1)).0.0"
        ;;
    *)
        NEW_VER="${BUMP_SPEC#v}"
        if [[ ! "${NEW_VER}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            echo "error: invalid version or bump level '${BUMP_SPEC}' (expected patch|minor|major|<X.Y.Z>)" >&2
            exit 1
        fi
        ;;
esac

TAG="v${NEW_VER}"
if git -C "${ROOT}" rev-parse -q --verify "refs/tags/${TAG}" >/dev/null 2>&1; then
    echo "error: git tag '${TAG}' already exists locally" >&2
    exit 1
fi

if [ -n "${SUBTITLE}" ]; then
    RELEASE_TITLE="SmartKey ${TAG} — ${SUBTITLE}"
else
    RELEASE_TITLE="SmartKey ${TAG}"
fi

HOST_UNAME="$(uname -m)"
case "${HOST_UNAME}" in
    x86_64|amd64)
        HOST_ARCH="x86_64"
        PORTABLE_ARCH="linux-x86_64"
        ;;
    aarch64|arm64)
        HOST_ARCH="aarch64"
        PORTABLE_ARCH="linux-aarch64"
        ;;
    *)
        HOST_ARCH="${HOST_UNAME}"
        PORTABLE_ARCH="linux-${HOST_UNAME}"
        ;;
esac

PORTABLE_ASSET="${PKGNAME}-${NEW_VER}-${PORTABLE_ARCH}.tar.zst"
ARCH_ASSET="${PKGNAME}-${NEW_VER}-1-${HOST_ARCH}.pkg.tar.zst"

PREV_TAG="$(git -C "${ROOT}" describe --tags --abbrev=0 2>/dev/null || true)"
if [ -n "${NOTES_SOURCE_FILE}" ]; then
    HIGHLIGHTS="$(cat "${NOTES_SOURCE_FILE}")"
elif [ -n "${CUSTOM_NOTES}" ]; then
    HIGHLIGHTS="${CUSTOM_NOTES}"
else
    LOG_RANGE="${PREV_TAG:+${PREV_TAG}..}HEAD"
    HIGHLIGHTS="$(git -C "${ROOT}" log --no-merges --invert-grep --grep='^chore(release):' --pretty=format:'- %s (%h)' "${LOG_RANGE}" || true)"
    if [ -z "${HIGHLIGHTS}" ]; then
        HIGHLIGHTS="- Maintenance and stability updates."
    fi
fi

NOTES_FILE="$(mktemp)"
PRE_RELEASE_SHA="$(git -C "${ROOT}" rev-parse HEAD)"
PUSH_STARTED=0
CREATED_TAG=0
CREATED_COMMIT=0
MODIFIED_FILES=0

cleanup() {
    local rc=$?
    rm -f "${NOTES_FILE}"
    if [ "${rc}" -ne 0 ] && [ "${PUSH_STARTED}" -eq 0 ]; then
        echo "" >&2
        echo "Release aborted (exit ${rc}); rolling back local git state..." >&2
        if [ "${CREATED_TAG}" -eq 1 ]; then
            git -C "${ROOT}" tag -d "${TAG}" >/dev/null 2>&1 || true
        fi
        if [ "${CREATED_COMMIT}" -eq 1 ]; then
            git -C "${ROOT}" reset --hard "${PRE_RELEASE_SHA}" >/dev/null 2>&1 || true
        elif [ "${MODIFIED_FILES}" -eq 1 ]; then
            git -C "${ROOT}" checkout -- \
                Cargo.toml \
                Cargo.lock \
                adapters/fcitx5-smartkey/CMakeLists.txt \
                crates/smartkey-ffi/include/smartkey.h \
                packaging/arch/PKGBUILD >/dev/null 2>&1 || true
        fi
    fi
}
trap cleanup EXIT

cat > "${NOTES_FILE}" <<EOF
## Highlights

${HIGHLIGHTS}

## Installation

### Quick Install / Update (Recommended — Linux)
Auto-detects Arch (\`pacman\`) or portable (\`~/.local\`), verifies SHA-256 checksums, and reloads Fcitx5:
\`\`\`bash
curl -fsSL https://github.com/${REPO}/releases/latest/download/install.sh | bash
\`\`\`

### Manual: Arch Linux (pacman)
\`\`\`bash
sudo pacman -U ${ARCH_ASSET}
\`\`\`

### Manual: Portable Tarball (Any Linux distro with Fcitx5)
\`\`\`bash
tar -xf ${PORTABLE_ASSET}
cd ${PKGNAME}-${NEW_VER}-${PORTABLE_ARCH}
./install.sh
\`\`\`
EOF

if [ "${DRY_RUN}" -eq 1 ]; then
    "${SCRIPT_DIR}/check-versions.sh"
    echo ""
    echo "=== Dry Run Summary ==="
    echo "Current version : ${CURRENT_VER}"
    echo "Target version  : ${NEW_VER} (tag: ${TAG})"
    echo "Previous tag    : ${PREV_TAG:-<none>}"
    echo "Release title   : ${RELEASE_TITLE}"
    echo "Local install   : $([ "${NO_LOCAL_INSTALL}" -eq 1 ] && echo 'skip' || echo 'yes')"
    echo "Artifacts       :"
    echo "  - scripts/install.sh"
    echo "  - dist/${PORTABLE_ASSET}"
    echo "  - dist/${PORTABLE_ASSET}.sha256"
    echo "  - dist/${ARCH_ASSET}"
    echo "  - dist/${ARCH_ASSET}.sha256"
    echo ""
    echo "=== Release Notes Preview ==="
    cat "${NOTES_FILE}"
    exit 0
fi

echo "=== SmartKey Release ${TAG} (from ${CURRENT_VER}) ==="

if [ "${NEW_VER}" != "${CURRENT_VER}" ]; then
    echo "Bumping version across workspace manifests to ${NEW_VER}..."
    MODIFIED_FILES=1
    sed -i "0,/^version = \".*\"/s//version = \"${NEW_VER}\"/" "${ROOT}/Cargo.toml"
    sed -i "s/^project(fcitx5-smartkey VERSION [^ ]* LANGUAGES CXX)/project(fcitx5-smartkey VERSION ${NEW_VER} LANGUAGES CXX)/" "${ROOT}/adapters/fcitx5-smartkey/CMakeLists.txt"
    sed -i "s/^ \* SmartKey C\/C++ FFI Header (v[^)]*)/ * SmartKey C\/C++ FFI Header (v${NEW_VER})/" "${ROOT}/crates/smartkey-ffi/include/smartkey.h"
    sed -i -e "s/^pkgver=.*/pkgver=${NEW_VER}/" -e "s/^pkgrel=.*/pkgrel=1/" "${ROOT}/packaging/arch/PKGBUILD"

    "${SCRIPT_DIR}/check-versions.sh"
    cargo metadata --format-version 1 --manifest-path "${ROOT}/Cargo.toml" >/dev/null
else
    "${SCRIPT_DIR}/check-versions.sh"
fi

echo "Running pre-release verification gates (clippy + corpus eval)..."
cargo clippy --manifest-path "${ROOT}/Cargo.toml" --workspace --all-targets -- -D warnings
cargo run --release --locked --manifest-path "${ROOT}/Cargo.toml" -p smartkey-cli --bin smartkey-dev -- eval

if [ "${NEW_VER}" != "${CURRENT_VER}" ]; then
    git -C "${ROOT}" add -- \
        Cargo.toml \
        Cargo.lock \
        adapters/fcitx5-smartkey/CMakeLists.txt \
        crates/smartkey-ffi/include/smartkey.h \
        packaging/arch/PKGBUILD
    git -C "${ROOT}" commit -m "chore(release): bump version to ${NEW_VER}"
    CREATED_COMMIT=1
fi

git -C "${ROOT}" tag -a "${TAG}" -m "${RELEASE_TITLE}"
CREATED_TAG=1
touch "${ROOT}/crates/smartkey-cli/build.rs"

echo "Building portable & Arch release packages into ${DIST_DIR}..."
rm -f "${DIST_DIR}/${PKGNAME}-${NEW_VER}-"*
"${SCRIPT_DIR}/package-portable.sh"
"${SCRIPT_DIR}/build-arch-pkg.sh"

ASSETS=(
    "${SCRIPT_DIR}/install.sh"
    "${DIST_DIR}/${PORTABLE_ASSET}"
    "${DIST_DIR}/${PORTABLE_ASSET}.sha256"
    "${DIST_DIR}/${ARCH_ASSET}"
    "${DIST_DIR}/${ARCH_ASSET}.sha256"
)
for asset in "${ASSETS[@]}"; do
    [ -f "${asset}" ] || {
        echo "error: expected release asset not found: ${asset}" >&2
        exit 1
    }
done

(
    cd "${DIST_DIR}"
    sha256sum -c "${PORTABLE_ASSET}.sha256" "${ARCH_ASSET}.sha256"
)

if [ "${NO_LOCAL_INSTALL}" -eq 0 ]; then
    echo "Syncing local installation..."
    "${ROOT}/adapters/fcitx5-smartkey/scripts/install-local.sh"
fi

PUSH_STARTED=1
echo "Pushing main and tag ${TAG} to origin..."
git -C "${ROOT}" push origin main "${TAG}"

echo "Creating GitHub release ${TAG}..."
gh release create "${TAG}" "${ASSETS[@]}" \
    --title "${RELEASE_TITLE}" \
    --notes-file "${NOTES_FILE}"

echo ""
echo "Released ${RELEASE_TITLE} successfully."
