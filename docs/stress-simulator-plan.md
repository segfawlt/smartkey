# Stress Simulator & Host Protocol Fuzzing Plan

**Status:** Planned (Backlog)  
**Scope:** Long-horizon bilingual learner soak testing (`crates/smartkey-core`, `crates/smartkey-cli`) and deterministic Fcitx5/Wayland/Lotus host protocol simulation (`adapters/fcitx5-smartkey`).

---

## 1. Motivation & Coverage Gap

Existing test suites already cover single-token scoring and short isolated keystroke traces:

- `crates/smartkey-core/tests/corpus_eval.rs` — single-token accuracy and valid-word shields.
- `crates/smartkey-core/tests/learning_eval.rs` — short golden traces and a 200-case stateless English `proptest` (`1..160` keys on fresh `RuleStore::in_memory`).
- `crates/smartkey-core/tests/vi_lotus_eval.rs` — synchronous idealized Telex/Lotus traces.
- `adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp` — `dlopen` smoke check plus isolated unit assertions on header-only helper structs (`SurroundingSyncTracker`, `SelfReplaceResetGuard`, `LotusBridgeTracker`).

Nearly every post-`v0.1` regression escaped those suites because it lived in one of two untested dimensions:

1. **Multi-session learner & bilingual state drift:** Ghost `staged_undo_keep` shields surviving blocklist edits (`71a91ef`), sentence-start `TitleCase` hijacks and casing downgrades (`7d53585`), English `[corrections]` leaking into `vi` (`4f54c5c`), and forgotten-IM raw Telex syllables polluting English `personal_dict` (`8cab290`).
2. **Asynchronous Fcitx5 / Wayland client & Lotus event ordering in `adapters/fcitx5-smartkey/src/smartkey-addon.cpp`:** Fast keystrokes arriving inside the `15 ms` Wayland post-`BackSpace` commit delay (`4a8db69`), spurious GTK/Gecko `InputContextReset` bursts within `150 ms` of replacement/undo (`8cab290`), lagging `SurroundingTextUpdated` cursor snapshots (`9ab2ef5`), and late `LotusWlrVirtualKeyboard` synthetic keycode `10..21` echoes (`4a8db69`).

---

## 2. Architecture Matrix & Scope Decision

| Tier | Target | Effort | Throughput | ROI | Decision |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Tier A: Stateful Bilingual Session Soak Simulator (Rust)** | `SmartKeySession` + disk-backed `RuleStore` (`rules.toml` + `learner_state.json`) | ~0.5–1 day (~350 LOC) | ~50k+ words/sec | Very High | **Build 1st** |
| **Tier B: Deterministic `InputContext` Protocol Simulator (C++)** | `SmartKeyAddon` (`smartkey-addon.cpp`) + `MockClientContext` (`wayland_v2`, `xcb`, `dbus`) | ~1.5–2 days (~600 LOC + ~30 LOC seam) | ~10k+ events/sec | High | **Build 2nd** |
| **Tier C: Headless Wayland + `/dev/uinput` + Live Daemon Bot** | Compositor + `fcitx5` + `fcitx5-lotus-server` + GUI client | ~1–2 weeks + root/`uinput` | ~10 words/sec | Low / Negative | **Skip** (flaky wall-clock sleeps, non-deterministic races, CI privilege headaches) |

---

## 3. Tier A — Stateful Bilingual Session Soak Simulator (Rust)

### Placement
- Fast CI regression gate in `crates/smartkey-core/tests/soak_sim.rs` (fixed deterministic seeds, e.g. `5,000` words in `< 1 s`).
- Optional deep-fuzz CLI command `smartkey-dev sim --seed <u64> --steps <N>` in `crates/smartkey-cli/src/dev.rs` for multi-million-step local soak runs that emit minimal failing seed/step traces.

### Traffic Mix (English-Primary: ~80% `en` / ~20% `vi`)
English carries the highest autonomous mutation surface (Lev-2, multiset smash rescue, missed-space splits, proper-noun/homograph casing upgrades, and `[corrections]` graduation). Vietnamese traffic focuses on Telex rescue and bilingual boundary hygiene:

- **~65% Normal English prose & dev/chat tokens:** Valid dictionary words, contractions, `TitleCase` sentence starters, `ALL-CAPS` acronyms, slang (`bruh`, `tbh`, `idk`), dev compounds (`textbox`, `FastAPI`, `PyTorch`), URLs/markdown backticks, and curated/synthetic keyboard typos.
- **~15% English edit/undo interactions:**
  - `1x BackSpace` immediate undo → keep verbatim (`Undo + Keep`), edit to a third word (`Undo + Edit`), retype identical correction (`Self-Confirm Cancel`), or hold `BackSpace` past index 0 (`Clause-Erase Cancel`).
  - `Ctrl+BackSpace` / `BackSpace` manual typo repairs (testing `2x` graduation vs `Delta-Span` / `MindChangeDistance` guards).
  - Explicit CLI-equivalent store mutations mid-run (`remove_blocklist_pair`, `add/remove pin`, reload from disk).
- **~10% Bilingual mistake flows (high bug yield):**
  - Typing raw Telex syllables (`chuwa`, `chawcs`, `nguowif`) while still in `en` mode (forgot to switch IM).
  - Typing common English starters/words (`If`, `Of`, `Or`, `textbox`, `github`) while in `vi` (`lotus`) mode.
- **~10% Native Vietnamese (`vi` / Lotus simulation):**
  - Valid toned words, toneless Vietnamese words, split-coda Telex typos (`hoagfn`, `ttieengs`, `duongx`), and `needs_uinput_restore_on_break` boundary commits.
- **Session lifecycle:** Periodically flush, reload `RuleStore` from temporary `rules.toml` + `learner_state.json` on disk, switch `LangId` (`en` ↔ `vi`), and trigger `FocusLost` / `NavOrClick`.

### Hard Invariants Asserted Every Step / Reload
1. **Screen buffer safety:** `delete_chars <= client_buf.chars().count()` on every `Replace`, and resulting UTF-8 buffer matches expected character boundaries.
2. **Zero `personal_dict` poisoning:**
   - Known dictionary typos (e.g. `teh`, `somewher`, `machien`) never enter `personal_dict`.
   - `>= 4`-char raw Telex tokens typed in `en` mode never enter `[lang.en]` / root `personal_dict`.
   - Valid toneless Vietnamese dictionary words never pollute `[lang.vi] personal_dict`.
   - Ordinary lowercase lexicon words typed as sentence-start `TitleCase` never graduate as forced-capital personal overrides.
3. **Strict language scoping:** Top-level `[corrections]` never fires when `session.lang() == "vi"`, and valid standalone lexicon words never graduate to context-free `[corrections]`.
4. **Blocklist & ghost-shield consistency:** Removing a `(typo, fix)` pair from `blocklist` leaves zero residual `staged_undo_keep` shield blocking subsequent corrections.
5. **Round-trip persistence:** Reloading `rules.toml` (`toml_edit`) and `learner_state.json` preserves compiled rule behavior identically.

---

## 4. Tier B — Deterministic Fcitx5 `InputContext` Protocol Simulator (C++)

### Required Refactor Seam in `adapters/fcitx5-smartkey` (~30 LOC)
`SmartKeyAddon` currently reads wall-clock time via `monotonicNowMs()` (`steady_clock::now()`), queries `instance_->inputMethod(ic)`, and schedules the `15 ms` Wayland commit timer directly on `instance_->eventLoop()`. To run `50,000+` timing interleavings in milliseconds without `sleep()`:

1. Inject a lightweight host clock/timer/IM seam (or protected virtual hooks / `AddonHostHooks` struct defaulting to real `steady_clock` + `fcitx::Instance` in production):
   - `uint64_t nowMs()`
   - `scheduleDelayedCommit(uint64_t delayUs, callback)` (driven by a deterministic test priority queue in tests)
   - `activeInputMethod(ic)` / `activeLanguageCode(ic)` override for headless tests without external `liblotus.so`.
2. Subclass `fcitx::InputContext` (`MockClientContext`) implementing its 5 protected virtual methods (`frontend()`, `commitStringImpl`, `deleteSurroundingTextImpl`, `forwardKeyImpl`, `updatePreeditImpl`) backed by an exact UTF-32 client buffer + cursor index.

### Phase 1: English Wayland / X11 / DBus Client Races (Build First)
- **Delayed-commit interleaving (`wayland_v2`):** Trigger `Replace` when `surroundingSynced == false` (`forwardKey(BackSpace)` × `N` + `15 ms` timer), then inject fast user keystrokes, `BackSpace`, `Return`, `FocusOut`, or `InputContextDestroyed` at `t + 0..14 ms` (before timer fires) and `t + 16 ms` (after timer fires). Assert zero character duplication, dropped letters, or use-after-free on destroyed `InputContext`.
- **GTK/Gecko reset-echo storms:** Emit `InputContextReset` at `t + 10..140 ms` (ignored by `SelfReplaceResetGuard`) vs `t + 160 ms` (genuine reset) during `JustAutocorrected` and `EditingRestored` undo flows.
- **Stale vs real `SurroundingTextUpdated`:** Deliver lagging pre-mutation snapshots within `80 ms` vs true mouse-click cursor displacements after `80 ms`, verifying undo disarms on real cursor jumps without false mid-word resets.
- **GTK4 DBus without `SurroundingText`:** Verify empty-program `"dbus"` contexts with invalid surrounding text strictly classify as `SMARTKEY_PROFILE_BYPASS` even under `unknown_app_policy = "prose"` (preventing `orig + replacement` duplication when `ForwardKey` is a no-op).

### Phase 2: Vietnamese `fcitx5-lotus` Async Bridge Races (Build Second)
- **Uinput burst + `Pause` chaining:** Interleave `PreInputMethod` letters, synthetic `BackSpace`/`Left`/`Delete` echoes (`< 350 ms` vs expired `> 350 ms`), `FcitxKey_Pause` echo re-arming, and intermediate mid-word `CommitStringEvent` flushes before the boundary commit arrives.
- **Fast post-boundary typing (`pendingPostBoundaryChars`):** Inject next-word keystrokes (`t`, `i`, `e`...) while `hasPendingBoundary()` is armed, followed by a batched `<word><space><next>` `CommitStringEvent`; verify `adjustedAct` deletes and re-appends `afterBoundaryPart` without losing or duplicating queued letters.
- **`LotusWlrVirtualKeyboard` synthetic echoes:** Inject non-ASCII runes (`>= 0x80`) and number-row ASCII echoes (`keycode 10..21`, including late post-Space echoes) and verify zero phantom leading characters leak into the next word while `< 250 ms` physical `BackSpace` undo still works.

---

## 5. Execution Order & Verification Gates

1. Implement **Tier A** in `crates/smartkey-core/tests/soak_sim.rs` (+ optional `smartkey-dev sim`).
2. Fix any core/learner edge cases surfaced by Tier A seeds; verify with `cargo test --workspace` and `cargo run --release -p smartkey-cli --bin smartkey-dev -- eval`.
3. Add the clock/timer seam to `adapters/fcitx5-smartkey/src/smartkey-addon.{h,cpp}` and build **Tier B Phase 1 (English Wayland)** followed by **Phase 2 (Lotus bridge)** in `adapters/fcitx5-smartkey/tests/`, wired into `ctest`.
