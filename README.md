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

## Quick Start

```bash
# Run golden corpus evaluation (must_fix.tsv + must_not_touch.txt + learning_traces)
cargo test
cargo run -p smartkey-cli -- eval

# Inspect how SmartKey scores and explains a token
cargo run -p smartkey-cli -- explain "hstiautoin"
cargo run -p smartkey-cli -- explain --prev "over" "ther"

# Launch interactive terminal typing + backspace-learning sandbox
cargo run -p smartkey-cli -- repl
```

## License

Licensed under the [GNU General Public License v3.0 or later](./LICENSE) (`GPL-3.0-or-later`).
