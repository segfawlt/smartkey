# SmartKey Usage

This page collects the detailed usage reference that the [README](../README.md) summarizes.

## CLI quick start

`smartkey` is the primary control binary (`smartkeyctl` is installed alongside it as a compatibility symlink):

```bash
# Query status, daemon health, IM environment, and personal brain stats
smartkey status

# Toggle SmartKey autocorrect on/off (ideal for WM keybindings)
smartkey toggle

# Manage settings & diagnostics in ~/.config/smartkey/rules.toml
smartkey config list
smartkey config set aggressiveness conservative
smartkey plugin toggle vi-lotus

# Inspect or edit personal words, corrections, snippets, and blocklist pairs
smartkey rules list
smartkey rules add word FastAPI
smartkey rules add fix teh the
smartkey rules add snippet "addr;" "123 Main St"

# Query cumulative counters and durable day-partitioned JSONL event logs
smartkey log
smartkey log --today
smartkey log --since 2h --grep discord
smartkey log --yesterday --json

# Inspect how SmartKey scores and ranks candidates for a token
smartkey explain "implementintion"
smartkey explain --prev "over" "ther"
smartkey explain --lang es "porblema"
smartkey explain --lang vi "nuowcss"

# Inspect or customize per-app classification (`[app_profiles]`)
smartkey app list
smartkey app classify zcode
smartkey app add bypass my-custom-ide
smartkey app remove my-custom-ide

# Launch interactive terminal typing + live backspace-learning sandbox
smartkey repl

# Build or verify modular .skdict v3 packs (Tier A languages like es, fr)
cargo run --release -p smartkey-cli --bin smartkey-dev -- build-pack es --fetch --verify
smartkey dict list -v
smartkey dict install es target/packs/es.skdict

# Run full test suite & corpus evaluation (developer)
cargo test --workspace
cargo run --release -p smartkey-cli --bin smartkey-dev -- eval
```

Status panel widget: see [contrib/waybar/smartkey.jsonc](../contrib/waybar/smartkey.jsonc) (`smartkey status --json`).

## Desktop IM environment

SmartKey runs inside Fcitx5. Install Fcitx5 from your distribution first (`sudo pacman -S fcitx5`, `sudo apt install fcitx5`, or `sudo dnf install fcitx5`), then make sure your desktop session routes input through it:

- **Wayland (Hyprland, Sway, KDE Plasma 6, GNOME Wayland):**
  - Launch `fcitx5 -d` via your compositor/DE virtual-keyboard or autostart config.
  - Set `XMODIFIERS=@im=fcitx` (and `QT_IM_MODULE=fcitx` if needed for XWayland/Qt5 apps).
  - **Leave `GTK_IM_MODULE` unset on Wayland** so GTK4/libadwaita apps use native Wayland `text-input-v3` (surrounding-text replacement) instead of legacy DBus key forwarding.
- **X11:**
  - Set `GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`, and `XMODIFIERS=@im=fcitx` in `~/.xprofile` or `/etc/environment`.

Run `smartkey status` at any time to verify Fcitx5 daemon state, addon library discovery, and IM environment routing.

## App context profiles (`fcitx5-smartkey`)

- **Bypass (100% hands-off):** Password/PIN/URL/email/numeric fields (`CapabilityFlags`), terminals (`kitty`, `alacritty`, `foot`, `ghostty`, `wezterm`, `konsole`, `gnome-terminal`, `xterm`, etc.), and code editors/IDEs (`code`, `codium`, `cursor`, `windsurf`, `zcode`, `zed`, `sublime_text`, `nvim`, `vim`, `emacs`, `helix`, `jetbrains-*`, `idea`, `clion`, `pycharm`, `rustrover`, etc.).
- **Active (`keyboard-*` + optional `vi-lotus`):** Browsers (`firefox`, `zen`, `librewolf`, `floorp`, `vivaldi`, `opera`, `thorium`, `chromium`, `brave`, `epiphany`, `qutebrowser` — including Flatpak reverse-DNS IDs), chat apps (`discord`, `vesktop`, `telegram`, `slack`, `signal`), and general prose/text editors (`gnome-text-editor`, `kate`, `kwrite`, `gedit`, `obsidian`, `libreoffice`).
- **Custom overrides (`[app_profiles]` in `rules.toml` / `smartkey app`):** Override any application name or Flatpak ID via `smartkey app add <bypass|browser|chat|prose> <program_id>`, or set `smartkey config set app_profiles.unknown_app_policy bypass` for strict allowlist-only activation (applies to both empty program IDs and unlisted app names).

## Behavior boundaries & security caveats

- **`Enter` never autocorrects:** Intentional anti-surprise rule so pressing `Enter` in chat or search bars never mutates your final word right before sending.
- **ALL-CAPS stays untouched:** Uppercase tokens (`DONT`, `API`, CapsLock runs) are never rewritten by design.
- **Real words are shielded by default:** Valid-word confusion pairs (`form -> from`, `quiet -> quite`) stay untouched unless you add or 2x-teach a context rule (`[context_corrections]`).
- **`dialect = "mixed"` output bias:** Accepts both US and UK spellings untouched; unknown typos normalize toward US spelling (`smartkey config set dialect en_GB` for British output).
- **X11 / XIM password fields:** Password suppression relies on Fcitx5 `CapabilityFlag::PasswordOrSensitive`, which legacy X11 XIM frontends do not forward. On Wayland `text-input-v3` or native GTK/Qt IM modules it works automatically; on legacy X11 XIM apps, pause with `smartkey toggle` or set `unknown_app_policy = "bypass"`.
