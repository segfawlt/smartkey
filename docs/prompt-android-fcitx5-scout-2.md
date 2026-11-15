# Mission: Round-2 Technical Recon — Verification & Phase-0 Blueprint for Porting SmartKey to Android (`fcitx5-android`)

You are conducting the **second and final scouting round** before implementation begins. Round 1 produced `docs/scouting-android-fcitx5.md` (verdict: **GO** — out-of-tree plugin APK, no fork; Phase 0 = port the existing `adapters/fcitx5-smartkey` addon as a `Category=Module` plugin; Phase 1 = Option 2, a unified bilingual `InputMethodEngine` plugin). Round 1 closed the architecture questions but deliberately left `[Inferred]` claims, unknowns **U1/U2**, and risks **R2/R4/R6/R7** unresolved. Round 2's job: **close every one of those gaps with citations or empirical builds, and deliver a Phase 0 blueprint so complete that coding can start with zero open design questions.**

## Operating Rules (Strict)

1. **Read-only on SmartKey.** Do NOT modify any file in the SmartKey repo. All clones, builds, and experiments go into `/tmp/smartkey-android-scout-2/` (create it; round-1 clones live in `/tmp/smartkey-android-scout/` — reuse them only if the round-1 pins still match upstream HEAD, otherwise re-clone fresh and note the drift).
2. **Zero Guessing:** Cite exact `repo/path:line` for every architectural claim. Reuse round-1 abbreviations: `APP` = fcitx5-android, `CORE` = fcitx submodule, `UNIKEY` = fcitx5-unikey, `SCF` = plugin-scaffold, `SK` = this repo. Re-check upstream HEAD and record any pin drift vs. round 1 (`APP @ eb5b502`, fcitx5 core @ `1e00551` / 5.1.22).
3. **Label Evidence:** `[Observed]` (read in code/build files), `[Empirical]` (you built/ran it — include the command and the observed output/size/symbol list), `[Inferred]` (extrapolated + what would prove it). `[Empirical]` beats `[Observed]`; prefer it whenever the toolchain allows.
4. **Toolchain Reality Check First:** Check for Android SDK/NDK (`sdkmanager`, `~/Android/Sdk`, `ANDROID_HOME`), `rustup`, `cargo-ndk`, JDK 17+, Gradle. If a component is missing, do NOT install system-wide packages silently — build what you can, and for the rest record **exact ready-to-run commands** as deliverables (§6 of the report) instead of fabricating results.
5. **No Fluff:** Dense, engineering-grade findings, structured strictly per the Deliverable Format below.

---

## Step 1: Reload Context (Local Repo)

Read first: `docs/scouting-android-fcitx5.md` (round-1 report — the object of this round), `docs/prompt-android-fcitx5-scout.md` (round-1 mission), `docs/architecture-and-plan.md`. Then as needed: `crates/smartkey-ffi/src/lib.rs`, `crates/smartkey-ffi/include/smartkey.h`, `crates/smartkey-core/src/{lexicon.rs,event_log.rs,vi_telex.rs}`, `adapters/fcitx5-smartkey/src/smartkey-addon.cpp` and its `CMakeLists.txt`.

Carry this **open-items ledger** into the round-2 report (every row must end up with a verdict in the Verification Ledger):

| ID | Round-1 open item | Where stated |
|---|---|---|
| R2 | delete+commit not batched → flicker risk in Chrome/WebView/Telegram; no `SurroundingText` capability | §2.4, §2.6, §7.1-R2 |
| R4 | fcitx5 prefab not published to Maven; vendoring path unproven; 5.1.22 ≠ desktop 5.0.20 floor | §6.3, §7.1-R4 |
| R6 | `FCITX_ADDON_FACTORY_V2_BACKWARDS` symbol resolution under Android loader | §4.3, §7.1-R6 |
| U1 | Rust `aarch64-linux-android` + `minSdk 23` under NDK 28 | §6.1, §7.1-U1 |
| U2 | Unknown-term UX without candidate chips (silent fix vs visible undo) | §7.1-U2 |
| R7 | No glide/voice typing — product risk | §7.1-R7 |
| §5.2 | Module addon with `FCITX_CONFIGURATION` gets a settings UI entry — asserted, not traced end-to-end | §5.2 |
| §6.6 | Addon conf fields for a plugin (`@REQUIRED_FCITX_VERSION@` substitution mechanics) | §6.6 |

---

## Step 2: Verification Tasks (exact engineering questions)

### A. V1 — Text-replacement atomicity & flicker (closes R2)

1. Re-trace the Kotlin commit path: `FcitxInputMethodService.kt` — for a `deleteSurroundingTextInCodePoints(n,0)` followed by `commitText(word)`, list every `InputConnection` op and every resulting `onUpdateSelection` callback. How does the app's `CursorTracker` react to IME-initiated deletion — does fcitx receive a reset/`focusOutIn`, or does state stay clean? Cite lines.
2. **Atomic alternatives, verified in code:** (a) `setComposingText(replacement)` → `finishComposingText()` — trace both in the app (batching exists on the composing path, round-1 `FcitxInputMethodService.kt:922-972`) and assess as a replacement primitive: does it behave correctly in a plain `EditText`, and what breaks in `TYPE_NULL`/WebView fields? (b) `commitText(text, cursor)` — confirm from Android docs/source whether it can replace existing text or only insert (verify, don't assume). (c) Any other delete+insert-in-one-op API the app already uses.
3. Produce a **strategy decision matrix**: for Chrome/WebView URL+form fields, Telegram/Discord (plain EditText), Termux (no standard IC), and password/`TYPE_NULL` fields — which strategy (delete+commit vs composing-text-replace vs no-op) and why.
4. **Upstreamability probe:** where in `FcitxInputMethodService.kt` is `beginBatchEdit` already used, and what minimal change would batch the `CommitStringEvent`+delete path? Sketch the concrete patch (Kotlin-level) a future PR could carry. Also check `UNIKEY`'s experimental `ModifySurroundingText` retro-edit (`unikey-im.cpp:282-344`) for semantic precedent on boundary replacement after commit.

### B. V2 — fcitx5 prefab gap & vendoring recipe (closes R4)

1. Inspect `APP/lib/fcitx5/build.gradle.kts` and the `prebuilt` submodule: what exactly does the module produce (prefab AAR? headers? CMake config packages?), and what does `find_package(fcitx5 REQUIRED CONFIG)` consume at plugin-build time — where do `Fcitx5Core`/`Fcitx5Utils` config files come from in the unikey build? Cite the generation/consumption points.
2. Produce the **exact vendoring recipe**: which files/dirs to copy into a standalone plugin repo, `settings.gradle.kts` changes, Gradle version pins, and how `@REQUIRED_FCITX_VERSION@` is substituted (`APP/build-logic/.../FcitxComponentPlugin.kt`).
3. From `APP/.github/workflows/publish.yml` + repo README: confirm precisely what IS on `maven.fcitx.workers.dev` (group IDs, versions) and what a standalone plugin must pin.

### C. V3 — Rust → Android toolchain (closes U1, closes R6 empirically if possible)

1. **Empirical when possible:** `cargo ndk -t arm64-v8a -t x86_64 -p 23 -- build --release -p smartkey-ffi` in a copy of the repo under `/tmp/smartkey-android-scout-2/`. Record: target support (`rustc --print target-list | grep android`), errors, per-ABI `.a` size, `nm` symbol table — verify only `smartkey_*` ABI exports survive and Rust std symbols are hidden (`--exclude-libs ALL` at link, or `#[visibility]`). If cargo-ndk/NDK are missing, document the exact install-free command sequence expected to work and mark `[Inferred]`.
2. Write out the mangled/exported symbol names both loader paths resolve for the factory (`CORE/src/lib/fcitx/addoninstance.h:201-241` macro expansion vs `APP/app/src/main/cpp/androidaddonloader/androidaddonloader.h:29-56` fallback) and confirm `FCITX_ADDON_FACTORY_V2_BACKWARDS` (SK `smartkey-addon.cpp:861`) resolves on the Android path — or state that Phase 0 should just switch to `FCITX_ADDON_FACTORY_V2`.
3. Check `crates/smartkey-ffi` compiles warning-free on the android target (no `std::fs`/`env` regressions expected — verify `#[cfg]`s if any).

### D. V4 — Option 2 (Phase 1) `InputMethodEngine` integration map

1. Use in-app `androidkeyboard` as the reference engine: which `fcitx::InputMethodEngine` (V2/V3?) methods it overrides (`APP/app/src/main/cpp/androidkeyboard/androidkeyboard.cpp`), per-IC state class, how Space/Enter/Backspace/punctuation are classified (`handleIgnoredKey` analogues), how virtual (`KeyStates::Virtual`) vs hardware keys are distinguished (or not), and how preedit + candidate list are driven (`updatePreedit`, `inputContext->inputPanel()`, `InputContextFlushUI`). Full citation lines.
2. Determine: can a **plugin APK** ship a `Category=InputMethod` addon exactly like a Module (addon conf in merged `usr/share`, dlopen from `FCITX_ADDON_DIRS`) — or is anything about IM registration in-app-only? If no plugin-shipped IM example exists upstream, say so explicitly and identify the loader code path that proves/disproves capability.
3. How fcitx5-android presents/switches IMs (IM list UI, per-app or global switching, what happens to pending preedit on switch) — cite. Note what `smartkey-en` (keystream passthrough) and `smartkey-vi` (`vi_telex.rs` preedit + unikey-style `word+boundary` commit) must implement differently from `androidkeyboard`.
4. Confirm from `CapabilityFlag.kt` which flags an IM engine can rely on (`Preedit` default) and what `NoSpellCheck`/`Password` fields mean for a SmartKey IM (must the engine itself bypass, or does the app already suppress IM activation?).

### E. V5 — Config, storage & sync verification (closes round-1 §5.2 assertion)

1. Trace end-to-end: a Module addon exposing `FCITX_CONFIGURATION` → where the addon list/settings UI creates the per-addon config fragment (which fragment/activity, `mergeConfigDesc`, `PreferenceScreenFactory`) → our `setConfig` override writing `conf/smartkey.ini`. Use unikey (`UNIKEY/src/unikey-im.h:41-45`) as the working precedent. Cite every hop.
2. DocumentsProvider/user-data browsing: the exact user-visible steps to reach `{extData}/config/smartkey/rules.toml` from the app UI (`FcitxDataProvider.kt`, `AdvancedSettingsFragment.kt` browse flow), and confirm the backup/restore zip includes `config/` + `data/` and its restore merge semantics (overwrite vs merge).

### F. V6 — Phase 0 skeleton concretization (blueprint input)

1. Enumerate **every file plugin-scaffold generates** (`SCF/build.cfg` fields, all templates) and write the exact rename/patch list to turn it into `plugin-smartkey`: applicationId `org.fcitx.fcitx5.android.plugin.smartkey`, debug variant id, manifest activity, `plugin.xml`, `descriptor.json` inputs, addon conf, `en.skdict` asset placement.
2. Go through `APP/plugin/unikey/build.gradle.kts` **line by line**: classify each line as unikey-specific vs required-for-any-plugin.
3. Deliver the **final `CMakeLists.txt` content** for the Phase 0 native module: scaffold vs unikey's 2-liner vs SK's desktop CMake reconciled — static `libsmartkey_ffi.a` link, `--exclude-libs,ALL`, `usr/share/fcitx5/addon/smartkey.conf` + `usr/share/smartkey/dicts/en.skdict` install destinations, ABI loop for cargo-ndk.
4. Emulator/debug recipe: exact app-debug + plugin-debug applicationIds, install order, where plugin data lands (`Android/data/<plugin-id>/files/...`), adb verification commands (logcat tags, `run-as`/file listing).

### G. V7 — Mobile UX gap check (feeds R7, U2)

1. Search the app for any undo-of-commit / revoke facilities (`undo`, `revoke`, candidate "undo" chips) — informs U2 (silent boundary fix vs visible undo on mobile).
2. Confirm with citations: no glide typing; voice typing presence/absence; floating/split keyboard modes; fullscreen extract-mode behavior for landscape (`onEvaluateFullscreenMode`) and its effect on preedit visibility.
3. Check whether the in-app clipboard **module** exposes any UI surface a SmartKey module could reuse for a future undo affordance (low priority, one paragraph).

---

## Deliverable Format

Write `docs/scouting-android-fcitx5-2.md` with these exact sections:

1. **Verification Ledger** — one table: round-1 item (R#/U#/§ref) → verdict (**Confirmed / Refuted / Partially**) → evidence (`file:line` or `[Empirical]` command + result) → impact on Phase 0/1.
2. **Replacement-Strategy Decision Matrix** (V1) — per app-class recommended strategy + the upstream batch-edit patch sketch.
3. **Phase 0 Implementation Blueprint** (V2+V3+V6) — final plugin-repo file tree, full `build.gradle.kts` + `CMakeLists.txt` contents, cargo-ndk command sequence, debug/emulator recipe. Written so an implementer can execute without re-scouting.
4. **Option 2 (Phase 1) Engine Integration Map** (V4) — virtual-method checklist, per-IC state, IM switching semantics, plugin-shipped-IM verdict.
5. **Config & Sync Verification** (V5).
6. **Toolchain Log** (V3) — what was empirically built vs. recorded as commands; ABI sizes; symbol checks.
7. **Updated Risk Register & Go/No-Go** — which risks are closed, which remain, and the final call on starting Phase 0 coding.
