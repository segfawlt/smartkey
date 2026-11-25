# SmartKey

Phone-grade, privacy-first autocorrect for physical PC keyboards on Linux, running as an [Fcitx5](https://github.com/fcitx/fcitx5) input-method module. All processing is on-device: no cloud, no telemetry, no machine learning.

> [!WARNING]
> **Alpha software.** Not yet widely tested across desktop environments, distros, and edge-case workflows. Expected rough edges:
>
> - Unexpected interactions in some applications.
> - Password fields not shielded on legacy X11 XIM apps (Wayland and native GTK/Qt apps are fine).
> - Breaking changes to the `~/.config/smartkey/rules.toml` format.

## How it works

SmartKey sits between the keyboard and your applications as an Fcitx5 module. Three parts:

1. **Autocorrect engine (Layer 1).** When you finish a word, a pure-Rust scorer checks the finished word against a lexicon of real words plus ~96,000 curated real-world typos, using Damerau-Levenshtein automata and a character-multiset rescue pass. Median decision time is ~1 µs (p99 under 1 ms). Code editors, terminals, and ALL-CAPS tokens are bypassed by design.
2. **Learning (Layer 2).** One BackSpace within 5 s reverts an unwanted correction immediately. Fixing the same typo twice promotes your fix permanently to your personal dictionary and rules. All learned state lives in the plain-text, comment-preserving `~/.config/smartkey/rules.toml`, which you can inspect, edit, and sync with Git.
3. **Bilingual bridge (vi-lotus).** On Vietnamese Telex, an optional `vi-lotus` bridge works alongside `fcitx5-lotus` to rescue broken Telex input (`nuowcss` → `nước`) while leaving English prose untouched.

Deep technical detail lives in the [architecture & implementation plan](./docs/architecture-and-plan.md).

## Features

- Fixes physical-keyboard error classes: transpositions (`chagnes` → `changes`), key chatter (`thhe` → `the`), pinky slips (`don;t` → `don't`), Shift/CapsLock timing slips (`THat's` → `That's`), thumb-spacebar clips (`inbthe` → `in the`).
- Zero false positives on code: bypasses terminals, editors/IDEs, password fields, and URLs, and shields inline code, CLI tokens, and mixed-case identifiers (`useState`, `iOS`).
- Respects casing: lowercase brands stay lowercase (`github` stays `github`), mid-sentence capitals untouched.
- Real-word pairs (`form`/`from`) are shielded by default; teachable via context rules.
- Plain-text config and learned state, portable across Linux and Android (HeliBoard).
- Control CLI: `smartkey status | toggle | rules | app | log | explain | repl`.
- English dialects: `mixed` (default), `en_US`, `en_GB`.

Full command reference and app profiles: [docs/usage.md](./docs/usage.md).

## Language support

| Language | Status | Notes |
|---|---|---|
| English | built in | US + UK dialects |
| Vietnamese | built in | Telex rescue via optional `vi-lotus` bridge |
| Spanish | experimental pack | `smartkey-dev build-pack es --fetch --verify` |
| French | experimental pack | `smartkey-dev build-pack fr --fetch --verify` |

## Install

```bash
curl -fsSL https://github.com/segfawlt/smartkey/releases/latest/download/install.sh | bash
```

Requires Linux and **Fcitx5 (`>= 5.0.20`)**. Auto-detects Arch (`pacman`) or portable (`~/.local`, `glibc >= 2.36`, no root), verifies SHA-256, and handles install, update, uninstall, and status.

<details>
<summary>Fcitx5 setup &amp; manual install</summary>

Install Fcitx5 from your distribution (`sudo pacman -S fcitx5`, `sudo apt install fcitx5`, or `sudo dnf install fcitx5`), then route your session through it:

- **Wayland:** launch `fcitx5 -d` via autostart, set `XMODIFIERS=@im=fcitx`, and leave `GTK_IM_MODULE` unset so GTK4 apps use native Wayland text input.
- **X11:** set `GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`, `XMODIFIERS=@im=fcitx` in `~/.xprofile` or `/etc/environment`.

Run `smartkey status` after installing to verify the daemon, addon, and IM routing.

#### Arch Linux (`x86_64`)

```bash
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst.sha256
sudo pacman -U ./fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst
fcitx5 -r -d
smartkey status
```

#### Generic Linux (`x86_64`, glibc >= 2.36; Debian 12+ / Ubuntu 24.04+ / Fedora 39+ / openSUSE)

Installs to `~/.local` with no root:

```bash
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst.sha256
zstd -d -c fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst | tar -xf -
cd fcitx5-smartkey-0.4.0-linux-x86_64
./install.sh
smartkey status
```

On older distros (e.g. Ubuntu 22.04 with glibc 2.35) or `aarch64`, build from source instead.

</details>

## Build from source

Dependencies: Rust stable toolchain, `cmake >= 3.20`, and Fcitx5 development headers (`fcitx5` on Arch, `libfcitx5core-dev libfcitx5utils-dev` on Debian/Ubuntu, `fcitx5-devel` on Fedora).

```bash
git clone https://github.com/segfawlt/smartkey.git
cd smartkey
./adapters/fcitx5-smartkey/scripts/install-local.sh   # or: ./scripts/install.sh --from-source
smartkey status
# To uninstall: ./scripts/install.sh uninstall (or ./adapters/fcitx5-smartkey/scripts/uninstall-local.sh)
```

Other build targets:

- **Arch package:** `./scripts/build-arch-pkg.sh`, then `sudo pacman -U dist/fcitx5-smartkey-*.pkg.tar.zst`.
- **Portable release bundle:** `./scripts/package-portable.sh --container` (builds inside Debian 12 for glibc >= 2.36 portability), outputs `dist/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst`.
- **Tests & corpus eval:** `cargo test --workspace` and `cargo run --release -p smartkey-cli --bin smartkey-dev -- eval`.

## Documentation

- [Usage & CLI reference](./docs/usage.md) — commands, app profiles, behavior boundaries, IM environment detail.
- [Architecture & implementation plan](./docs/architecture-and-plan.md) — scorer waterfall, learning loop, pack format.
- [Competitor & ecosystem analysis](./docs/competitor-analysis.md).

## License

Licensed under the [GNU General Public License v3.0 or later](./LICENSE) (`GPL-3.0-or-later`).
