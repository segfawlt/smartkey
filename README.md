# SmartKey

Privacy-first, 100% on-device **PC-first autocorrect and deterministic backspace-learning engine**, written in pure Rust (`GPL-3.0-or-later`).

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
- **`crates/smartkey-cli`** — Control CLI (`smartkeyctl`: `status`, `toggle`, `reload`, `log`, `rules`, `explain`, `init`, `repl`) and developer corpus tools (`smartkey-dev`: `eval`, `build-lexicon`).
- **`adapters/fcitx5-smartkey`** — Standalone C++20 `fcitx5` module addon (`Category=Module`, zero `sudo` user-local install to `~/.local`) that runs English autocorrect on `keyboard-*` layouts while stepping aside automatically when `Alt+Z` switches to `fcitx5-bamboo` or `fcitx5-lotus`.

## Installation & Packaging

### 1. Arch Linux (`PKGBUILD` / `.pkg.tar.zst`)

Build a native Arch Linux package from source:

```bash
./scripts/build-arch-pkg.sh
sudo pacman -U dist/fcitx5-smartkey-*.pkg.tar.zst
```

This installs `/usr/bin/smartkeyctl`, `/usr/lib/fcitx5/libsmartkey.so`, and `/usr/share/fcitx5/addon/smartkey.conf`.

### 2. Portable Linux Archive (`.tar.zst`)

Build a standalone portable bundle for any Linux distribution (`x86_64`):

```bash
./scripts/package-portable.sh
```

To install on any system without requiring `sudo` or build dependencies:

```bash
tar --zstd -xf dist/fcitx5-smartkey-0.1.0-linux-x86_64.tar.zst
cd fcitx5-smartkey-0.1.0-linux-x86_64
./install.sh
# To uninstall: ./uninstall.sh
```

### 3. From Source (User-Local Install)

Build and install directly into `~/.local` without root privileges:

```bash
./adapters/fcitx5-smartkey/scripts/install-local.sh
# To uninstall: ./adapters/fcitx5-smartkey/scripts/uninstall-local.sh
```

## Quick Start & CLI Usage

```bash
# Run full test suite & corpus evaluation
cargo test --workspace
cargo run --release -p smartkey-cli --bin smartkey-dev -- eval

# Query status and inspect running Fcitx5 addon
smartkeyctl status

# Toggle SmartKey autocorrect on/off (ideal for WM keybindings)
smartkeyctl toggle

# Reload rules.toml in running Fcitx5 daemon
smartkeyctl reload

# Inspect live autocorrects, 1x/2x/3x+ backspace undos, and manual fixes
smartkeyctl log
smartkeyctl rules

# Inspect how SmartKey scores and ranks candidates for a token
smartkeyctl explain "implementintion"
smartkeyctl explain --prev "over" "ther"

# Initialize or reconfigure rules.toml with preset
smartkeyctl init

# Launch interactive terminal typing + live backspace-learning sandbox
smartkeyctl repl
```

### App Context Profiles (`fcitx5-smartkey`)

- **Bypass (`100%` hands-off):** Password/PIN/URL/email/numeric fields (`CapabilityFlags`), terminals (`kitty`, `alacritty`, `foot`, `ghostty`, `wezterm`, `konsole`, `gnome-terminal`, `xterm`, etc.), and code editors/IDEs (`code`, `codium`, `zed`, `nvim`, `vim`, `emacs`, `helix`, `jetbrains-*`).
- **Active (`keyboard-*` only):** Browsers (`firefox`, `zen`, `chromium`, `brave`), chat apps (`discord`, `vesktop`, `telegram`, `slack`, `signal`), and general prose/text editors (`gnome-text-editor`, `kate`, `kwrite`, `gedit`, `obsidian`, `libreoffice`). Switching to Vietnamese (`bamboo` / `lotus`) via `Alt+Z` steps SmartKey aside automatically.

## License

Licensed under the [GNU General Public License v3.0 or later](./LICENSE) (`GPL-3.0-or-later`).
