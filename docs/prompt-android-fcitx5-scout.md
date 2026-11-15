# Mission: Technical Reconnaissance for Porting SmartKey to Android (`fcitx5-android` Primary Target)

You are conducting a deep technical scouting investigation for porting **SmartKey** (`/home/domixi/Projects/smartkey` — or the current SmartKey repo workspace) to **Android**, with **`fcitx5-android`** as the primary target host.

## Operating Rules (Strict)

1. **Read-Only Scouting:** Do NOT modify existing SmartKey source files. If you need to clone upstream repos (`fcitx5-android/fcitx5-android`, `fcitx5-android/plugins`) to inspect real C++/Kotlin/CMake code, clone them into `/tmp/smartkey-android-scout/` using `--depth 1`.
2. **Zero Guessing:** Do NOT rely on training-data memory about `fcitx5-android` internals. Inspect actual upstream files (or fetch raw files from GitHub) and cite exact `repo/path:line` for every architectural claim.
3. **Label Evidence:** Tag every non-trivial finding as `[Observed]` (verified in code/build files) or `[Inferred]` (extrapolated, stating what test would prove it).
4. **No Fluff:** Write dense, engineering-grade findings structured strictly according to the Deliverable Template at the end.

---

## Step 1: Audit SmartKey's Current Portability Seam (Local Repo)

Inspect these local files first so you understand what already exists vs. what is Linux/Wayland-specific:

- `AGENTS.md` and `docs/architecture-and-plan.md` (note the legacy HeliBoard plan vs. our pivot to `fcitx5-android`).
- `crates/smartkey-core/Cargo.toml` and `crates/smartkey-core/src/` (`lib.rs`, `session.rs`, `scorer.rs`, `learner.rs`, `vi_telex.rs`, `store.rs`, `event_log.rs`, `lexicon.rs`).
- `docs/rfc-multilang-dict-packs.md` (v3 `.skdict` dictionary pack architecture).
- `crates/smartkey-ffi/Cargo.toml`, `crates/smartkey-ffi/src/lib.rs`, and `crates/smartkey-ffi/include/smartkey.h` (compare keystroke-stream `smartkey_session_process_key` + `smartkey_session_on_lotus_commit` vs. word-level `smartkey_mobile_on_*`).
- `adapters/fcitx5-smartkey/CMakeLists.txt`, `src/smartkey-addon.h`, and `src/smartkey-addon.cpp`.

Extract and list:

1. Any OS/POSIX/filesystem assumptions in `crates/smartkey-core` and `crates/smartkey-ffi` (verify zero `-sys` C deps, check `std::fs` usage in `store.rs` / `event_log.rs`, memory footprint of `bundled-en` `.skdict` vs. external mmap/bytes via `smartkey_engine_new_with_lexicon_bytes`).
2. Exactly which parts of `adapters/fcitx5-smartkey/src/smartkey-addon.cpp` are generic `fcitx5` (`Instance::watchEvent`, `InputContextProperty`, `SmartKeyAction` execution) vs. strictly Linux `fcitx5-lotus` Wayland Uinput workarounds (`isSyntheticVkKey`, burst echo suppression, XDG `$HOME` paths, desktop app-ID lists).

---

## Step 2: Inspect Upstream `fcitx5-android` & `fcitx5-android/plugins`

Clone with `--depth 1` into `/tmp/smartkey-android-scout/` (or inspect via GitHub):

- `https://github.com/fcitx5-android/fcitx5-android`
- `https://github.com/fcitx5-android/plugins` (specifically `unikey` and one simple module/engine plugin as reference)

Answer these exact engineering questions with `file:line` citations:

### A. Plugin APK vs. Fork Architecture (Make-or-Break Gate)

1. How does `fcitx5-android` discover and load external plugin APKs (`org.fcitx.fcitx5.android.plugin`, `plugin.xml`, `AddonLoader`, native `.so` search paths, `FCITX_ADDON_FACTORY`)?
2. **Signature & Permission Check:** Does `org.fcitx.fcitx5.android.permission.PLUGIN` use `protectionLevel="signature"`, `"normal"`, or something else? Can a self-signed out-of-tree plugin APK be loaded into official F-Droid / GitHub releases of `fcitx5-android`, or must the user install a custom-signed build / fork?
3. **Global `Category=Module` Watcher Support:** Can an external plugin APK register a `Category=Module` addon (`OnDemand=False` or loaded at startup) that attaches `instance_->watchEvent(EventType::InputContextKeyEvent, ...)` globally across all active input methods, or does `fcitx5-android` only load `InputMethod` engines from plugins?

### B. Virtual Keyboard, Candidate Bar & `AndroidFrontend` Event Pipeline

1. Trace a user tap on `fcitx5-android`'s built-in soft keyboard (`TextKeyboard` / `FcitxInputMethodService` -> JNI -> `AndroidFrontend` -> `InputContext`):
   - Do normal letter taps, Space, Backspace, Enter, punctuation, and long-press symbols emit `KeyEvent` through `InputContext::keyEvent` (triggering `PreInputMethod` and `PostInputMethod` watchers)?
   - What happens on **Backspace repeat / swipe-delete** or **spacebar cursor-move** gestures? Does it send `KeyEvent(BackSpace)` or bypass `InputContext` via direct `InputConnection` calls?
   - How does `fcitx5-android`'s built-in **English spellcheck / candidate bar** (`spell` addon / `KeyboardEngine`) work? Would it conflict with SmartKey's boundary replacement or feed candidates we can hook?
2. Trace `AndroidFrontend` -> Kotlin `InputConnection` for text replacement:
   - How does `AndroidFrontend` implement `InputContext::deleteSurroundingText(before, after)` and `commitString`?
   - Does `AndroidFrontend` set `CapabilityFlag::SurroundingText`? Are `deleteSurroundingText` + `commitString` wrapped in `InputConnection.beginBatchEdit()` / `endBatchEdit()` so word-boundary autocorrect doesn't flicker or race in Chrome/WebView/Telegram/Discord on Android?
   - How are cursor jumps / tap-to-reposition (`onUpdateSelection`) and focus changes (`onStartInputView` / `onFinishInputView`) forwarded into `InputContext` (`InputContextReset` / `SurroundingTextUpdated` / `FocusOut`)?

### C. Vietnamese Input Strategy on Android (3-Way Architecture Comparison)

On Linux, SmartKey sits beside `fcitx5-lotus` and runs `LotusSimEngine` (`crates/smartkey-core/src/vi_telex.rs`) to mirror it. On Android, `fcitx5-lotus` does not exist (no Uinput); instead `fcitx5-android/plugins` has `unikey` (`fcitx5-unikey`, inline preedit).

Compare these 3 concrete architectures for SmartKey on `fcitx5-android` and recommend one:

- **Option 1 (Addon Module + `plugin.unikey`):** SmartKey stays `Category=Module` watching `unikey` preedit/commit events + `keyboard-us`. Identify where `unikey` Telex/fallback rules diverge from `vi_telex.rs` (`LotusSimEngine`).
- **Option 2 (Unified SmartKey Bilingual `InputMethodEngine` Plugin):** Since `smartkey-core` already has a complete Rust Telex engine (`vi_telex.rs`) + EN/VI dictionaries + typo rescue + learner, wrap `smartkey-core` as a standalone `fcitx::InputMethodEngine` using Android inline `preedit` (`updatePreedit` + `commitString`). Evaluate whether this eliminates all dual-engine state desync and Uinput complexity.
- **Option 3 (Fork `fcitx5-android` directly):** Embed `smartkey-ffi` directly into `fcitx5-android` core C++/Kotlin.

### D. App Context, Bypass Safety & Storage / Sync on Android

1. **App Profiles & Password/Code Bypass:** How does `AndroidFrontend` expose `EditorInfo.packageName` (`ic->program()`?) and `EditorInfo.inputType` (`CapabilityFlag::Password`, `Email`, `Url`, `NoSpellCheck`, terminal apps like `com.termux`, mobile code editors, browser URL bars)? Map these to `SMARTKEY_PROFILE_*`.
2. **Paths, Dict Packs & Cross-Device Sync:**
   - Where `fcitx5-android` stores addon configs/data (`StandardPath` mapping on Android).
   - How `rules.toml`, `learner_state.json`, v3 `.skdict` packs, and `event_log` should live on Android so a user can sync `rules.toml` between Linux and Android (e.g., Syncthing / SAF folder / `fcitx5-android` backup-restore zip).
   - Note what replaces the desktop `smartkey` CLI on Android (e.g., `fcitx5` `Configuration` C++ macros auto-rendered into Android Preferences UI vs. custom Kotlin UI vs. file sync).

### E. Build Toolchain & Mobile UX Gap Check

1. **Build Pipeline:** Exact Gradle + CMake + `cargo-ndk` + `fcitx5-android` prebuilt SDK/headers workflow required to compile `crates/smartkey-ffi` (`arm64-v8a`, `x86_64` emulator) and link it into an `fcitx5-android` plugin/module without symbol visibility or C++ STL (`libc++_shared.so`) clashes.
2. **Mobile vs. PC Engine Gaps:** Which `smartkey-core` Layer 1 behaviors help vs. hurt on a phone touchscreen (e.g., physical Shift-hold normalizers, `;` -> `'` pinky rule, lack of touch spatial coordinates, silent spacebar autocorrect vs. candidate strip chips, undo-on-backspace UX on mobile)?
3. **Brief Sanity Comparison vs. HeliBoard / FlorisBoard:** In 1 short table, verify whether `fcitx5-android` has any dealbreakers (e.g., virtual keyboard feel, lack of glide typing, APK size, upstream maintenance health) compared to HeliBoard + JNI (`smartkey_mobile_on_*`), so we commit to `fcitx5-android` with eyes wide open.

---

## Deliverable Format

Write a comprehensive scouting report (`docs/scouting-android-fcitx5.md` if asked to save, otherwise return in response / artifact) with these exact sections:

1. **Executive Verdict & Recommended Architecture** (Plugin APK vs. Fork; Module Watcher vs. Unified Bilingual `InputMethodEngine`).
2. **`fcitx5-android` Internals & Event Flow Map** (with `file:line` citations for plugin loading, signature permissions, soft-key events, `AndroidFrontend` `deleteSurroundingText`/`commitString`/`preedit`, and `EditorInfo` flags).
3. **Vietnamese + English Integration Design** (Option 1 vs. 2 vs. 3 comparison table and concrete recommendation).
4. **What Transfers As-Is vs. What Must Change in `smartkey-core` & `smartkey-ffi`** (concrete file/API checklist).
5. **Storage, Config UI & Linux <-> Android `rules.toml` Sync Strategy.**
6. **Build & Packaging Blueprint** (`cargo-ndk` + CMake + Gradle + ABI targets + APK budget).
7. **Top Risks, Unknowns & Day-1 Prototype Plan** (the smallest PoC that proves end-to-end key interception, text replacement in Chrome/Telegram, and `rules.toml` persistence on an Android emulator/device).
