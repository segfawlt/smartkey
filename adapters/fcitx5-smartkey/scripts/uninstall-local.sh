#!/usr/bin/env bash
set -euo pipefail

rm -f "${HOME}/.local/lib/fcitx5/libsmartkey.so"
rm -f "${HOME}/.local/share/fcitx5/addon/smartkey.conf"

echo "Removed fcitx5-smartkey from ~/.local."

if command -v busctl >/dev/null 2>&1 && fcitx5-remote --check >/dev/null 2>&1; then
    busctl --user call org.fcitx.Fcitx5 /controller org.fcitx.Fcitx.Controller1 Restart >/dev/null 2>&1 || true
    echo "Restarted running Fcitx5 daemon in-place via DBus."
else
    echo "Restart Fcitx5 in your desktop session to unload: fcitx5 -r -d"
fi
