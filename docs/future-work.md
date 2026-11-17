# SmartKey — Future Work & Backlog

Consolidated roadmap of open engineering work after `v0.2.0` (core engine, `fcitx5-smartkey` Wayland/Lotus bridge, and `.skdict` v3 multi-language packs).

---

## 1. Stress Testing & Host Simulation (Current Polishing Phase)

Detailed design & invariants: [Stress Simulator & Host Protocol Fuzzing Plan](./stress-simulator-plan.md).

- **Tier A — Stateful bilingual session soak simulator (Rust, build 1st):**
  - Replay thousands of deterministic seeded sentences (~80% English / ~20% Vietnamese Telex + unswitched-IM bilingual mistakes) across `rules.toml` + `learner_state.json` persistence, reloads, typos, `BackSpace` / `Ctrl+BackSpace` undos, and manual fixes (`crates/smartkey-core/tests/soak_sim.rs` + optional `smartkey-dev sim` CLI).
  - Assert zero poisoning of `personal_dict` (no ordinary prose `TitleCase`, no known typos, no `>= 4`-char raw Telex syllables, no toneless `vi` words), strict `[corrections]` EN-scoping, and zero ghost `staged_undo_keep` shields after `blocklist` edits.
- **Tier B — Deterministic `MockInputContext` Fcitx5 protocol simulator (C++, build 2nd):**
  - Add a thin virtual clock/timer/IM seam to `SmartKeyAddon` (`adapters/fcitx5-smartkey/src/smartkey-addon.{h,cpp}`) and drive an in-process `MockClientContext` (`wayland_v2`, `xcb`, `dbus`) in `adapters/fcitx5-smartkey/tests/` (wired into `ctest`).
  - Phase 1 covers English Wayland/GTK timing races (`15 ms` post-`BackSpace` delayed commit interleaving, `150 ms` `InputContextReset` storms, stale `SurroundingTextUpdated`, GTK4 DBus guard); Phase 2 covers `fcitx5-lotus` `Pause` bursts, `pendingPostBoundaryChars`, and `LotusWlrVirtualKeyboard` keycode `10..21` echoes.
- **Explicit non-goal:** Skip headless Wayland + `/dev/uinput` GUI bot harnesses (low ROI, flaky wall-clock timing, and CI privilege overhead compared to Tier B).

---

## 2. Documentation & Onboarding

- **`README.md` simplification & approachable landing rewrite:**
  - Trim `README.md` into a shorter, welcoming entry point for normal Linux desktop users rather than an internal architecture spec.
  - Lead with a plain-English introduction to physical-keyboard autocorrect and the everyday loop (Space fixes slips → `1x BackSpace` reverts → `2x` repair teaches → automatically hands-off in terminals/code editors).
  - Merge/deduplicate overlapping `What is SmartKey?` and `Highlights` sections; move deep internals (`POPCNT`, `levenshtein-automata`, `SKDICT03`, `toml_edit`, `Delta-Span Check`, and exhaustive 40-app / 25-command listings) into `docs/`.
  - Separate end-user setup (updating stale `0.1.2` links to `scripts/install-release-arch.sh` and `v0.2.0+` bundles + concise Wayland/X11 Fcitx5 env preconditions) from contributor/pack-builder commands (`cargo test`, `smartkey-dev`).

---

## 3. Modular Dictionaries & Pack Distribution

- **Layered domain overlays in `LexiconRegistry`:** Support loading multiple packs/overlays per language (e.g., `en-core.skdict` + `en-dev.skdict`) instead of taking only the first `<lang>.skdict` found.
- **Standalone `.skdict` releases (CalVer data drops):** Publish prebuilt packs on GitHub releases with independent pack versioning (`PackHeader::pack_version` / CalVer release tags) and end-to-end `smartkey dict install/update` verification.
- **Optional `Norvig count_2w.txt` replacement:** Find a clearly licensed bigram corpus to replace the `UNVERIFIED` `count_2w.txt` source without losing the missed-space recall (`95/95` vs `92/95`).

---

## 4. UI & Configuration Experience

- **Proper desktop UI:** Build a graphical configuration, rule-review, and diagnostics interface (editing `personal_dict`, `[corrections]`, `blocklist`, `telex_passthrough`, inspecting `events-*.jsonl`, and toggling presets/features without needing CLI commands).

---

## 5. Packaging, CI & Cross-Platform (`fix-plan.md` Batch 4 & `architecture-and-plan.md` Milestone 5)

- **Packaging & CI (`docs/fix-plan.md` Batch 4):**
  - Build portable bundles against an older `glibc` baseline (or musl-static CLI) and document Fcitx5 ABI compatibility for prebuilt `libsmartkey.so`.
  - Add a CI matrix (Debian 12 `glibc` floor, `aarch64`, `cargo test --workspace`, `ctest`).
  - Keep desktop IM environment preconditions (`XMODIFIERS`, Wayland `GTK_IM_MODULE`) clearly documented in the simplified `README.md` (Section 2).
- **Android (`docs/architecture-and-plan.md` Milestone 5):** `heliboard-smartkey` Android JNI fork (`cargo-ndk`, `--no-default-features`).
