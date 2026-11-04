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
3. **Seamless Bilingual Coexistence:** Runs as a lightweight Fcitx5 module addon on top of standard English layouts (`keyboard-us`), but instantly steps aside whenever you switch to Vietnamese input engines like `fcitx5-bamboo` or `fcitx5-lotus`.

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
- **`crates/smartkey-cli`** — Control CLI (`smartkeyctl`: `status`, `toggle`, `reload`, `log`, `rules`, `explain`, `init`, `repl`) and developer corpus tools (`smartkey-dev`: `eval`, `build-lexicon`).
- **`adapters/fcitx5-smartkey`** — Standalone C++20 `fcitx5` module addon (`Category=Module`, zero `sudo` user-local install to `~/.local`) that runs English autocorrect on `keyboard-*` layouts while stepping aside automatically when `Alt+Z` switches to `fcitx5-bamboo` or `fcitx5-lotus`.

## Installation

### 1. Download Prebuilt Binaries (No Build Required)

#### Arch Linux (`.pkg.tar.zst`)
```bash
# Download latest release package
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.0-1-x86_64.pkg.tar.zst

# Install via pacman
sudo pacman -U fcitx5-smartkey-0.1.0-1-x86_64.pkg.tar.zst

# Restart Fcitx5 and check status
fcitx5 -r -d
smartkeyctl status
```

#### Generic Linux (Ubuntu / Fedora / Debian / SUSE)
```bash
# Download and unpack portable bundle
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.1.0-linux-x86_64.tar.zst
tar --zstd -xf fcitx5-smartkey-0.1.0-linux-x86_64.tar.zst
cd fcitx5-smartkey-0.1.0-linux-x86_64

# Install to ~/.local (no sudo required)
./install.sh
smartkeyctl status
```

---

### 2. Build From Source

Clone the repository and its dependencies:

```bash
git clone https://github.com/segfawlt/smartkey.git
cd smartkey
```

#### Option A: User-Local Install (`~/.local`, no root)
```bash
./adapters/fcitx5-smartkey/scripts/install-local.sh
smartkeyctl status

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
./scripts/package-portable.sh
# Outputs dist/fcitx5-smartkey-0.1.0-linux-x86_64.tar.zst
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
