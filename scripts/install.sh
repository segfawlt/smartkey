#!/usr/bin/env bash
# Unified Linux installer, updater, uninstaller, and status inspector for SmartKey.
# Auto-detects Arch Linux (pacman) vs. portable ~/.local (~/.local/lib/fcitx5),
# checks SHA-256 checksums, detects dual-install conflicts, and handles both
# interactive terminal runs (including `curl ... | bash`) and --yes automation.
set -euo pipefail

REPO="${SMARTKEY_REPO:-segfawlt/smartkey}"
PKGNAME="fcitx5-smartkey"
PKG_VER_MIN_GLIBC="2.36"

BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
LIB_DIR="${HOME}/.local/lib/fcitx5"
CONF_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/fcitx5/addon"
DICT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/smartkey/dicts"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/smartkey"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/smartkey"
CACHEDIR="${XDG_CACHE_HOME:-$HOME/.cache}/smartkey-pkg"

ACTION=""
METHOD="auto"
OPT_FROM_SOURCE=0
OPT_VERSION=""
ASSUME_YES=0
FORCE=0
DRY_RUN=0
NO_RESTART=0
OPT_PURGE=0
OPT_NO_COLOR=0

usage() {
    cat <<'EOF'
USAGE:
  install.sh [install|update|uninstall|status] [OPTIONS]

Unified Linux lifecycle manager for fcitx5-smartkey.
When no command is given, inspects your current installation and guides you
through installing, updating, reinstalling, or uninstalling.

COMMANDS:
  install                     Install the latest (or --version) release
  update                      Update an existing installation to the latest release
  uninstall                   Remove SmartKey (preserves ~/.config/smartkey by default)
  status                      Print system, install, and latest-release summary

OPTIONS:
  --method <auto|pacman|portable>
                              Choose Arch pacman (/usr) or portable (~/.local)
                              (default: auto)
  --from-source               Build & install from local git checkout via
                              adapters/fcitx5-smartkey/scripts/install-local.sh
  --version <vX.Y.Z|X.Y.Z>    Install a specific release tag instead of latest
  --uninstall                 Alias for the `uninstall` command
  -y, --yes                   Non-interactive mode; accept defaults without prompting
  -f, --force                 Reinstall even if already up to date; ignore glibc floor warning
  --dry-run                   Download and verify SHA-256 only; make no system changes
  --no-restart                Do not restart Fcitx5 after install or uninstall
  --purge                     With uninstall: also remove ~/.config/smartkey and state
  --no-color                  Disable ANSI colors and spinner animations
  -h, --help                  Show this help message

EXAMPLES:
  curl -fsSL https://github.com/segfawlt/smartkey/releases/latest/download/install.sh | bash
  ./scripts/install.sh status
  ./scripts/install.sh --method portable --yes
  ./scripts/install.sh uninstall --purge
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        install|update|uninstall|status)
            if [ -n "${ACTION}" ]; then
                echo "error: multiple commands specified ('${ACTION}' and '$1')" >&2
                exit 2
            fi
            ACTION="$1"
            ;;
        --uninstall)
            if [ -n "${ACTION}" ] && [ "${ACTION}" != "uninstall" ]; then
                echo "error: multiple commands specified ('${ACTION}' and '$1')" >&2
                exit 2
            fi
            ACTION="uninstall"
            ;;
        --status)
            if [ -n "${ACTION}" ] && [ "${ACTION}" != "status" ]; then
                echo "error: multiple commands specified ('${ACTION}' and '$1')" >&2
                exit 2
            fi
            ACTION="status"
            ;;
        --update)
            if [ -n "${ACTION}" ] && [ "${ACTION}" != "update" ]; then
                echo "error: multiple commands specified ('${ACTION}' and '$1')" >&2
                exit 2
            fi
            ACTION="update"
            ;;
        --method)
            shift
            [ $# -gt 0 ] || { echo "error: --method requires a value (auto|pacman|portable)" >&2; exit 2; }
            METHOD="$1"
            ;;
        --method=*)
            METHOD="${1#*=}"
            ;;
        --from-source)
            OPT_FROM_SOURCE=1
            ;;
        --version)
            shift
            [ $# -gt 0 ] || { echo "error: --version requires a value (e.g. v0.4.0)" >&2; exit 2; }
            OPT_VERSION="$1"
            ;;
        --version=*)
            OPT_VERSION="${1#*=}"
            ;;
        -y|--yes)
            ASSUME_YES=1
            ;;
        -f|--force)
            FORCE=1
            ;;
        --dry-run)
            DRY_RUN=1
            ;;
        --no-restart)
            NO_RESTART=1
            ;;
        --purge)
            OPT_PURGE=1
            ;;
        --no-color)
            OPT_NO_COLOR=1
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option or argument: $1" >&2
            echo "Run with --help for usage." >&2
            exit 2
            ;;
    esac
    shift
done

case "${METHOD}" in
    auto|pacman|portable) ;;
    *)
        echo "error: invalid --method '${METHOD}' (expected auto, pacman, or portable)" >&2
        exit 2
        ;;
esac

# --- Source delegation (--from-source) --------------------------------------

if [ "${OPT_FROM_SOURCE}" -eq 1 ]; then
    LOCAL_CANDIDATES=()
    if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
        SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
        LOCAL_CANDIDATES+=("${SCRIPT_DIR}/../adapters/fcitx5-smartkey/scripts/install-local.sh")
    fi
    LOCAL_CANDIDATES+=("${PWD}/adapters/fcitx5-smartkey/scripts/install-local.sh")

    for cand in "${LOCAL_CANDIDATES[@]}"; do
        if [ -f "${cand}" ]; then
            exec bash "${cand}"
        fi
    done

    echo "error: local SmartKey checkout not found from current directory." >&2
    echo "Clone and build from source with:" >&2
    echo "  git clone https://github.com/${REPO}.git" >&2
    echo "  cd smartkey && ./adapters/fcitx5-smartkey/scripts/install-local.sh" >&2
    exit 1
fi

# --- Terminal capabilities, colors, spinner & cleanup -----------------------

HAS_TTY=0
if [ -t 1 ] && [ -z "${CI:-}" ] && ( : < /dev/tty ) 2>/dev/null; then
    HAS_TTY=1
fi

USE_COLOR=0
USE_ANIM=0
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ -z "${CI:-}" ] && [ "${OPT_NO_COLOR}" -eq 0 ]; then
    USE_COLOR=1
    USE_ANIM=1
fi

if [ "${USE_COLOR}" -eq 1 ]; then
    C_RESET=$'\033[0m'
    C_BOLD=$'\033[1m'
    C_DIM=$'\033[2m'
    C_RED=$'\033[31m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_BLUE=$'\033[34m'
    C_CYAN=$'\033[36m'
else
    C_RESET=""
    C_BOLD=""
    C_DIM=""
    C_RED=""
    C_GREEN=""
    C_YELLOW=""
    C_BLUE=""
    C_CYAN=""
fi

SPINNER_PID=""
TMP_DIR="$(mktemp -d)"

stop_spinner() {
    if [ -n "${SPINNER_PID:-}" ]; then
        kill "${SPINNER_PID}" 2>/dev/null || true
        wait "${SPINNER_PID}" 2>/dev/null || true
        SPINNER_PID=""
        if [ "${USE_ANIM}" -eq 1 ]; then
            printf '\r\033[2K\033[?25h'
        fi
    fi
}

cleanup() {
    stop_spinner
    if [ "${USE_ANIM:-0}" -eq 1 ]; then
        printf '\033[?25h' 2>/dev/null || true
    fi
    if [ -n "${TMP_DIR:-}" ] && [ -d "${TMP_DIR}" ]; then
        rm -rf "${TMP_DIR}"
    fi
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

start_spinner() {
    local msg="$1"
    stop_spinner
    if [ "${USE_ANIM}" -ne 1 ]; then
        printf '  %s...\n' "${msg}"
        return 0
    fi
    printf '\033[?25l'
    (
        local frames=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
        local i=0
        while true; do
            printf '\r  %s%s%s %s' "${C_CYAN}" "${frames[i]}" "${C_RESET}" "${msg}"
            i=$(( (i + 1) % 10 ))
            sleep 0.08
        done
    ) &
    SPINNER_PID=$!
}

info() { printf '%s==>%s %s\n' "${C_CYAN}${C_BOLD}" "${C_RESET}" "$*"; }
ok()   { printf '  %s✓%s %s\n' "${C_GREEN}" "${C_RESET}" "$*"; }
warn() { printf '  %s!%s %s\n' "${C_YELLOW}${C_BOLD}" "${C_RESET}" "$*" >&2; }
err()  { printf '  %s✗%s %s\n' "${C_RED}${C_BOLD}" "${C_RESET}" "$*" >&2; }

prompt_tty() {
    local prompt_text="$1"
    local reply_var="$2"
    local default_val="${3:-}"
    local input=""
    if [ "${HAS_TTY}" -ne 1 ]; then
        printf -v "${reply_var}" '%s' "${default_val}"
        return 0
    fi
    printf '%s' "${prompt_text}" > /dev/tty
    IFS= read -r input < /dev/tty || input="${default_val}"
    if [ -z "${input}" ]; then
        input="${default_val}"
    fi
    printf -v "${reply_var}" '%s' "${input}"
}

format_bytes() {
    local bytes="${1:-0}"
    awk -v b="${bytes}" 'BEGIN {
        if (b >= 1048576) printf "%.1f MB", b / 1048576
        else if (b >= 1024) printf "%.1f KB", b / 1024
        else printf "%d B", b
    }'
}

glibc_ge() {
    local actual="$1"
    local req="$2"
    local a_maj a_min r_maj r_min
    a_maj="${actual%%.*}"
    a_min="${actual#*.}"
    a_min="${a_min%%.*}"
    r_maj="${req%%.*}"
    r_min="${req#*.}"
    r_min="${r_min%%.*}"
    [ $(( 10#${a_maj} * 1000 + 10#${a_min} )) -ge $(( 10#${r_maj} * 1000 + 10#${r_min} )) ]
}

# --- System & installation discovery ---------------------------------------

HOST_ARCH="$(uname -m)"
case "${HOST_ARCH}" in
    x86_64|amd64) ASSET_ARCH="x86_64" ;;
    aarch64|arm64) ASSET_ARCH="aarch64" ;;
    *)
        err "Unsupported architecture: ${HOST_ARCH} (supported: x86_64, aarch64)"
        exit 1
        ;;
esac

DISTRO_NAME="Linux"
if [ -r /etc/os-release ]; then
    DISTRO_NAME="$(sed -n 's/^PRETTY_NAME="\?\([^"]*\)"\?/\1/p' /etc/os-release | head -n1)"
    [ -n "${DISTRO_NAME}" ] || DISTRO_NAME="Linux"
fi

LIBC_LABEL="unknown"
GLIBC_VER=""
if command -v ldd >/dev/null 2>&1; then
    LDD_LINE="$(ldd --version 2>&1 | head -n1 || true)"
    if printf '%s' "${LDD_LINE}" | grep -qi 'musl'; then
        LIBC_LABEL="musl"
    else
        GLIBC_VER="$(printf '%s' "${LDD_LINE}" | grep -oE '[0-9]+\.[0-9]+' | tail -n1 || true)"
        if [ -n "${GLIBC_VER}" ]; then
            LIBC_LABEL="glibc ${GLIBC_VER}"
        fi
    fi
fi

HAS_PACMAN=0
if command -v pacman >/dev/null 2>&1; then
    HAS_PACMAN=1
fi

FCITX_STATE="not found"
FCITX_RUNNING=0
if command -v fcitx5 >/dev/null 2>&1 || command -v fcitx5-remote >/dev/null 2>&1; then
    if command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
        FCITX_RUNNING=1
        FCITX_STATE="running"
    else
        FCITX_STATE="installed (not running)"
    fi
fi

PACMAN_VER=""
PACMAN_SEMVER=""
if [ "${HAS_PACMAN}" -eq 1 ]; then
    PACMAN_VER="$(pacman -Q "${PKGNAME}" 2>/dev/null | awk '{print $2}' || true)"
    PACMAN_SEMVER="${PACMAN_VER%%-*}"
fi

LOCAL_INSTALLED=0
LOCAL_VER=""
if [ -f "${LIB_DIR}/libsmartkey.so" ] || [ -x "${BIN_DIR}/smartkey" ]; then
    LOCAL_INSTALLED=1
    if [ -x "${BIN_DIR}/smartkey" ]; then
        LOCAL_VER="$("${BIN_DIR}/smartkey" --version 2>/dev/null | awk '{print $2}' || true)"
    fi
    [ -n "${LOCAL_VER}" ] || LOCAL_VER="unknown"
fi

DUAL_INSTALL=0
if [ -n "${PACMAN_VER}" ] && [ "${LOCAL_INSTALLED}" -eq 1 ]; then
    DUAL_INSTALL=1
fi

INSTALLED_METHOD=""
CUR_SEMVER=""
INSTALLED_SUMMARY="not installed"
if [ "${DUAL_INSTALL}" -eq 1 ]; then
    INSTALLED_METHOD="pacman"
    CUR_SEMVER="${PACMAN_SEMVER}"
    INSTALLED_SUMMARY="pacman ${PACMAN_VER} + ~/.local ${LOCAL_VER} (CONFLICT)"
elif [ -n "${PACMAN_VER}" ]; then
    INSTALLED_METHOD="pacman"
    CUR_SEMVER="${PACMAN_SEMVER}"
    INSTALLED_SUMMARY="${PACMAN_VER} (pacman -> /usr)"
elif [ "${LOCAL_INSTALLED}" -eq 1 ]; then
    INSTALLED_METHOD="portable"
    CUR_SEMVER="${LOCAL_VER}"
    INSTALLED_SUMMARY="${LOCAL_VER} (local -> ~/.local)"
fi

# --- GitHub release metadata & parser ---------------------------------------

RELEASE_JSON="${TMP_DIR}/release.json"
TAG=""
LATEST_VER=""
RELEASE_TITLE=""
PUBLISHED_AT=""
PACMAN_URL=""
PACMAN_SIZE=0
PACMAN_FILE=""
PORTABLE_URL=""
PORTABLE_SIZE=0
PORTABLE_FILE=""

parse_release_meta() {
    local json_file="$1"
    if command -v jq >/dev/null 2>&1; then
        jq -r '.tag_name // "", .name // "", ((.published_at // "") | split("T")[0])' "${json_file}"
    elif command -v python3 >/dev/null 2>&1; then
        python3 - "${json_file}" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as fh:
    d = json.load(fh)
print(d.get("tag_name") or "")
print(d.get("name") or "")
print((d.get("published_at") or "").split("T")[0])
PY
    else
        awk '
            /"assets"[[:space:]]*:/ { in_assets = 1 }
            !in_assets && tag == "" && /"tag_name"[[:space:]]*:/ {
                v = $0; sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, "", v); sub(/".*/, "", v); tag = v
            }
            !in_assets && rname == "" && /"name"[[:space:]]*:/ {
                v = $0; sub(/.*"name"[[:space:]]*:[[:space:]]*"/, "", v); sub(/".*/, "", v); rname = v
            }
            !in_assets && pub == "" && /"published_at"[[:space:]]*:/ {
                v = $0; sub(/.*"published_at"[[:space:]]*:[[:space:]]*"/, "", v); sub(/T.*/, "", v); pub = v
            }
            END {
                printf "%s\n%s\n%s\n", tag, rname, pub
            }
        ' "${json_file}"
    fi
}

parse_release_asset() {
    local json_file="$1"
    local suffix="$2"
    if command -v jq >/dev/null 2>&1; then
        jq -r --arg suf "${suffix}" '
            (.assets // [])[]
            | select(.name | endswith($suf))
            | select(.name | endswith(".sha256") | not)
            | "\(.browser_download_url)\t\(.size // 0)\t\(.name)"
        ' "${json_file}" | head -n1
    elif command -v python3 >/dev/null 2>&1; then
        python3 - "${json_file}" "${suffix}" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as fh:
    d = json.load(fh)
suf = sys.argv[2]
for a in d.get("assets") or []:
    name = a.get("name") or ""
    if name.endswith(suf) and not name.endswith(".sha256"):
        print(f"{a.get('browser_download_url') or ''}\t{a.get('size') or 0}\t{name}")
        break
PY
    else
        awk -v suf="${suffix}" '
            /"assets"[[:space:]]*:/ { in_assets = 1; next }
            in_assets {
                if ($0 ~ /"url"[[:space:]]*:[[:space:]]*"https:\/\/api\.github\.com\/repos\/.*\/releases\/assets\//) {
                    cur_name = ""; cur_size = 0; cur_url = ""
                } else if ($0 ~ /"name"[[:space:]]*:/) {
                    v = $0; sub(/.*"name"[[:space:]]*:[[:space:]]*"/, "", v); sub(/".*/, "", v)
                    cur_name = v
                } else if ($0 ~ /"size"[[:space:]]*:/) {
                    v = $0; sub(/.*"size"[[:space:]]*:[[:space:]]*/, "", v); sub(/[^0-9].*/, "", v)
                    cur_size = (v == "" ? 0 : v)
                } else if ($0 ~ /"browser_download_url"[[:space:]]*:/) {
                    v = $0; sub(/.*"browser_download_url"[[:space:]]*:[[:space:]]*"/, "", v); sub(/".*/, "", v)
                    cur_url = v
                }
                if (cur_name != "" && cur_url != "" &&
                    length(cur_name) >= length(suf) &&
                    substr(cur_name, length(cur_name) - length(suf) + 1) == suf &&
                    cur_name !~ /\.sha256$/) {
                    printf "%s\t%s\t%s\n", cur_url, cur_size, cur_name
                    exit 0
                }
            }
        ' "${json_file}"
    fi
}

fetch_release_metadata() {
    command -v curl >/dev/null 2>&1 || { err "missing dependency: curl"; exit 1; }

    local api_url
    if [ -n "${OPT_VERSION}" ]; then
        local tag_query="v${OPT_VERSION#v}"
        api_url="https://api.github.com/repos/${REPO}/releases/tags/${tag_query}"
        start_spinner "Querying release ${tag_query} from github.com/${REPO}"
    else
        api_url="https://api.github.com/repos/${REPO}/releases/latest"
        start_spinner "Querying latest release from github.com/${REPO}"
    fi

    if ! curl -fsSL --retry 3 "${api_url}" > "${RELEASE_JSON}" 2>"${TMP_DIR}/curl.err"; then
        stop_spinner
        err "failed to query GitHub release metadata (${api_url})"
        sed 's/^/    /' "${TMP_DIR}/curl.err" >&2 || true
        exit 1
    fi
    stop_spinner

    local meta_lines=()
    while IFS= read -r line; do
        meta_lines+=("${line}")
    done < <(parse_release_meta "${RELEASE_JSON}")

    TAG="${meta_lines[0]:-}"
    RELEASE_TITLE="${meta_lines[1]:-}"
    PUBLISHED_AT="${meta_lines[2]:-}"

    if [ -z "${TAG}" ]; then
        err "could not parse release tag from GitHub API response"
        exit 1
    fi
    LATEST_VER="${TAG#v}"

    local line_pacman line_portable
    line_pacman="$(parse_release_asset "${RELEASE_JSON}" "${ASSET_ARCH}.pkg.tar.zst" || true)"
    line_portable="$(parse_release_asset "${RELEASE_JSON}" "linux-${ASSET_ARCH}.tar.zst" || true)"

    if [ -n "${line_pacman}" ]; then
        IFS=$'\t' read -r PACMAN_URL PACMAN_SIZE PACMAN_FILE <<< "${line_pacman}"
    fi
    if [ -n "${line_portable}" ]; then
        IFS=$'\t' read -r PORTABLE_URL PORTABLE_SIZE PORTABLE_FILE <<< "${line_portable}"
    fi
}

# --- Helpers: banner, summary, Fcitx5 restart, local uninstall -------------

print_header() {
    printf '\n%sSmartKey%s %s— On-device PC autocorrect for Fcitx5%s\n' \
        "${C_BOLD}${C_CYAN}" "${C_RESET}" "${C_DIM}" "${C_RESET}"
    printf '%s──────────────────────────────────────────────────────%s\n' "${C_DIM}" "${C_RESET}"
}

print_summary() {
    printf '  %-11s %s (%s, %s)\n' "System:" "${DISTRO_NAME}" "${HOST_ARCH}" "${LIBC_LABEL}"
    printf '  %-11s %s\n' "Fcitx5:" "${FCITX_STATE}"
    printf '  %-11s %s\n' "Installed:" "${INSTALLED_SUMMARY}"
    if [ -n "${TAG}" ]; then
        local pub_note=""
        [ -n "${PUBLISHED_AT}" ] && pub_note=" (${PUBLISHED_AT})"
        printf '  %-11s %s%s\n' "Latest:" "${TAG}" "${pub_note}"
        local pkg_parts=()
        if [ -n "${PACMAN_FILE}" ]; then
            pkg_parts+=("pacman: ${PACMAN_FILE} ($(format_bytes "${PACMAN_SIZE}"))")
        fi
        if [ -n "${PORTABLE_FILE}" ]; then
            pkg_parts+=("portable: ${PORTABLE_FILE} ($(format_bytes "${PORTABLE_SIZE}"))")
        fi
        if [ "${#pkg_parts[@]}" -gt 0 ]; then
            printf '  %-11s %s\n' "Packages:" "${pkg_parts[0]}"
            if [ "${#pkg_parts[@]}" -gt 1 ]; then
                printf '  %-11s %s\n' "" "${pkg_parts[1]}"
            fi
        fi
    fi
    printf '\n'
}

restart_fcitx5() {
    if [ "${NO_RESTART}" -eq 1 ]; then
        ok "Skipped Fcitx5 restart (--no-restart)."
        return 0
    fi
    if command -v busctl >/dev/null 2>&1 && command -v fcitx5-remote >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
        start_spinner "Restarting running Fcitx5 daemon via DBus"
        busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
        local ready=0
        for _ in $(seq 1 30); do
            if fcitx5-remote --check >/dev/null 2>&1; then
                ready=1
                break
            fi
            sleep 0.1
        done
        stop_spinner
        if [ "${ready}" -eq 1 ]; then
            ok "Restarted Fcitx5 daemon in-place."
        else
            warn "Sent Fcitx5 restart; daemon is still starting up."
        fi
    else
        info "Restart Fcitx5 in your desktop session to apply: fcitx5 -r -d"
    fi
}

remove_local_files() {
    rm -f "${BIN_DIR}/smartkey" "${BIN_DIR}/smartkeyctl"
    rm -f "${LIB_DIR}/libsmartkey.so"
    rm -f "${CONF_DIR}/smartkey.conf"
    rm -f "${DICT_DIR}/en.skdict" "${DICT_DIR}/vi.skdict"
    rmdir "${DICT_DIR}" 2>/dev/null || true
    if [ -d "${CACHEDIR}" ]; then
        rm -f "${CACHEDIR}/${PKGNAME}"-*.pkg.tar.zst "${CACHEDIR}/${PKGNAME}"-*.pkg.tar.zst.sha256
    fi
}

remove_pacman_pkg() {
    local pac_flags=(-R "${PKGNAME}")
    if [ "${ASSUME_YES}" -eq 1 ] || [ "${HAS_TTY}" -ne 1 ]; then
        sudo pacman "${pac_flags[@]}" --noconfirm
    else
        sudo pacman "${pac_flags[@]}" < /dev/tty
    fi
}

run_installed_status() {
    local bin=""
    if [ "${LOCAL_INSTALLED}" -eq 1 ] && [ -x "${BIN_DIR}/smartkey" ]; then
        bin="${BIN_DIR}/smartkey"
    elif [ -n "${PACMAN_VER}" ] && [ -x "/usr/bin/smartkey" ]; then
        bin="/usr/bin/smartkey"
    elif command -v smartkey >/dev/null 2>&1; then
        bin="$(command -v smartkey)"
    fi
    if [ -n "${bin}" ]; then
        info "Live status (${bin}):"
        "${bin}" status || true
    fi
}

print_im_env_hint() {
    local session="${XDG_SESSION_TYPE:-}"
    case "${session}" in
        wayland)
            printf '    Wayland: launch `fcitx5 -d` via autostart, set XMODIFIERS=@im=fcitx, and leave GTK_IM_MODULE unset.\n'
            ;;
        x11)
            printf '    X11:     set GTK_IM_MODULE=fcitx, QT_IM_MODULE=fcitx, XMODIFIERS=@im=fcitx in ~/.xprofile or /etc/environment.\n'
            ;;
        *)
            printf '    Wayland: launch `fcitx5 -d` via autostart, set XMODIFIERS=@im=fcitx, leave GTK_IM_MODULE unset.\n'
            printf '    X11:     set GTK_IM_MODULE=fcitx, QT_IM_MODULE=fcitx, XMODIFIERS=@im=fcitx in ~/.xprofile or /etc/environment.\n'
            ;;
    esac
}

print_fcitx5_guidance() {
    [ "${FCITX_RUNNING:-0}" -eq 1 ] && return 0

    if [ "${FCITX_STATE}" = "not found" ]; then
        local install_cmd=""
        if command -v pacman >/dev/null 2>&1; then
            install_cmd="sudo pacman -S fcitx5"
        elif command -v apt-get >/dev/null 2>&1; then
            install_cmd="sudo apt install fcitx5"
        elif command -v dnf >/dev/null 2>&1; then
            install_cmd="sudo dnf install fcitx5"
        elif command -v zypper >/dev/null 2>&1; then
            install_cmd="sudo zypper install fcitx5"
        fi
        warn "Fcitx5 (>= 5.0.20) is not installed yet; SmartKey needs Fcitx5 to run:"
        if [ -n "${install_cmd}" ]; then
            printf '    Install: %s\n' "${install_cmd}"
        else
            printf '    Install Fcitx5 (>= 5.0.20) via your distribution package manager.\n'
        fi
        print_im_env_hint
    else
        warn "Fcitx5 is installed but not running; start it with: fcitx5 -d"
        case "${XMODIFIERS:-}" in
            *@im=fcitx*) ;;
            *) print_im_env_hint ;;
        esac
    fi
}

# --- Conflict check & resolution --------------------------------------------

handle_dual_install_conflict() {
    [ "${DUAL_INSTALL}" -eq 1 ] || return 0
    [ "${ACTION}" != "uninstall" ] || return 0
    [ "${ACTION}" != "status" ] || {
        warn "Dual install detected: ${LIB_DIR}/libsmartkey.so shadows /usr/lib/fcitx5/libsmartkey.so."
        return 0
    }

    warn "Dual install detected:"
    printf '    %s~/.local/lib/fcitx5/libsmartkey.so%s (%s) shadows %s/usr/lib/fcitx5/libsmartkey.so%s (%s).\n' \
        "${C_BOLD}" "${C_RESET}" "${LOCAL_VER}" "${C_BOLD}" "${C_RESET}" "${PACMAN_VER}" >&2

    if [ "${DRY_RUN}" -eq 1 ] || [ "${ASSUME_YES}" -eq 1 ] || [ "${HAS_TTY}" -ne 1 ]; then
        warn "Continuing non-interactively; remove the duplicate copy after verifying."
        return 0
    fi

    printf '  Resolve duplicate install before proceeding:\n'
    printf '    [1] Keep pacman (/usr) & remove ~/.local shadow %s(recommended)%s\n' "${C_DIM}" "${C_RESET}"
    printf '    [2] Keep ~/.local & remove pacman package\n'
    printf '    [3] Continue without cleanup\n'
    printf '    [q] Quit\n'
    local choice=""
    prompt_tty "  Choice [1]: " choice "1"
    case "${choice}" in
        1)
            remove_local_files
            LOCAL_INSTALLED=0
            LOCAL_VER=""
            DUAL_INSTALL=0
            INSTALLED_METHOD="pacman"
            CUR_SEMVER="${PACMAN_SEMVER}"
            ok "Removed ~/.local shadow copy."
            ;;
        2)
            remove_pacman_pkg
            PACMAN_VER=""
            PACMAN_SEMVER=""
            DUAL_INSTALL=0
            INSTALLED_METHOD="portable"
            CUR_SEMVER="${LOCAL_VER}"
            ok "Removed pacman package."
            ;;
        3)
            warn "Keeping both installs."
            ;;
        q|Q)
            info "Aborted."
            exit 0
            ;;
        *)
            err "Invalid choice: ${choice}"
            exit 2
            ;;
    esac
    printf '\n'
}

# --- Uninstall flow ---------------------------------------------------------

do_uninstall() {
    if [ -z "${PACMAN_VER}" ] && [ "${LOCAL_INSTALLED}" -ne 1 ] && [ ! -f "${CONF_DIR}/smartkey.conf" ]; then
        info "SmartKey is not installed."
        exit 0
    fi

    if [ "${DRY_RUN}" -eq 1 ]; then
        info "Dry run — would remove:"
        [ -n "${PACMAN_VER}" ] && printf '  - pacman package: %s (%s)\n' "${PKGNAME}" "${PACMAN_VER}"
        [ "${LOCAL_INSTALLED}" -eq 1 ] && printf '  - user-local files in %s, %s, %s, %s\n' "${BIN_DIR}" "${LIB_DIR}" "${CONF_DIR}" "${DICT_DIR}"
        [ "${OPT_PURGE}" -eq 1 ] && printf '  - personal config & state: %s, %s\n' "${CONFIG_DIR}" "${STATE_DIR}"
        exit 0
    fi

    if [ "${ASSUME_YES}" -ne 1 ] && [ "${HAS_TTY}" -eq 1 ]; then
        local ans=""
        prompt_tty "  Remove SmartKey (${INSTALLED_SUMMARY})? [y/N] " ans "n"
        case "${ans}" in
            y|Y|yes|YES) ;;
            *)
                info "Uninstall cancelled."
                exit 0
                ;;
        esac
    fi

    if [ -n "${PACMAN_VER}" ]; then
        info "Removing pacman package ${PKGNAME}..."
        remove_pacman_pkg
        ok "Removed ${PKGNAME} via pacman."
    fi

    if [ "${LOCAL_INSTALLED}" -eq 1 ] || [ -f "${CONF_DIR}/smartkey.conf" ]; then
        info "Removing user-local SmartKey files..."
        remove_local_files
        ok "Removed SmartKey files from ~/.local."
    fi

    restart_fcitx5

    if [ -d "${CONFIG_DIR}" ] || [ -d "${STATE_DIR}" ]; then
        if [ "${OPT_PURGE}" -eq 1 ]; then
            rm -rf "${CONFIG_DIR}" "${STATE_DIR}"
            ok "Purged personal config and state (${CONFIG_DIR}, ${STATE_DIR})."
        elif [ "${ASSUME_YES}" -ne 1 ] && [ "${HAS_TTY}" -eq 1 ]; then
            local purge_ans=""
            prompt_tty "  Also remove personal config & learned rules (${CONFIG_DIR})? [y/N] " purge_ans "n"
            case "${purge_ans}" in
                y|Y|yes|YES)
                    rm -rf "${CONFIG_DIR}" "${STATE_DIR}"
                    ok "Removed personal config and state."
                    ;;
                *)
                    ok "Kept personal config and learned rules in ${CONFIG_DIR}."
                    ;;
            esac
        else
            info "Kept personal config and learned rules in ${CONFIG_DIR} (use --purge to remove)."
        fi
    fi
}

# --- Install / Update execution ---------------------------------------------

resolve_method() {
    if [ "${METHOD}" != "auto" ]; then
        if [ "${METHOD}" = "pacman" ] && [ "${HAS_PACMAN}" -ne 1 ]; then
            err "--method pacman requested, but 'pacman' is not installed on this system."
            exit 1
        fi
        return 0
    fi

    if [ -n "${INSTALLED_METHOD}" ]; then
        METHOD="${INSTALLED_METHOD}"
        return 0
    fi

    if [ "${HAS_PACMAN}" -eq 1 ]; then
        if [ "${HAS_TTY}" -eq 1 ] && [ "${ASSUME_YES}" -ne 1 ] && [ -n "${PORTABLE_URL}" ] && [ -n "${PACMAN_URL}" ]; then
            printf '  Choose install method:\n'
            printf '    [1] Arch pacman (/usr, tracked by pacman, %s) %s[default]%s\n' "$(format_bytes "${PACMAN_SIZE}")" "${C_DIM}" "${C_RESET}"
            printf '    [2] Portable (~/.local, no root needed, %s)\n' "$(format_bytes "${PORTABLE_SIZE}")"
            local m_choice=""
            prompt_tty "  Method [1]: " m_choice "1"
            case "${m_choice}" in
                1) METHOD="pacman" ;;
                2) METHOD="portable" ;;
                *) err "Invalid choice: ${m_choice}"; exit 2 ;;
            esac
        else
            METHOD="pacman"
        fi
    else
        METHOD="portable"
    fi
}

download_file() {
    local url="$1"
    local out_path="$2"
    local show_bar="${3:-0}"
    if [ "${show_bar}" -eq 1 ] && [ "${USE_ANIM}" -eq 1 ]; then
        curl --fail --location --retry 3 --progress-bar -o "${out_path}" "${url}"
    else
        curl -fsSL --retry 3 -o "${out_path}" "${url}"
    fi
}

install_via_pacman() {
    for cmd in curl sha256sum pacman sudo; do
        command -v "${cmd}" >/dev/null 2>&1 || { err "missing dependency: ${cmd}"; exit 1; }
    done
    if [ -z "${PACMAN_URL}" ]; then
        err "release ${TAG} has no ${ASSET_ARCH} .pkg.tar.zst asset"
        err "check: https://github.com/${REPO}/releases/tag/${TAG}"
        exit 1
    fi

    local size_human
    size_human="$(format_bytes "${PACMAN_SIZE}")"
    mkdir -p "${CACHEDIR}"

    info "[1/4] Downloading ${PACMAN_FILE} (${size_human})..."
    rm -f "${CACHEDIR}/${PKGNAME}"-*.pkg.tar.zst "${CACHEDIR}/${PKGNAME}"-*.pkg.tar.zst.sha256
    download_file "${PACMAN_URL}" "${CACHEDIR}/${PACMAN_FILE}" 1
    download_file "${PACMAN_URL}.sha256" "${CACHEDIR}/${PACMAN_FILE}.sha256" 0

    info "[2/4] Verifying SHA-256 checksum..."
    local sum_prefix
    (
        cd "${CACHEDIR}"
        sha256sum -c "${PACMAN_FILE}.sha256" >/dev/null
    )
    sum_prefix="$(awk '{print substr($1, 1, 12)}' "${CACHEDIR}/${PACMAN_FILE}.sha256")"
    ok "Verified SHA-256 (${sum_prefix}...)"

    if [ "${DRY_RUN}" -eq 1 ]; then
        info "Dry run complete: verified ${CACHEDIR}/${PACMAN_FILE}; skipping pacman."
        exit 0
    fi

    info "[3/4] Installing ${PACMAN_FILE} with pacman..."
    local pac_args=(-U)
    [ "${FORCE}" -eq 1 ] || pac_args+=(--needed)
    if [ "${ASSUME_YES}" -eq 1 ] || [ "${HAS_TTY}" -ne 1 ]; then
        sudo pacman "${pac_args[@]}" --noconfirm "${CACHEDIR}/${PACMAN_FILE}"
    else
        sudo pacman "${pac_args[@]}" "${CACHEDIR}/${PACMAN_FILE}" < /dev/tty
    fi
    ok "Installed $(pacman -Q "${PKGNAME}" 2>/dev/null || echo "${PKGNAME} ${LATEST_VER}")."

    info "[4/4] Reloading Fcitx5 & verifying..."
    restart_fcitx5
    PACMAN_VER="$(pacman -Q "${PKGNAME}" 2>/dev/null | awk '{print $2}' || true)"
    run_installed_status
    print_fcitx5_guidance
}

install_via_portable() {
    for cmd in curl sha256sum zstd tar; do
        command -v "${cmd}" >/dev/null 2>&1 || { err "missing dependency: ${cmd}"; exit 1; }
    done
    if [ "${LIBC_LABEL}" = "musl" ]; then
        err "portable binary requires glibc >= ${PKG_VER_MIN_GLIBC}; musl is not supported (use --from-source)."
        exit 1
    fi
    if [ -n "${GLIBC_VER}" ] && ! glibc_ge "${GLIBC_VER}" "${PKG_VER_MIN_GLIBC}"; then
        if [ "${FORCE}" -ne 1 ]; then
            err "portable binary requires glibc >= ${PKG_VER_MIN_GLIBC} (detected ${GLIBC_VER})."
            err "Build from source instead (--from-source), or pass --force to try anyway."
            exit 1
        fi
        warn "glibc ${GLIBC_VER} is older than ${PKG_VER_MIN_GLIBC}; continuing due to --force."
    fi
    if [ -z "${PORTABLE_URL}" ]; then
        err "release ${TAG} has no linux-${ASSET_ARCH}.tar.zst asset"
        err "check: https://github.com/${REPO}/releases/tag/${TAG}"
        exit 1
    fi

    local size_human
    size_human="$(format_bytes "${PORTABLE_SIZE}")"
    local dl_dir="${TMP_DIR}/dl"
    local extract_dir="${TMP_DIR}/extract"
    mkdir -p "${dl_dir}" "${extract_dir}"

    info "[1/4] Downloading ${PORTABLE_FILE} (${size_human})..."
    download_file "${PORTABLE_URL}" "${dl_dir}/${PORTABLE_FILE}" 1
    download_file "${PORTABLE_URL}.sha256" "${dl_dir}/${PORTABLE_FILE}.sha256" 0

    info "[2/4] Verifying SHA-256 checksum..."
    local sum_prefix
    (
        cd "${dl_dir}"
        sha256sum -c "${PORTABLE_FILE}.sha256" >/dev/null
    )
    sum_prefix="$(awk '{print substr($1, 1, 12)}' "${dl_dir}/${PORTABLE_FILE}.sha256")"
    ok "Verified SHA-256 (${sum_prefix}...)"

    if [ "${DRY_RUN}" -eq 1 ]; then
        info "Dry run complete: verified ${PORTABLE_FILE}; skipping install."
        exit 0
    fi

    info "[3/4] Extracting and installing to ~/.local..."
    zstd -d -c "${dl_dir}/${PORTABLE_FILE}" | tar -xf - -C "${extract_dir}"
    local stage_dir
    stage_dir="$(find "${extract_dir}" -mindepth 1 -maxdepth 1 -type d | head -n1)"
    if [ -z "${stage_dir}" ] || [ ! -f "${stage_dir}/bin/smartkey" ] || [ ! -f "${stage_dir}/lib/fcitx5/libsmartkey.so" ]; then
        err "unexpected archive layout inside ${PORTABLE_FILE}"
        exit 1
    fi

    mkdir -p "${BIN_DIR}" "${LIB_DIR}" "${CONF_DIR}" "${DICT_DIR}"
    install -m 755 "${stage_dir}/bin/smartkey" "${BIN_DIR}/smartkey"
    ln -sf smartkey "${BIN_DIR}/smartkeyctl"
    install -m 644 "${stage_dir}/share/smartkey/dicts/en.skdict" "${DICT_DIR}/en.skdict"
    install -m 644 "${stage_dir}/share/smartkey/dicts/vi.skdict" "${DICT_DIR}/vi.skdict"
    install -m 755 "${stage_dir}/lib/fcitx5/libsmartkey.so" "${LIB_DIR}/libsmartkey.so"

    if [ -f "${HOME}/.cargo/bin/smartkey" ]; then
        rm -f "${HOME}/.cargo/bin/smartkey"
    fi

    cat > "${CONF_DIR}/smartkey.conf" <<CONF_EOF
[Addon]
Name=SmartKey Autocorrect
Comment=On-device PC English autocorrect, backspace learner, and fcitx5-lotus Telex bridge
Type=SharedLibrary
Library=${LIB_DIR}/libsmartkey
Category=Module
Version=${LATEST_VER}
OnDemand=False
Configurable=True

[Addon/Dependencies]
0=core:5.0.20

[Addon/OptionalDependencies]
0=notifications
CONF_EOF

    ok "Binary: ${BIN_DIR}/smartkey (aliased as smartkeyctl)"
    ok "Addon:  ${LIB_DIR}/libsmartkey.so"
    ok "Dicts:  ${DICT_DIR}/{en,vi}.skdict"
    ok "Config: ${CONF_DIR}/smartkey.conf"

    info "[4/4] Reloading Fcitx5 & verifying..."
    restart_fcitx5
    LOCAL_INSTALLED=1
    run_installed_status
    print_fcitx5_guidance

    case ":${PATH}:" in
        *":${BIN_DIR}:"*) ;;
        *)
            warn "${BIN_DIR} is not on your PATH; add it to run \`smartkey\` directly:"
            printf '    export PATH="%s:$PATH"\n' "${BIN_DIR}"
            ;;
    esac
}

# --- Main dispatch ----------------------------------------------------------

print_header

if [ "${ACTION}" = "uninstall" ]; then
    print_summary
    do_uninstall
    exit 0
fi

fetch_release_metadata
PKG_FILE="${PACMAN_FILE}"

print_summary
handle_dual_install_conflict

if [ "${ACTION}" = "status" ]; then
    run_installed_status
    print_fcitx5_guidance
    exit 0
fi

# Determine action when omitted
if [ -z "${ACTION}" ]; then
    if [ -z "${CUR_SEMVER}" ]; then
        ACTION="install"
    elif [ "${CUR_SEMVER}" = "${LATEST_VER}" ]; then
        if [ "${FORCE}" -eq 1 ]; then
            ACTION="update"
        elif [ "${HAS_TTY}" -eq 1 ] && [ "${ASSUME_YES}" -ne 1 ]; then
            ok "Already up to date (${TAG})."
            local_choice=""
            prompt_tty "  Action: [r]einstall / [u]ninstall / [s]tatus / [q]uit [q]: " local_choice "q"
            case "${local_choice}" in
                r|R) FORCE=1; ACTION="update" ;;
                u|U) do_uninstall; exit 0 ;;
                s|S) run_installed_status; exit 0 ;;
                q|Q) exit 0 ;;
                *) err "Invalid choice: ${local_choice}"; exit 2 ;;
            esac
        else
            ok "Already up to date (${TAG}). Use --force to reinstall."
            exit 0
        fi
    else
        ACTION="update"
        if [ "${HAS_TTY}" -eq 1 ] && [ "${ASSUME_YES}" -ne 1 ]; then
            info "Update available: ${CUR_SEMVER} -> ${LATEST_VER}"
            upd_choice=""
            prompt_tty "  Action: [U]pdate / [r]emove / [q]uit [U]: " upd_choice "U"
            case "${upd_choice}" in
                u|U) ;;
                r|R) do_uninstall; exit 0 ;;
                q|Q) exit 0 ;;
                *) err "Invalid choice: ${upd_choice}"; exit 2 ;;
            esac
        fi
    fi
elif [ "${ACTION}" = "update" ] && [ "${CUR_SEMVER}" = "${LATEST_VER}" ] && [ "${FORCE}" -ne 1 ]; then
    ok "Already up to date (${TAG}). Use --force to reinstall."
    exit 0
fi

resolve_method

if [ "${ASSUME_YES}" -ne 1 ] && [ "${HAS_TTY}" -eq 1 ] && [ "${ACTION}" = "install" ]; then
    local_size_bytes=0
    [ "${METHOD}" = "pacman" ] && local_size_bytes="${PACMAN_SIZE}" || local_size_bytes="${PORTABLE_SIZE}"
    confirm=""
    prompt_tty "  Install SmartKey ${TAG} via ${METHOD} ($(format_bytes "${local_size_bytes}"))? [Y/n] " confirm "y"
    case "${confirm}" in
        y|Y|yes|YES) ;;
        *)
            info "Cancelled."
            exit 0
            ;;
    esac
fi

if [ "${METHOD}" = "pacman" ]; then
    install_via_pacman
else
    install_via_portable
fi
