# SmartKey

Privacy-first, local-only autocorrect and typing assistant that **learns deterministically from your own edits and undone corrections** — across Linux (`fcitx5` / `fcitx5-lotus`), Android (`HeliBoard`), and Windows.

## Documentation

- **[Architecture & Implementation Plan (Approach #3)](./docs/architecture-and-plan.md)** — Full technical specification: AOSP `.combined` noisy-channel decoder, guarded Undo/Edit learning state machine, anti-poisoning shields, `rules.toml` schema, FFI boundary, `fcitx5` integration, and 5-milestone build roadmap.
- **[Competitor & Ecosystem Analysis](./docs/competitor-analysis.md)** — Detailed feature matrix and architectural post-mortem of Pilcrow, EkaKey, Gboard, HeliBoard, FUTO, FlorisBoard, `quan-autocorrect`, `SmartType`, `ibus-typing-booster`, macOS Autocorrect, and `fcitx5-bamboo` / `fcitx5-lotus`.
