# Findings: ngosen (`github.com/ngosen/ngosen`) vs SmartKey — Feature & Implementation Comparison (2026-11-18)

> **Status:** Corrected & verified against `ngosen/ngosen` `main` source and SmartKey's live `SmartKeyEngine` + `fcitx5-smartkey` adapter.
> `[INFERENCE]` marks claims derived from reading `ngosen` source without executing `ngosen` in-process; SmartKey behavior is directly observed in code and test runs.

## TL;DR

1. **Different products, complementary strengths:** `ngosen` is a Vietnamese *input method composer* (with English passthrough/retention heuristics, retroactive IM-toggle conversion, macros, and a multi-protocol desktop wrapper). SmartKey is a multi-language *post-word autocorrector + learner* (with a dedicated `vi-lotus` Telex rescue & bilingual shield layer).
2. **Correction of earlier probe errors:**
   - The earlier `/tmp/bamptest` probe called bare `bamboo_core::Engine` (`auto_correct(true)`), bypassing `ngosen`'s actual `BambooEngine` wrapper (`bamboo-rs/src/engine/mod.rs`), which explicitly sets `Config::auto_correct(false)` and implements `nhuwg` → `nhưng`, `coongty` → `công ty`, `dduowjcd` → `được`, `tienf` → `tiền`, and `issue`/`sorry`/`coffee` restoration (`ngosen-state-retype.cpp` handles retroactive "Cứu chữ" on IM toggle).
   - The earlier `/tmp/skprobe` probe called English-only `LatinScorer::new_bundled()` instead of `SmartKeyEngine` in `vi-lotus` mode (`crates/smartkey-core/src/vi_telex.rs` & `session.rs`), missing that SmartKey already rescues Telex chatter, adjacent transpositions, split-coda transpositions (`tieegns`/`tieegsn` → `tiếng`, `hoagnf`/`hoagfn` → `hoàng`), and restore-class final tone substitutions (`duongx` → `dương`).
3. **What we actually adopt from `ngosen`:** **not** its 5-strategy XTEST/DEL/Messenger-overtype ladder (which conflicts with SmartKey's hard terminal & URL-bar `BYPASS` invariants), **but** two targeted safety guards in `adapters/fcitx5-smartkey`:
   - `CapabilityFlag::KeyEventOrderFix` (`canForwardBackspace`) to catch *named* GTK4/SDL `dbus` clients whose `ForwardKey` is a no-op stub, plus a hard abort before committing replacements when neither `deleteSurroundingText` nor `BackSpace` forwarding works;
   - post-`deleteSurroundingText` surrounding-text verification and an unanswered-delete circuit breaker per `InputContext` to prevent `orig + replacement` text duplication and blocklist poisoning.

---

## 1. Accurate Feature Comparison (Core & Vietnamese Telex)

| Capability | `ngosen` (`BambooEngine` + C++ core) | SmartKey (`SmartKeyEngine` + `fcitx5-smartkey`) |
| --- | --- | --- |
| **Primary model** | Mid-word Vietnamese Telex/VNI/VIQR composition (`Engine` + `BambooEngine` wrapper) | Post-word noisy-channel English/ES/FR autocorrect + deterministic `rules.toml` learner + `fcitx5-lotus` bilingual bridge |
| **General typo repair (EN/ES/FR)** | ❌ Not present (`recieve`, `teh`, `wrods` untouched) | ✅ `LatinScorer` + `RuleStore` (`95/95` EN recall, `0` FP on `141/141` eval corpus) |
| **Undo → permanent blocklist & manual fix learning** | ❌ No pair-level blocklist or `BS -> fix -> Space` learning | ✅ Pair blocklist, triangle capture, Mind-Change & Valid-Word shields |
| **Telex chatter & transposition rescue (`tieegns`, `hoagfn`, `ttieengs`, `duongx`)** | ❌ Unimplemented (`ROADMAP.md` item #4: *"Sửa lỗi gõ vội, như `tiếgn` thành `tiếng`"*) | ✅ Implemented in `crates/smartkey-core/src/vi_telex.rs` (`rescue_telex_typo_with_dict`, Strategies 1–5) |
| **Omitted vowel marks (`tienf` → `tiền`), `-ng` coda drop (`nhuwg` → `nhưng`), unspaced compounds (`coongty` → `công ty`)** | ✅ Implemented in `BambooEngine` (`auto_marked_word`, `MARKED_RIMES`, `KeptWord` tail drop, `space_reads_g_as_ng`) `[INFERENCE]` | ❌ Not yet handled in `vi_telex.rs` Strategies 1–5 |
| **Retroactive EN→VI conversion ("Cứu chữ") on IM switch** | ✅ Implemented (`src/core/ngosen-state-retype.cpp`) `[INFERENCE]` | ❌ Out of scope (delegated to Lotus / word-boundary flow) |
| **Mid-sentence English word protection in VI mode** | `english-words.txt` + `lost_a_letter_of_english_word` + `english-*-lookalikes.txt`; standalone `nest` → `nét` and `too` → `tô` remain open (`ROADMAP.md` #1) `[INFERENCE]` | `should_passthrough_telex` (English-Sentence Momentum Shield + `If`/`Of`/`Or` sentence-start passthrough + `1x BackSpace` learned `telex_passthrough`) + `EnglishLexicon` shield |
| **Vietnamese syllable authority** | Bundled `vietnamese.cm.dict` | Runtime `fcitx5-lotus` `vietnamese.cm.dict` (`resolve_lotus_dict_path`) with automatic fallback to bundled `vi_lexicon.skdict` (`vi_telex.rs:128-147`) |

---

## 2. Replacement Delivery: What SmartKey Rejects vs. Adopts

### Why `ngosen`'s 5-Strategy Ladder (`ngosen-state-replace.cpp:554-579`) Is NOT Copied Verbatim

`ngosen`'s `TypingState::deleteAndCommit` runs mid-word on every keystroke and across terminals, URL bars, IBus, XIM, and `fcitx4`:

```cpp
static constexpr Method methods[] = {
    {"overtype xtest",       &TypingState::overtypeXTestUsable,       &TypingState::runOvertypeXTest},
    {"overtype",             &TypingState::overtypeUsable,             &TypingState::runOvertype},
    {"del",                  &TypingState::delUsable,                  &TypingState::runDel},
    {"surrounding",          &TypingState::surroundingUsable,          &TypingState::runSurrounding},
    {"forwarded backspaces", &TypingState::forwardedBackspacesUsable,  &TypingState::runForwardedBackspaces},
};
```

1. **`del` (`erasesWithCommittedDel` for VTE/Ptyxis/xterm) — REJECTED:** SmartKey enforces `SMARTKEY_PROFILE_BYPASS` on all terminals (`CapabilityFlag::Terminal` and `classifyProgramName`), so autocorrect never mutates shell commands.
2. **`overtype xtest` (X11 `XTestFakeKeyEvent` for Chromium URL autocompletion) — REJECTED:** SmartKey enforces `SMARTKEY_PROFILE_BYPASS` on `CapabilityFlag::Url`, and `XTest` is X11-only (broken on native Wayland) while adding `libX11`/`libXtst` link dependencies.
3. **`overtype` (`Shift+Left` selection) — REJECTED:** In `ngosen-state-replace.cpp:522-525`, `overtypeUsable` only triggers when `looksLikeFacebookComposer(request.copy)` is true (not general Firefox/Chromium address bars), because arbitrary fields do not reliably report selection-only changes.
4. **Hardcoded app quirks (`ngosen-app-quirks.cpp`) — ALREADY HANDLED DIFFERENTLY:** Contrary to the earlier claim that `ngosen` relies purely on reported behavior, `ngosen-app-quirks.cpp` matches app names (`isChromiumBrowser`, `isFirefoxFamily`, `isWps`, `ptyxis`, `kgx`, `LibreOffice`). SmartKey centralizes app matching in `crates/smartkey-core/src/store.rs` (`classify_app`) + `adapters/fcitx5-smartkey/src/smartkey-addon.h`.

### What SmartKey Adopts

1. **Fcitx5 `CapabilityFlag::KeyEventOrderFix` (`1ULL << 37`) check on `frontend == "dbus"`:**
   - Binary inspection of Arch's installed modules confirms GTK3 (`im-fcitx5.so`) and Qt6 (`libfcitx5platforminputcontextplugin.so`) set `KeyEventOrderFix`, while GTK4 (`libim-fcitx5.so`, where `_fcitx_im_context_forward_key_cb` is `endbr64; ret`) and SDL do not.
   - SmartKey previously required `rawProg.empty()` alongside `frontend == "dbus"` and missing/invalid `SurroundingText` (`smartkey-addon.cpp:578-584`). Checking `!canForwardBackspace(frontend, flags) && !canSurround` protects *named* GTK4/SDL DBus clients as well, and `applyReplaceAction` / `applyDeleteAndCommit` abort without filtering the key or calling `commitString` if neither delete path is usable.
2. **Post-`deleteSurroundingText` verification & per-`InputContext` circuit breaker (`SurroundingSyncTracker`):**
   - Mirrors the core insight from `ngosen-state-check.cpp`: record a bounded pre-delete UTF-8 scalar snapshot around the cursor, classify the subsequent `InputContextSurroundingTextUpdated` (`Confirmed` vs `StalePreCommit` vs `DroppedDeletion`), mark `SurroundingText` untrusted for that `InputContext` when a deletion is silently dropped (or after `3` consecutive unanswered deletions when `BackSpace` forwarding is available), disarm `JustAutocorrected` so cleanup `BackSpace` presses do not poison `rules.toml`, and add a `Mode::JustAutocorrected` defense-in-depth check in `SmartKeySession::reconcile_surrounding`.

---

## 3. Verification & Backlog Status (Local-First)

- **Workspace & eval gates:** Verified via `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `smartkey-dev eval`, and `ctest --test-dir adapters/fcitx5-smartkey/build --output-on-failure`.
- **Fake-host simulation plan:** Already specified in `docs/stress-simulator-plan.md` (Tier A Rust bilingual soak + Tier B C++ `MockClientContext`) and `docs/future-work.md`.
- **GitHub CI & `smartkey status` `lotus.conf`/uinput warnings:** Deferred to `docs/future-work.md` while optimizing for single-machine local usage.
