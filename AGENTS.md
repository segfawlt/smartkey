# SmartKey — Project Instructions (`AGENTS.md`)

## Workspace Architecture

- `crates/smartkey-core` — Pure-Rust Layer 1 typo scorer (`LatinScorer` / `PackLexicon`, compressed FST + Damerau–Levenshtein + multiset rescue), `.skdict` v3 `PackBuilder` + `LexiconRegistry`, Layer 2 deterministic shadow-buffer learner (`SmartKeySession`), Vietnamese Telex simulator & typo rescue (`vi_telex` with runtime Lotus dict authority), day-partitioned event logger (`event_log`), and comment-preserving `toml_edit` `RuleStore`.
- `crates/smartkey-ffi` — Panic-safe `extern "C"` ABI (`include/smartkey.h`) statically linked into the Fcitx5 addon and portable to Android `HeliBoard` JNI.
- `crates/smartkey-cli` — User control CLI (`smartkey`, aliased as `smartkeyctl`) and developer pack/evaluation builder (`smartkey-dev`: `eval`, `build-pack`, `build-lexicon`).
- `adapters/fcitx5-smartkey` — C++20 `fcitx5` module addon (`Category=Module`) installed user-locally to `~/.local/lib/fcitx5/libsmartkey.so` and `~/.local/share/fcitx5/addon/smartkey.conf`, routing `keyboard-*` layouts by `InputMethodEntry::languageCode()`.
- `dicts/` — Language pack recipes (`pack.toml`, `rules.toml`, and curated dictionary/slang/shorthand/proper-noun overlays). English & Vietnamese fallback packs embedded (v3) in `crates/smartkey-core/assets/{en_lexicon,vi_lexicon}.skdict`, fully overridable by installed `.skdict` packs; Tier A packs (`es`, `fr`, etc.) built via `smartkey-dev build-pack`. (`data/sources/` is the gitignored upstream fetch cache.)

## Build, Test & Verification Commands

Ensure `$HOME/.cargo/bin` is on `PATH` when invoking `cargo`.

```bash
# 1. Full workspace unit & integration tests (must pass before any commit/deploy)
cargo test --workspace

# 2. Corpus evaluation gate (Layer 1 accuracy & zero false-positive check; supports --lang es|fr and --without-count2w)
cargo run --release -p smartkey-cli --bin smartkey-dev -- eval

# 3. Rebuild embedded English & Vietnamese v3 packs after editing dicts/* recipes/overlays
cargo run --release -p smartkey-cli --bin smartkey-dev -- build-lexicon
# Or build/verify a single Tier A/B pack:
cargo run --release -p smartkey-cli --bin smartkey-dev -- build-pack <lang|all> [--fetch] [--verify]

# 4. Verify version parity across Cargo.toml, CMakeLists.txt, and PKGBUILD
./scripts/check-versions.sh
```

## Automatic Local Deploy After Green Iterations

After completing and verifying a development iteration on `main` (in the primary workspace) that modifies runtime code or pack data (`crates/`, `adapters/`, or `dicts/`), **automatically build, install, and restart the live Fcitx5 addon**:

```bash
./adapters/fcitx5-smartkey/scripts/install-local.sh
~/.local/bin/smartkey status
```

- **Why:** `smartkey reload` only reloads `~/.config/smartkey/rules.toml`. Changes compiled into `smartkey-core`, `smartkey-ffi`, `smartkey-cli`, or `adapters/fcitx5-smartkey` require rebuilding `~/.local/lib/fcitx5/libsmartkey.so` and restarting Fcitx5 in-place over DBus (`org.fcitx.Fcitx.Controller1.Restart`), which `install-local.sh` handles automatically.
- **When to skip auto-deploy:**
  1. Tests are failing or edits are unfinished/unverified WIP.
  2. Working inside an unmerged secondary git worktree (`.worktrees/*`).
  3. Only documentation (`docs/`, `*.md`) changed.

## Domain & Packaging Invariants

- **XDG / Fcitx5 Path Contract:** Keep user-local install paths aligned with Fcitx5 `StandardPaths` and XDG (`~/.local/bin/smartkey`, `~/.local/lib/fcitx5/libsmartkey.so`, `~/.local/share/fcitx5/addon/smartkey.conf`, `~/.local/share/smartkey/dicts/{en,vi}.skdict`, `~/.config/smartkey/rules.toml`, `~/.local/state/smartkey/`).
- **Zero False-Positive Guardrails:** Changes to `crates/smartkey-core/src/scorer.rs`, `casing.rs`, or `vi_telex.rs` must preserve English and Vietnamese valid-word shields (`corpus_eval`, `learning_eval`, `vi_lotus_eval`).
- **Moving the Repository Checkout:** If the repository folder is moved on disk, delete `adapters/fcitx5-smartkey/build/` once before running `install-local.sh` so CMake regenerates its cached absolute source path.
