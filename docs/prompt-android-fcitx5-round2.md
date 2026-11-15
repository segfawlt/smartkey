# Mission: Round 2 Surgical Deep-Dive (`fcitx5-android` Implementation Traps)

Follow up on `docs/scouting-android-fcitx5.md` using the existing clones in `/tmp/smartkey-android-scout/`. Stay strictly read-only on the SmartKey repo, cite exact `repo/path:line`, and mark `[Observed]` vs `[Inferred]`. Do not repeat Round 1 findings — answer ONLY these 4 implementation gates and append/update `docs/scouting-android-fcitx5.md` (Section 8) then commit & push.

## Gate 1: Preedit-to-Commit Atomicity (Can Option 2 Eliminate Risk R2?)
In `unikey` (`UNIKEY/src/unikey-im.cpp`) + `AndroidFrontend` + `FcitxInputMethodService.kt`:
1. Trace step-by-step what happens across JNI and `InputConnection` when an engine has non-empty client preedit (`ic->inputPanel().setClientPreedit(...)` + `ic->updatePreedit()`) and then commits at Space (`commitString("word ")` + clearing preedit).
2. In `FcitxInputMethodService.kt`, what exact `InputConnection` calls fire, in what order, and inside how many `beginBatchEdit()`/`endBatchEdit()` blocks? Does `commitText` replace the active composing span directly, or does `updatePreedit("")` fire a separate `setComposingText("", 1)` / `finishComposingText()` IPC call before/after `commitText`?
3. Verdict: If `smartkey-en` (Option 2) holds the current word in client `preedit` (like AOSP LatinIME/Gboard) instead of letting raw letters commit and calling `deleteSurroundingText` at Space, does that eliminate unbatched delete+commit flicker (R2) and block mid-word swipe-delete (R3)? List any downsides (e.g., apps with `TYPE_NULL` / terminal / underline styling).

## Gate 2: What Soft-Keyboard Features Depend on `keyboard-us` (`androidkeyboard`)?
If Option 2 replaces `keyboard-us` with custom `InputMethodEngine`s (`smartkey-en`, `smartkey-vi`):
1. Search the entire `APP/` (`fcitx5-android`) codebase for `"keyboard-us"`, `"androidkeyboard"`, `CapFlags`, auto-capitalization (Shift latch after `. ` or at field start `EditorInfo.TYPE_TEXT_FLAG_CAP_*`), double-space period, key popup/long-press symbols, and layout switching.
2. Which of those behaviors live in `TextKeyboard` / `FcitxInputMethodService` (shared automatically by ALL engines, including `unikey` and a future `smartkey-en`) vs. hardcoded inside `androidkeyboard.cpp` (which we would lose and must reimplement)?
3. How does `InputMethodEntry` metadata (`.conf` / `listInputMethods()`) bind an engine to the standard QWERTY `TextKeyboard` layout and spacebar label?

## Gate 3: Candidate Toolbar Integration (`CommonCandidateList`)
1. Show the minimal C++ `fcitx5` pattern used by `androidkeyboard.cpp` or `unikey` to display 2–3 horizontal chips in `fcitx5-android`'s candidate bar (`ic->inputPanel().setCandidateList(...)` + `ic->updateUserInterface(UserInterfaceComponent::InputPanel)`) and handle user tap (`CandidateWord::select(InputContext*)`).
2. Confirm how `fcitx5-android` renders `CommonCandidateList` on the toolbar (does chip 0 highlight as default, or only show clickable chips?) and whether tapping a chip happens on the same fcitx event-dispatcher thread.

## Gate 4: Real Out-of-Tree Native Build Wiring (`plugin-scaffold` & `fcitx5-android` Org Repos)
Round 1 noted `lib:fcitx5` is not published in `APP/.github/workflows/publish.yml`. Verify before we vendor `lib/fcitx5`:
1. Inspect EVERY template file in `/tmp/smartkey-android-scout/plugin-scaffold` (`build.gradle.kts.mustache`, `settings.gradle.kts.mustache`, `CMakeLists.txt.mustache`, GitHub workflows, README). How does a freshly generated out-of-tree native C++ plugin resolve `find_package(Fcitx5Core)` / prefab AARs?
2. Check GitHub org `fcitx5-android` for any actual out-of-tree plugin repos (e.g., `fcitx5-android/fcitx5-android` wiki/docs or `plugin-*` repos) to see whether `fcitx5` prefabs are published from a separate repo/release or if `includeBuild` / git submodule of `fcitx5-android` is the canonical pattern. Give the exact cleanest repo layout for our plugin.
