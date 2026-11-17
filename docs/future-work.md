# SmartKey — Future Work & Backlog

Consolidated roadmap of open engineering work after `v0.2.0` (core engine, `fcitx5-smartkey` Wayland/Lotus bridge, and `.skdict` v3 multi-language packs).

---

## 1. Stress Testing & Host Simulation (Current Polishing Phase)

- **Long-horizon bilingual session soak test (Rust):**
  - Replay thousands of realistic English + Vietnamese Telex + accidental unswitched-Telex sentences across multiple simulated sessions (`rules.toml` + `learner_state.json` persistence, reloads, typos, `BackSpace` / `Ctrl+BackSpace` undos, and manual fixes).
  - Assert invariants over time: zero poisoning of `personal_dict` (no ordinary prose `TitleCase`, no known typos, no `>= 4`-char raw Telex syllables), `[corrections]`, and `blocklist` / `staged_undo_keep`.
- **Mock-`InputContext` Wayland/Fcitx5 timing harness (C++):**
  - Simulate Fcitx5 `InputContextReset`, delayed `SurroundingTextUpdated` echoes, and `LotusWlrVirtualKeyboard` replacement bursts against `SmartKeyAddon` (`adapters/fcitx5-smartkey/src/smartkey-addon.cpp`), extending `addon_load_smoke.cpp` beyond header-only tracker tests.

---

## 2. Modular Dictionaries & Pack Distribution

- **Layered domain overlays in `LexiconRegistry`:** Support loading multiple packs/overlays per language (e.g., `en-core.skdict` + `en-dev.skdict`) instead of taking only the first `<lang>.skdict` found.
- **Standalone `.skdict` releases (CalVer data drops):** Publish prebuilt packs on GitHub releases with independent pack versioning (`PackHeader::pack_version` / CalVer release tags) and end-to-end `smartkey dict install/update` verification.
- **Optional `Norvig count_2w.txt` replacement:** Find a clearly licensed bigram corpus to replace the `UNVERIFIED` `count_2w.txt` source without losing the missed-space recall (`95/95` vs `92/95`).

---

## 3. UI & Configuration Experience

- **Proper desktop UI:** Build a graphical configuration, rule-review, and diagnostics interface (editing `personal_dict`, `[corrections]`, `blocklist`, `telex_passthrough`, inspecting `events-*.jsonl`, and toggling presets/features without needing CLI commands).

---

## 4. Packaging, CI & Cross-Platform (`fix-plan.md` Batch 4 & `architecture-and-plan.md` Milestone 5)

- **Packaging & CI (`docs/fix-plan.md` Batch 4):**
  - Build portable bundles against an older `glibc` baseline (or musl-static CLI) and document Fcitx5 ABI compatibility for prebuilt `libsmartkey.so`.
  - Add a CI matrix (Debian 12 `glibc` floor, `aarch64`, `cargo test --workspace`, `ctest`).
  - Document desktop IM environment preconditions (`XMODIFIERS`, Wayland `GTK_IM_MODULE`) in `README.md`.
- **Android (`docs/architecture-and-plan.md` Milestone 5):** `heliboard-smartkey` Android JNI fork (`cargo-ndk`, `--no-default-features`).
