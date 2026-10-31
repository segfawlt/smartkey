# SmartKey

Privacy-first, local-only autocorrect and typing assistant that **learns deterministically from your own edits and undone corrections** — built around a portable pure-Rust core (`smartkey-core`) targeting **`fcitx5-lotus`** (seamless Vietnamese + English & pure English on Linux), standalone English Linux adapters (`fcitx5` Module / GNOME `IBus`), **Android (`HeliBoard`)**, and **Windows (`TSF`)**.

## Documentation

- **[Architecture & Implementation Plan (v2 Hardened Spec)](./docs/architecture-and-plan.md)** — Zero-copy `u64` static DAWG + Leipzig bigram noisy-channel decoder, situational disambiguation (`"it os"` → `"it is"` vs `"arch os"`), Guards 1–3, hardened Undo/Edit state machine, `rules.toml` v2 schema, and `fcitx5-lotus` integration plan.
- **[Competitor & Ecosystem Analysis](./docs/competitor-analysis.md)** — Detailed feature matrix and architectural post-mortem of Pilcrow, EkaKey, Gboard, HeliBoard, FUTO, FlorisBoard, `quan-autocorrect`, `SmartType`, `ibus-typing-booster`, macOS Autocorrect, and `fcitx5-bamboo` / `fcitx5-lotus`.
