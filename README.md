# SmartKey

On-device, privacy-first autocorrect for physical PC keyboards — no cloud, no telemetry, no machine learning.

SmartKey fixes everyday typing slips (transpositions, doubled letters, pinky misses, Shift timing, clipped spaces) as you type, steps out of terminals, code editors, and password fields automatically, and learns from a single BackSpace in a plain-text config file you own.

> [!WARNING]
> **Alpha software.** Expect rough edges across desktop environments, distros, and edge-case apps:
> - Unexpected interactions in some applications.
> - Password fields are not shielded on legacy X11 XIM apps (Wayland and native GTK/Qt apps are fine).
> - Breaking changes to the `rules.toml` format.

## Supported platforms

| OS | Backend | Architectures | Status |
|---|---|---|---|
| **Linux** (Wayland & X11) | [Fcitx5](https://github.com/fcitx/fcitx5) (`>= 5.0.20`) | `x86_64`, `aarch64` (source) | Primary |
| **Windows 10 / 11** | Windows Text Services Framework (TSF) | `x86_64` | Alpha ([Windows guide](./packaging/windows/README.md)) |

**Languages:** English (`mixed`, `en_US`, `en_GB`) and Vietnamese Telex rescue (`vi-lotus`) are built in; experimental Spanish (`es`) and French (`fr`) packs are available via `smartkey-dev build-pack`.

## Install

### Linux

```bash
curl -fsSL https://github.com/segfawlt/smartkey/releases/latest/download/install.sh | bash
```

Requires **Fcitx5 (`>= 5.0.20`)**. Auto-detects Arch (`pacman`) or portable (`~/.local`, `glibc >= 2.36`, no root), verifies SHA-256, and handles install, update, uninstall, and status.

<details>
<summary>Fcitx5 setup, manual packages &amp; build from source</summary>

#### 1. Fcitx5 session setup

Install Fcitx5 from your distribution (`sudo pacman -S fcitx5`, `sudo apt install fcitx5`, or `sudo dnf install fcitx5`), then route your session through it:

- **Wayland:** launch `fcitx5 -d` via autostart, set `XMODIFIERS=@im=fcitx`, and leave `GTK_IM_MODULE` unset so GTK4 apps use native Wayland text input.
- **X11:** set `GTK_IM_MODULE=fcitx`, `QT_IM_MODULE=fcitx`, `XMODIFIERS=@im=fcitx` in `~/.xprofile` or `/etc/environment`.

Run `smartkey status` after installing to verify the daemon, addon, and IM routing.

#### 2. Manual prebuilt install (Arch `x86_64`)

```bash
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst.sha256
sudo pacman -U ./fcitx5-smartkey-0.4.0-1-x86_64.pkg.tar.zst
fcitx5 -r -d
smartkey status
```

#### 3. Manual prebuilt install (Generic Linux `x86_64`, `glibc >= 2.36`)

Installs to `~/.local` with no root (Debian 12+, Ubuntu 24.04+, Fedora 39+, openSUSE):

```bash
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst
curl -LO https://github.com/segfawlt/smartkey/releases/latest/download/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst.sha256
sha256sum -c fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst.sha256
zstd -d -c fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst | tar -xf -
cd fcitx5-smartkey-0.4.0-linux-x86_64
./install.sh
smartkey status
```

On older distros (e.g. Ubuntu 22.04 with glibc 2.35) or `aarch64`, build from source.

#### 4. Build from source

Dependencies: Rust stable toolchain, `cmake >= 3.20`, and Fcitx5 dev headers (`fcitx5` on Arch, `libfcitx5core-dev libfcitx5utils-dev` on Debian/Ubuntu, `fcitx5-devel` on Fedora).

```bash
git clone https://github.com/segfawlt/smartkey.git
cd smartkey
./adapters/fcitx5-smartkey/scripts/install-local.sh   # or: ./scripts/install.sh --from-source
smartkey status
```

Other build targets:
- **Arch package:** `./scripts/build-arch-pkg.sh`
- **Portable tarball:** `./scripts/package-portable.sh --container` (`dist/fcitx5-smartkey-0.4.0-linux-x86_64.tar.zst`)
- **Tests & corpus eval:** `cargo test --workspace` and `cargo run --release -p smartkey-cli --bin smartkey-dev -- eval`

</details>

### Windows

Download `smartkey-windows-x86_64.zip` from [GitHub Releases](https://github.com/segfawlt/smartkey/releases/latest), extract it, and run `.\install.ps1` in PowerShell (or `.\install.ps1 -User` without Administrator), then press `Win+Space` to pick **SmartKey Autocorrect**. See [packaging/windows/README.md](./packaging/windows/README.md) for details.

## How it works

SmartKey runs a pure-Rust engine inside your OS input method (`fcitx5` on Linux, TSF COM DLL on Windows):

1. **Autocorrect engine (Layer 1).** When you finish a word, a zero-copy `.skdict` lexicon checks it against real words and ~96,000 curated keyboard typos using Damerau–Levenshtein automata and a character-multiset rescue pass (~1 µs median, < 1 ms p99). Terminals, IDEs, URLs, inline code, mixed-case identifiers (`useState`, `iOS`), and ALL-CAPS tokens are shielded by design.
2. **Deterministic learning (Layer 2).** One `BackSpace` within 5 s reverts an unwanted correction; fixing the same typo twice promotes it to your personal dictionary/rules in `rules.toml` (inspectable, editable, Git-friendly).
3. **Bilingual Vietnamese bridge.** Detects Vietnamese input (`fcitx5-lotus` on Linux; UniKey/EVKey/OpenKey coexistence on Windows) to rescue broken Telex sequences (`nuowcss` → `nước`) without interfering with English prose.

Includes an interactive TUI & control CLI (`smartkey status | toggle | rules | app | log | explain | repl`). Full reference: [docs/usage.md](./docs/usage.md).

## Documentation

- [Usage & CLI reference](./docs/usage.md) — commands, app profiles, behavior boundaries, IM environment detail.
- [Architecture & implementation plan](./docs/architecture-and-plan.md) — scorer waterfall, learning loop, pack format.
- [Competitor & ecosystem analysis](./docs/competitor-analysis.md).

## License

Licensed under the [GNU General Public License v3.0 or later](./LICENSE) (`GPL-3.0-or-later`).
