# SmartKey

> [!WARNING]
> **Experimental Testing / Beta Proof of Concept**
> 
> SmartKey is currently in an early **testing beta / proof-of-concept (PoC)** phase. It has **not yet been widely tested across varied desktop environments, distros, or edge-case workflows**. Expect rough edges, unexpected interactions in certain applications, or changes to internal configuration formats as feedback comes in.
>
> If you are trying this out, please treat it as an experimental preview. Feedback, bug reports, and typo edge cases are very welcome via [GitHub Issues](https://github.com/segfawlt/smartkey/issues)!

---

## What is SmartKey?

SmartKey brings **phone-grade, privacy-first, 100% on-device autocorrect to physical PC keyboards** on Linux (via Fcitx5).

Unlike touchscreens, physical desktop keyboards suffer from distinct typing errors: physical key chatter, 10-finger rolling transpositions (`teh`, `chagnes`), pinky slips on punctuation (`don;t`, `situatioN:`), and thumb-spacebar collisions (`inbthe`). Most existing desktop spellcheckers either show slow red squiggly popups or blindly overwrite code, URLs, and variable names.

SmartKey is designed from the ground up for desktop environments:
1. **Sub-20 µs Latency & 0% False Positives on Code:** Powered by pure Rust with an embedded compressed finite-state transducer (FST), 96,000+ real-world keyboard smashes, and character-multiset rescue. It automatically steps aside in code editors, terminals, passwords, and URLs.
2. **Deterministic Learning Through Your Backspace:** It learns from what you actually do without machine learning or cloud telemetry:
   - **1x Backspace:** Immediately reverts an unwanted suggestion back to what you literally typed.
   - **2x Fix:** Fixing the same typo twice promotes your fix permanently to your personal dictionary and learned rules.
   - **Plain-text Brain:** All learned preferences live in a human-readable, comment-preserving `~/.config/smartkey/rules.toml` that you can inspect, edit, or sync with Git.
3. **Seamless Bilingual Coexistence (`vi-lotus` Bridge):** Runs as a lightweight Fcitx5 module addon on standard English layouts (`keyboard-us`), includes an optional zero-fork **`vi-lotus`** bridge for `fcitx5-lotus` (rescuing broken Telex smashes like `nuowcss → nước` and shielding English prose momentum like `type of → type of`), and steps aside cleanly on `fcitx5-bamboo` or when `fcitx5-lotus` is in `Off`/macro mode.

---

## Highlights

- **Layer 1 — Phone-Grade PC Autocorrect (`< 20 µs`):**
  - `95,924` curated real-world keyboard smashes (`crate-ci/typos`) + compressed `fst` lexicon + Damerau–Levenshtein automata (`levenshtein-automata`) + `POPCNT` character-multiset smash rescue (`hstiautoin → situation`).
  - Tuned for physical 10-finger keyboards: transpositions (`chagnes → changes`, `soemtimes → sometimes`), switch bounces (`thhe → the`, `nnext → next`), pinky `;` contractions (`don;t → don't`, `doesnt → doesn't`, `im → I'm`), Shift/CapsLock slips (`THat's → That's`, `situatioN: → situation:`, `tHIS → This`), and thumb-spacebar clips (`inbthe → in the`).
  - **Respects user casing & developer tokens:** Never nag-capitalizes lowercase brands (`github` stays `github`), never touches mid-sentence capitals (`Apple`, `GitHub`), and shields CLI/Linux/code tokens, URLs, and expressive stretches (`soooo`, `trờiiii`).
  - **3-way English dialect support:** `mixed` (default — accepts both US & UK words, fixes typos to US), `en_US`, or `en_GB`.
- **Layer 2 — Deterministic Learning From User Corrections:**
  - **`1x` Backspace Undo:** Snaps an unwanted autocorrect right back to your verbatim word (immediately shielding it in-session; `Undo + Edit` blocks `(typed, rejected)` permanently on `1x`, `Undo + Keep` promotes at `2x`, and `Self-Confirm Cancel` ignores accidental backspaces).
  - **`2x` Guarded Manual-Edit Learning:** Backspacing and fixing a typo twice promotes it to `[corrections]` (or `[context_corrections]` for real-word pairs like `"sent form" → "sent from"`), guarded by `Delta-Span Check` (`XXYYY → XXZZZZ` mind-change rejection) and Valid-Word Shields.
  - **`3x` Untouched Vocabulary Promotion:** Repeatedly used slang or project names (`khum`, `promptcord`) graduate cleanly to `personal_dict`.
  - **Plain-Text Source of Truth:** Human-editable, comment-preserving `~/.config/smartkey/rules.toml` (`toml_edit`), portable across Linux (`fcitx5-lotus`) and Android (`HeliBoard` JNI).

## Documentation

- [Architecture & Implementation Plan (v3.2)](./docs/architecture-and-plan.md)
- [Competitor & Ecosystem Analysis](./docs/competitor-analysis.md)

## Workspace Crates & Adapters

- **`crates/smartkey-core`** — Pure-Rust Layer 1 scorer, `SmartKeySession` shadow-buffer learner, diagnostic event logger, and comment-preserving `toml_edit` `RuleStore` (zero OS/C-sys/`$HOME` dependencies).
- **`crates/smartkey-ffi`** — Panic-safe `extern "C"` ABI (`include/smartkey.h`) for desktop input method modules (`fcitx5`) and portable word-level JNI hooks for Android `HeliBoard`.
- **`crates/smartkey-cli`** — Control CLI (`smartkey`: `status`, `toggle`, `reload`, `log`, `rules`, `config`, `plugin`, `dict`, `explain`, `init`, `repl`; aliased as `smartkeyctl`) and developer corpus tools (`smartkey-dev`: `eval`, `build-lexicon`).
- **`adapters/fcitx5-smartkey`** — Standalone C++20 `fcitx5` module addon (`Category=Module`, requires `fcitx5 >= 5.0.20`, zero `sudo` user-local install to `~/.local`) that runs English autocorrect on `keyboard-*` layouts, bridges `fcitx5-lotus` (`vi-lotus` plugin), and steps aside automatically on other CJK/Vietnamese input engines.

## Prerequisites & Desktop IM Environment

SmartKey runs inside **Fcitx5 (`>= 5.0.20`)**. Install `fcitx5` from your distribution first (`sudo pacman -S fcitx5`, `sudo apt install fcitx5`, or `sudo dnf install fcitx5`), then ensure your desktop session routes input through Fcitx5:

- **Wayland (Hyprland, Sway, KDE Plasma 6, GNOME Wayland):**
  - Launch `fcitx5 -d` via your compositor/DE virtual-keyboard or autostart config.
  - Set `XMODIFIERS=@im=fcitx` (and `QT_IM_MODULE=fcitx` if needed for XWayland/Qt5 apps).
  - **Leave `GTK_IM_MODULE` unset on Wayland** so GTK4/libadwaita apps use native Wayland `text-input-v3` (surrounding-text replacement) instead of legacy DBus key forwarding.
- **X11:**
  - Set `GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`, and `XMODIFIERS=@im=fcitx` in `~/.xprofile` or `/etc/environment`.

Run `smartkey status` at any time to verify Fcitx5 daemon state, addon library discovery, and IM environment routing.

## Installation

### 1. Download Prebuilt Binaries (No Build Required)

#### Arch Linux (`.pkg.tar.zst`)
```bash
# 1. Download latest release package and checksum
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.1-1-x86_64.pkg.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.1-1-x86_64.pkg.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.1.1-1-x86_64.pkg.tar.zst.sha256

# 2. Install local package
sudo pacman -U ./fcitx5-smartkey-0.1.1-1-x86_64.pkg.tar.zst

# 3. Restart Fcitx5 and check status
fcitx5 -r -d
smartkey status
```

#### Generic Linux (Debian 12+ / Ubuntu 24.04+ / Fedora 39+ / openSUSE)
> **Compatibility floor:** Prebuilt portable bundles target `x86_64` Linux with **`glibc >= 2.36`** and **`fcitx5 >= 5.0.20`** (`libFcitx5Core.so.7`). On older distros (e.g. Ubuntu 22.04 with `glibc 2.35`) or `aarch64`, build from source using Option A below.

```bash
# Download, verify checksum, and unpack portable bundle
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.1-linux-x86_64.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.1-linux-x86_64.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.1.1-linux-x86_64.tar.zst.sha256
zstd -d -c fcitx5-smartkey-0.1.1-linux-x86_64.tar.zst | tar -xf -
cd fcitx5-smartkey-0.1.1-linux-x86_64

# Install to ~/.local (no sudo required)
./install.sh
smartkey status
```

---

### 2. Build From Source

Clone the repository (requires Rust stable toolchain, `cmake >= 3.20`, and Fcitx5 development headers such as `fcitx5` on Arch, `libfcitx5core-dev libfcitx5utils-dev` on Debian/Ubuntu, or `fcitx5-devel` on Fedora):

```bash
git clone https://github.com/segfawlt/smartkey.git
cd smartkey
```

#### Option A: User-Local Install (`~/.local`, no root)
```bash
./adapters/fcitx5-smartkey/scripts/install-local.sh
smartkey status

# To uninstall:
# ./adapters/fcitx5-smartkey/scripts/uninstall-local.sh
```

#### Option B: Build Arch Linux Package (`PKGBUILD`)
```bash
./scripts/build-arch-pkg.sh
sudo pacman -U dist/fcitx5-smartkey-*.pkg.tar.zst
```

#### Option C: Build Portable Release Bundle
```bash
# Host build (or pass --container to build inside Debian 12 for glibc >= 2.36 portability):
./scripts/package-portable.sh --container
# Outputs dist/fcitx5-smartkey-0.1.1-linux-x86_64.tar.zst (+ .sha256)
```

## Quick Start & CLI Usage

`smartkey` is the primary control binary (`smartkeyctl` is installed alongside it as a compatibility symlink):

```bash
# Run full test suite & corpus evaluation
cargo test --workspace
cargo run --release -p smartkey-cli --bin smartkey-dev -- eval

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
smartkey explain --lang vi "nuowcss"

# Launch interactive terminal typing + live backspace-learning sandbox
smartkey repl
```

### App Context Profiles (`fcitx5-smartkey`)

- **Bypass (`100%` hands-off):** Password/PIN/URL/email/numeric fields (`CapabilityFlags`), terminals (`kitty`, `alacritty`, `foot`, `ghostty`, `wezterm`, `konsole`, `gnome-terminal`, `xterm`, etc.), and code editors/IDEs (`code`, `codium`, `cursor`, `windsurf`, `zed`, `sublime_text`, `nvim`, `vim`, `emacs`, `helix`, `jetbrains-*`, `idea`, `clion`, `pycharm`, `rustrover`, etc.).
- **Active (`keyboard-*` + optional `vi-lotus`):** Browsers (`firefox`, `zen`, `librewolf`, `floorp`, `vivaldi`, `opera`, `thorium`, `chromium`, `brave`, `epiphany`, `qutebrowser` — including Flatpak reverse-DNS IDs), chat apps (`discord`, `vesktop`, `telegram`, `slack`, `signal`), and general prose/text editors (`gnome-text-editor`, `kate`, `kwrite`, `gedit`, `obsidian`, `libreoffice`).
- **Custom Overrides (`[app_profiles]` in `rules.toml`):** Override any application name or Flatpak ID (`bypass = ["my-custom-ide"]`, `chat = ["element"]`, `browser = [...]`, `prose = [...]`, or `unknown_app_policy = "bypass"` for strict allowlist-only activation).

## License

Licensed under the [GNU General Public License v3.0 or later](./LICENSE) (`GPL-3.0-or-later`).
