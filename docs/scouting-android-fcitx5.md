# Scouting Report: Porting SmartKey to Android via `fcitx5-android`

> **Date:** 2026-11-16 · **Method:** read-only recon. Local repo audited directly; upstream inspected from fresh `--depth 1` clones in `/tmp/smartkey-android-scout/`.
> **Pins inspected:** `fcitx5-android` @ `eb5b502` (2026-10-13), fcitx5 core submodule @ `1e00551` (= fcitx **5.1.22**), `fcitx5-unikey` @ `ed9170c`, `plugin-scaffold` @ HEAD.
> **Citation abbreviations:** `APP` = fcitx5-android, `CORE` = fcitx (pinned submodule), `UNIKEY` = fcitx5-unikey, `SCF` = plugin-scaffold, `SK` = this repo.
> **Correction to the mission brief:** there is no `fcitx5-android/plugins` repo (404). Plugins are **in-tree** under `APP/plugin/` (engines as git submodules); out-of-tree plugins are generated from `fcitx5-android/plugin-scaffold`. All plugin questions were answered against the real layout.

---

## 1. Executive Verdict & Recommended Architecture

**Verdict: GO — out-of-tree plugin APK, no fork.** Two-stage architecture:

- **Phase 0 (de-risk, ~days):** cross-compile the **existing** `adapters/fcitx5-smartkey` addon as a `Category=Module` plugin APK and validate the English boundary-replacement loop end-to-end. Nearly all of its non-lotus code is Android-compatible as-is (details §4).
- **Phase 1 (target): **Option 2 — one unified bilingual `InputMethodEngine` plugin** owning both EN autocorrect and VI Telex preedit, powered entirely by `smartkey-core`.

**Why the plugin channel is real (the make-or-break gates):**

1. **A self-signed plugin APK loads into official builds.** The `org.fcitx.fcitx5.android.permission.PLUGIN` permission is `protectionLevel="signature"` (`APP/app/src/main/AndroidManifest.xml:33-35`) but it is **never checked in code** — zero `checkSignature`/`GET_SIGNATURES`/`GET_SIGNING` hits across `app/src`, `lib/`, `build-logic/`; the only `PackageInfo` uses read version/meta-data (`APP/.../core/data/DataManager.kt:151-156`). Signature only matters for the *reverse* IPC channel: plugins wanting to bind `FcitxRemoteService` need the app-declared `.permission.IPC` signature permission (`APP/app/src/main/AndroidManifest.xml:29-31, 139-147`), which a self-signed plugin won't be granted — irrelevant for a pure addon plugin like unikey. `[Observed]`
2. **`Category=Module` addons from plugins are loadable and nothing filters them.** The addon scan reads every `*.conf` under `PkgData/addon` (`CORE/src/lib/fcitx/addonmanager.cpp:264-301`), loads `OnDemand=False` addons at startup (`addonmanager.cpp:157-189`), and neither the native `getAddons` (`APP/app/src/main/cpp/native-lib.cpp:102-106, 296-300`) nor the Kotlin UI filters by category (`APP/.../ui/main/settings/addon/AddonListFragment.kt:98-101`). The pattern already exists in-tree: the clipboard **module** registers a global `instance_->watchEvent<EventType::InputContextKeyEvent>(EventWatcherPhase::Default, ...)` (`CORE/src/modules/clipboard/clipboard.cpp:191-198`). Our desktop addon uses the identical `watchEvent` mechanism (`SK/adapters/fcitx5-smartkey/src/smartkey-addon.cpp:115-125`). `[Observed]`
3. **Key events from the soft keyboard traverse the standard fcitx5 watcher pipeline** — `AndroidFrontend::keyEvent` constructs a plain `fcitx::KeyEvent` and calls `activeIC_->keyEvent(keyEvent)` (`APP/app/src/main/cpp/androidfrontend/androidfrontend.cpp:367-372`), dispatching through `PreInputMethod → InputMethod → PostInputMethod` exactly like hardware keys (`CORE/src/lib/fcitx/instance.cpp:1656-1683`). `[Observed]`
4. **`deleteSurroundingText` + `commitString` reach the app's `InputConnection`** (`APP/.../input/FcitxInputMethodService.kt:309-311, 340-350, 423-454`) — SmartKey's word-boundary replacement primitive exists on Android. Caveat: they are **not** wrapped in `beginBatchEdit`/`endBatchEdit` (only the cursor-commit and composing-text paths are — `FcitxInputMethodService.kt:449-452, 925, 971`), so a delete+commit pair is two `InputConnection` ops. `[Inferred: minor flicker risk in Chrome/WebView; must be tested in the Day-1 PoC.]`

**Known costs, accepted with eyes open:** no `SurroundingText` capability on Android (§2.6 — our shadow buffer already doesn't need it); the fcitx5 prefab SDK is **not published to Maven** so a native plugin must vendor `lib/fcitx5` (§6); no glide typing in `fcitx5-android` (§7 sanity table).

---

## 2. `fcitx5-android` Internals & Event Flow Map

### 2.1 Plugin APK discovery & loading chain `[Observed]`

```
Install plugin APK (applicationId prefix org.fcitx.fcitx5.android.plugin.[.debug])
  └─ exported activity w/ intent action ${applicationId}.plugin.MANIFEST   ← inherited from lib/plugin-base
     (APP/lib/plugin-base/src/main/AndroidManifest.xml:24-31; debug variant :27-33)
  └─ res/xml/plugin.xml: <apiVersion>0.1</apiVersion> (+domain, +description)
     (APP/plugin/unikey/src/main/res/xml/plugin.xml:2; parsed APP/.../core/data/DataManager.kt:110-146)
  └─ assets/descriptor.json (sha256 manifest of usr/share files)
     (APP/build-logic/convention/src/main/kotlin/DataDescriptorPlugin.kt:47-158)
APP side:
  1. pm.queryIntentActivities(Intent(PLUGIN_INTENT), MATCH_ALL)   (DataManager.kt:37, 93-102)
  2. apiVersion gate == PluginDescriptor.pluginAPI "0.1"          (DataManager.kt:149-172; PluginDescriptor.kt:42)
  3. descriptor.json → DataHierarchy merge into {dataDir}/usr/share
     (DataManager.kt:181-283; conflicts hard-fail: DataHierarchy.kt:20-35)
  4. plugin nativeLibraryDir appended to app nativeLibraryDir     (APP/.../core/Fcitx.kt:429-439)
  5. startupFcitx(... appLib ...) → setenv("FCITX_ADDON_DIRS", appLib)
     (native-lib.cpp:510-580, esp. :568)
  6. AndroidSharedLibraryLoader registered before instance init   (native-lib.cpp:86)
  7. AddonManager scans PkgData/addon/*.conf (now incl. plugin-contributed)
     (CORE/src/lib/fcitx/addonmanager.cpp:264-301)
  8. loader dlopens <Library>.so from any FCITX_ADDON_DIRS dir
     (APP/app/src/main/cpp/androidaddonloader/androidaddonloader.cpp:30-100)
  9. resolves fcitx_addon_factory_instance_<name> → AddonFactory::create
     (androidaddonloader.h:29-56; same symbols as FCITX_ADDON_FACTORY_V2, CORE/src/lib/fcitx/addoninstance.h:201-241)
```

- No per-plugin JNI entry, no `System.loadLibrary` of plugin code, no manual `.so` extraction — the Android package installer extracts native libs to `nativeLibraryDir` (`useLegacyPackaging = true`, `APP/build-logic/convention/src/main/kotlin/NativeAppConventionPlugin.kt:20-24`); the only `System.loadLibrary` in the app is its own `native-lib` (`Fcitx.kt:232`). `[Observed]`
- Package-id gate: prefix `org.fcitx.fcitx5.android.plugin.` (+ `.debug` matching a debug build's id suffix) (`APP/.../core/data/PluginDescriptor.kt:40-41`). `[Observed]`

### 2.2 Signature & permission semantics `[Observed]`

| Mechanism | Direction | Effect for a self-signed plugin |
|---|---|---|
| `.permission.PLUGIN` (signature) | app self-uses; protects nothing at load time | none — loading is intent + apiVersion gated |
| `.permission.IPC` (signature) | protects `FcitxRemoteService` | plugin can't bind it (loses remote-service/IPC features only) |
| Package-name prefix + `plugin.xml` apiVersion | discovery gate | must match exactly |

README confirms GitHub Release/F-Droid share one signing key (`APP/README.md:14-27`) — i.e. a self-signed SmartKey plugin coexists with **any** of the official distributions, but users must not expect app-upgrade compatibility with a Play-store variant (different cert there; loading still unaffected).

### 2.3 Soft-key → KeyEvent pipeline `[Observed]`

1. Letter keys are `AlphabetKey` = `Behavior.Press(KeyAction.FcitxKeyAction(char))` with `states = KeyStates.Virtual` and an evdev scancode (`APP/.../input/keyboard/KeyDefPreset.kt:42-62`; `KeyAction.kt:14-18`; `TextKeyboard.kt:35-37`).
2. Click → `CommonKeyActionListener` → `service.postFcitxJob { sendKey(act, states, code) }` (`BaseKeyboard.kt:203-207, 459-464`; `CommonKeyActionListener.kt:93-95`).
3. JNI `sendKeyToFcitxString` → `fcitx::Key{keySymFromString, KeyStates(state), code + 8}` → `IAndroidFrontend::keyEvent` (`Fcitx.kt:81-88, 261-267`; `native-lib.cpp:768-774, 114-116`).
4. `AndroidFrontend::keyEvent` → `activeIC_->keyEvent(KeyEvent)`; **if unaccepted**, bounces to Kotlin via `keyEventCallback` → `FcitxEvent.KeyEvent` → `commitText(unicode)` for virtual keys (`androidfrontend.cpp:367-375`; `native-lib.cpp:640-649`; `Fcitx.kt:397-402`; `FcitxInputMethodService.kt:245-249`).

**Consequence for SmartKey:** letters normally pass through the engine (so `PreInputMethod` watchers see every raw keystroke), and the app itself commits the char when no engine filters. Boundary keys are the same event stream. All taps are serialized on the fcitx dispatcher thread (`postFcitxJob`, `FcitxInputMethodService.kt:87, 197-203`; `Fcitx.kt:423-478`) — matching the FFI single-thread handle contract (`SK/crates/smartkey-ffi/include/smartkey.h:19-24`). `[Observed]`

### 2.4 Backspace repeat / swipe-delete / spacebar cursor-move `[Observed]`

| Gesture | Path | SmartKey visibility |
|---|---|---|
| Backspace tap + **repeat** (`RepeatInterval = 50 ms`) | full `sendKey(FcitxKey_BackSpace)` through `InputContext::keyEvent` each tick (`KeyDefPreset.kt:128-143`; `BaseKeyboard.kt:214-220`; `CustomGestureView.kt:171-183`) | ✅ watcher sees every BS |
| Backspace delivery (unaccepted) | Kotlin: selection → `commitText("")`; `TYPE_NULL`/flag → `sendDownUpKeyEvents(KEYCODE_DEL)`; else `ic.deleteSurroundingTextInCodePoints(1, 0)` (`FcitxInputMethodService.kt:352-381`) | deletion bypasses fcitx, but session already consumed the BS event |
| **Swipe-delete** | pure Kotlin/InputConnection: Move → `setSelection` extend, Up → `commitText("")` (`BaseKeyboard.kt:178-199`; `FcitxInputMethodService.kt:488-493, 516-525`); skipped while preedit/candidates active (`CommonKeyActionListener.kt:135-152`) | ⚠️ **invisible to fcitx** — shadow-buffer desync risk (§7 R3) |
| Spacebar cursor-move | per-step virtual `Left`/`Right` KeyEvents through fcitx (`BaseKeyboard.kt:154-177`); unaccepted → `ic.setSelection` (`FcitxInputMethodService.kt:403-421`) | ✅ = `SMARTKEY_KEY_NAV_OR_CLICK` reset |
| Direct-commit bypasses | `CommitAction` keys, clipboard window, selection-delete (`CommonKeyActionListener.kt:99-102`; `ClipboardWindow.kt:127`; `FcitxInputMethodService.kt:379, 492`) | minor; same class as desktop mouse/clipboard gaps |

### 2.5 Built-in English spell/candidate system `[Observed]`

- Upstream `keyboard` engine is compiled **out** (`ENABLE_KEYBOARD OFF` in `APP/lib/fcitx5/src/main/cpp/CMakeLists.txt` options block); the app ships its own `androidkeyboard` engine exposing `keyboard-us` "English" (`APP/app/src/main/cpp/androidkeyboard/androidkeyboard.cpp:164-172`).
- Candidates: spell module `hintForDisplay(...)` → `CommonCandidateList` of `AndroidKeyboardCandidateWord` whose `select()` = `inputContext->commitString(...)` (`androidkeyboard.cpp:248-291`); gated off by `NoSpellCheck`/`Password`/no-dict (`:300-306`). Candidate UI via `InputContextFlushUI` → bulk/paged lists (`androidfrontend.cpp:94-148, 342-364`). libime is for Chinese only (`APP/app/src/main/cpp/CMakeLists.txt:57-59`).
- Conflict assessment: SmartKey's EN mode is autocorrect-on-boundary, not chip candidates — the spell bar competes for UX mindshare but not for buffer state. A SmartKey IME can coexist (spell bar shows suggestions; SmartKey silently fixes on boundary) or replace `keyboard-us` outright. `[Inferred]`

### 2.6 Replacement primitives, preedit, focus, app context `[Observed]`

- `AndroidInputContext : InputContextV2` (`androidfrontend.cpp:20-30`).
  - `commitStringImpl` → JNI → `FcitxEvent.CommitStringEvent` → `ic.commitText(text[, cursor])` (`androidfrontend.cpp:39-45`; `FcitxInputMethodService.kt:232-236, 423-454`).
  - `deleteSurroundingTextImpl(before, after)` → `ic.deleteSurroundingTextInCodePoints(before, after)` (or `deleteSurroundingText` pre-N) — **not capability-gated, not batched** (`androidfrontend.cpp:51-60`; `FcitxInputMethodService.kt:309-311, 340-350`).
- **`CapabilityFlag::SurroundingText` is never set**: `DefaultFlags = Preedit | ClientUnfocusCommit | CommitStringWithCursor` and `fromEditorInfo` never adds it (`APP/.../core/CapabilityFlag.kt:91-95, 97-180`); no `updateSurroundingText` anywhere in `app/src/main/cpp/`; in-code TODO: *"maybe it's time to implement SurroundingText"* (`androidkeyboard.cpp:243`). Selection is tracked privately by `CursorTracker`/`onUpdateSelection` instead (`FcitxInputMethodService.kt:123, 780-799, 856-915`).
  - **Impact:** our Linux guard `ic->capabilityFlags().test(CapabilityFlag::SurroundingText)` (`SK/.../smartkey-addon.cpp:434`) would always fail → the addon would take its `forwardKey(BackSpace)` fallback. Phase 0 must switch to unconditional `deleteSurroundingText` (the Android impl ignores the flag anyway) and rely on the shadow buffer + `smartkey_session_reconcile_surrounding` only when text is independently available.
- Preedit: `updatePreeditImpl` → client preedit → Kotlin `ic.setComposingText(...)` inline in the editor, cursor via `setSelection`, batched (`androidfrontend.cpp:62-64`; `FcitxInputMethodService.kt:922-972`). IME-window (app-bar) preedit via `InputPanelEvent` (`PreeditComponent.kt:37-41`). Both channels available to a SmartKey VI engine.
- Focus/reset: `onStartInput`/`onStartInputView` → capability merge + `focus(true)`; `onFinishInputView` → `finishComposingText` + `focusOutIn()` (= `focusOut(); focusIn()`); cursor-jump-outside-preedit → `finishComposingText + focusOutIn`; hard `reset()` → `InputContext::reset` → `ResetEvent` (`FcitxInputMethodService.kt:727-762, 880-914, 1050-1072`; `androidfrontend.cpp:431-449`; `CORE/src/lib/fcitx/inputcontext.cpp:375-379`). All map 1:1 onto our existing `SMARTKEY_KEY_FOCUS_LOST` / `InputContextReset` / `InputContextFocusOut` watchers (`SK/.../smartkey-addon.cpp:127-163`).
- `ic->program()` = **app package name**: `pkgName = PackageManager.getNameForUid(uid)` with `":uid"` stripped, numeric-uid fallback (`androidfrontend.cpp:20-30`; `PackageNameCache.kt:11-23`). Feeds `normalizeProgramId`/`smartkey_engine_classify_app_override` directly.
- `EditorInfo` → capabilities (`CapabilityFlag.kt:97-180`): `Password` (+`Sensitive`/`NoSpellCheck` for visible passwords), `Email` (:139-143), `Url` (:153-155), `Number`/`Dialable`, `NoSpellCheck` (TYPE_NULL/AUTO_CORRECT off), `Multiline`, `Sensitive` (`NO_PERSONALIZED_LEARNING`). Applied via `setCapabilityFlags` JNI (`native-lib.cpp:1034-1037`; `androidfrontend.cpp:474-477`). This is a **superset** of what `classifyProfile` consumes (`SK/.../smartkey-addon.cpp:252-274`) — the `SMARTKEY_PROFILE_*` mapping transfers unchanged. Terminal apps like Termux: no capability flag; handle via `[app_profiles]` rule `"com.termux" = "bypass"` in `rules.toml`.

---

## 3. Vietnamese + English Integration Design (3-Way Comparison)

### 3.1 unikey ground truth `[Observed]`

- Engine `UnikeyEngine` (`UNIKEY/src/unikey-im.cpp:358`) with per-IC `UnikeyState` (:132). Composition buffer is internal (`preeditStr_`, :353); client preedit when `CapabilityFlag::Preedit` (:772-806) — Android always sets `Preedit` by default (`CapabilityFlag.kt:91-95`).
- Key handling (`unikey-im.cpp:489-636`): Enter/Delete/Tab/nav → `handleIgnoredKey()` = **commit pending preedit, forward key** (:513-519, 730-734); Backspace captured & re-transforms (tone position may move, :529-564); **Space commits `word+space` as one `commitString`** (:620-626); consonant/digit word-start autocommit passes through unfiltered (:577-588); double-Shift / Shift+Space → `restoreKeyStrokes()` raw-ASCII restore (:495-507, 608-613). IM-switch-away commits; context reset **discards** (:472-479, 638-648, 189-194). Experimental `ModifySurroundingText` can retro-edit committed text (`unikey-config.h:61-66`; `unikey-im.cpp:282-344`) — default **off**.
- Tables are inline in `UNIKEY/unikey/inputproc.cpp`: Telex (:59-74, incl. `[→ư ]→ơ {→Ư }→Ơ` bracket map-chars), SimpleTelex (:76-79, W=hook-all), SimpleTelex2 (:81-84), VNI (:86-89), VIQR (:91-95), MSVI (:97-121); vowel-doubling is the `VSeqList` state machine (`ukengine.cpp:69-110, 271-541`); handlers `processRoof/Hook/Dd/Tone/TelexW/MapChar` (`ukengine.cpp:834-855`).
- **"Spell check" is a pure word-form state machine, no dictionary** (zero `checkVN` hits): vowel/consonant sequence failure marks `vnw_nonVn` (`ukengine.cpp:2018-2022, 2068-2069`); `AutoNonVnRestore` (default true) restores raw keystrokes at word break for invalid words (`ukengine.cpp:2908-2949, 2955-2989`); `ModernStyle` (default false = old `òa/úy` tone placement) shifts tone position (`ukengine.cpp:1525-1527`); `FreeMarking` loosens validity (:1121, 1163, ...). `strictSpellCheck` is declared but never read (`keycons.h:30-40`).
- Android packaging is verbatim: submodule + 2-line CMake (`APP/plugin/unikey/src/main/cpp/CMakeLists.txt:1-21`), zero Kotlin, all settings via the generic config pipeline (§5.2).

### 3.2 Options compared

| | **Option 1: Module watcher + `plugin.unikey`** | **Option 2: Unified bilingual SmartKey IME plugin** ✅ | **Option 3: Fork fcitx5-android** |
|---|---|---|---|
| Effort | Lowest — port existing addon nearly verbatim (see Phase 0) | Medium — new `InputMethodEngine` wiring around `smartkey-core` (engine logic already exists) | Highest — permanent fork tax |
| Engine ownership | unikey owns VI preedit/commit; SmartKey watches `CommitStringEvent` + boundary keys (≈ current fcitx5-lotus architecture, `SK/.../smartkey-addon.cpp:743-820`) | SmartKey owns everything: VI preedit via `vi_telex.rs`, EN via passthrough + boundary replace | N/A |
| Desync surface | Two engines' buffers (unikey `preeditStr_` vs SmartKey shadow) + invisible swipe-delete | Single buffer; swipe-delete still invisible in EN mode | Single |
| VI fidelity | unikey Telex ≠ Lotus semantics: old-style tone placement default, `freeMarking` validity, `AutoNonVnRestore` fallback, bracket keys, double-Shift restore, no Lotus `Pause`-burst model. `LotusSimEngine` (`SK/crates/smartkey-core/src/vi_telex.rs`) must be diffed & re-tuned — **not bit-identical** `[Inferred: verify by corpus A/B in Phase 1]` | We control every rule; mirror Lotus exactly; add unikey-compat presets later if wanted | Full control |
| Double-correction risk | Real: unikey restores raw ASCII on invalid VN words → SmartKey EN may see `"nguoi"` etc.; mitigated by `inputMethod()=="unikey"` → `set_lang("vi")` + pass-through shields | None (one engine tags lang itself) | None |
| User setup | Install unikey plugin + SmartKey plugin, enable both | One plugin, one IM per lang (or auto-switch) | Custom app install |
| Maintenance | Tracks upstream unikey | Ours only | Every upstream rebase |

### 3.3 Recommendation

**Commit to Option 2**, with **Option 1 executed first as Phase 0** because ~85% of its code is the addon we already have (§4), and it proves the three riskiest unknowns (plugin load on official builds, delete+commit UX in Chrome/Telegram, `rules.toml` persistence) without writing an IME. Concretely:

- **Phase 0 deliverable:** `libsmartkey.so` (Module, `OnDemand=False`) + `usr/share/fcitx5/addon/smartkey.conf` + `en.skdict` asset in a plugin APK. EN path: `watchEvent(InputContextKeyEvent, PreInputMethod)` → `smartkey_session_process_key` → `deleteSurroundingText + commitString`. VI path: dormant (unikey integration deferred until its rules are re-tuned or Option 2 lands).
- **Phase 1 deliverable:** `Category=InputMethod` engines `smartkey-en` (keystream mode, no preedit) and `smartkey-vi` (`vi_telex.rs` produces client preedit via `updatePreedit`, commits `word+boundary` like unikey does). One shared `SmartKeyEngine`; `set_lang` switches per active IM. This **eliminates** the dual-engine desync and the entire Uinput/burst/echo layer (`isSyntheticVkKey`, `kTextKeys`, burst windows, `FcitxKey_Pause` handling — `SK/.../smartkey-addon.cpp:500-614` — all dead on Android).

---

## 4. What Transfers As-Is vs. What Must Change

### 4.1 `crates/smartkey-core` — transfers with 2 small seams `[Observed]`

| Item | Status | Evidence |
|---|---|---|
| Zero `-sys` deps, pure std | ✅ as-is | `SK/crates/smartkey-core/Cargo.toml:12-17` (only `fst`, `levenshtein_automata`, `serde`, `serde_json`, `toml_edit`) |
| `SmartKeyEngine`/`Session`, scorer, casing, learner, `rules.toml` store | ✅ as-is — all paths injected, "Never reads `$HOME` implicitly" | `SK/crates/smartkey-core/src/store.rs:664, 751, 776` |
| Dict loading | ✅ as-is | `EnglishLexicon::from_owned_bytes` is documented for "Android asset stream" (`SK/crates/smartkey-core/src/lexicon.rs:225-227`); `smartkey_engine_new_with_lexicon_bytes` exists (`smartkey.h:77-83`) |
| `event_log::resolve_home_dir` | ⚠️ Linux-only (`/proc/self` uid vs `/etc/passwd`, `$HOME`) | `SK/crates/smartkey-core/src/event_log.rs:69-116` — host must always inject `log_dir` via `smartkey_engine_set_log_dir` (already the addon pattern, `smartkey-addon.cpp:103`); `XDG_STATE_HOME` is not set on Android |
| `lexicon::resolve_dict_path` | ⚠️ Linux paths (`/usr/share/smartkey/dicts`, `XDG_DATA_HOME`) | `SK/crates/smartkey-core/src/lexicon.rs:529-560` — bypass with `with_lexicon_bytes`, or have the addon `setenv("SMARTKEY_DICT_DIR", {extData}/data/smartkey/dicts)` before `engine_new` |
| `store.rs` lotus.conf sibling discovery | ✅ harmless (optional file probe) | `SK/crates/smartkey-core/src/store.rs:793-801` |

### 4.2 `crates/smartkey-ffi` — transfers 100% `[Observed]`

- Panic-safe: 10 × `catch_unwind` across the ABI (`SK/crates/smartkey-ffi/src/lib.rs`), zero `std::fs`/`env` usage — all paths arrive as UTF-8 strings. Staticlib + cdylib already configured (`smartkey-ffi/Cargo.toml:9`).
- Keystream API (`process_key`, `on_lotus_commit`) and word-level `smartkey_mobile_on_*` API both present (`smartkey.h:116-204`) — Option 2 needs the keystream API + `on_lotus_commit`'s boundary-commit logic generalized (rename semantics: any engine boundary commit, not just Lotus).

### 4.3 `adapters/fcitx5-smartkey` — generic vs. Linux-only split `[Observed]`

| Component | Lines | Android fate |
|---|---|---|
| `watchEvent` watchers (Key Pre/Post, CommitString, UpdatePreedit, SurroundingTextUpdated, FocusOut, Destroyed/Reset/SwitchIM) | `smartkey-addon.cpp:112-163` | ✅ reuse verbatim (Module) / reimplement inside engine (IME) |
| `InputContextProperty` factory + per-IC state | `:89-91, 112-113` | ✅ same |
| `decodeKeyEvent` (mods, BS, Enter, Tab, nav, charsym) | `:365-425` | ✅ same; `Key::keySymToUnicode` path identical because soft keys carry real keysyms |
| `applyDeleteAndCommit` | `:429-452` | 🔧 drop the `SurroundingText`-capability guard (never set on Android, §2.6); call `ic->deleteSurroundingText` unconditionally; add batch-edit-free flicker test |
| Profile/classification + bypass logging | `:244-318, 486-496` | ✅ same; `program()` = package name; extend `[app_profiles]` for Android packages |
| English path (`keyboard-*`, `isComposing` guard) | `:616-647` | ✅ core of Phase 0 |
| **Lotus machinery**: `isLotusActive`, burst/echo windows, `isSyntheticVkKey`/`kTextKeys`, `FcitxKey_Pause`, pending-boundary replay, `onCommitString` word splitting | `:234-242, 500-614, 649-741, 743-820`; `smartkey-addon.h:142-263` | ❌ dead on Android (no fcitx5-lotus, no Uinput) — delete; its *ideas* (boundary-commit eval, post-boundary char replay) reappear as the Option-2 commit path |
| XDG path resolution (`getenv`, `getpwuid_r`) | `:34-69` | 🔧 replace: read `XDG_CONFIG_HOME`/`XDG_DATA_HOME`/`FCITX_*` env the app already sets (§5.1); `$HOME` = extData as backstop |
| `FCITX_ADDON_FACTORY_V2_BACKWARDS` | `:861` | ⚠️ verify the Android loader resolves the legacy symbol (`androidaddonloader.h:36-56` falls back to plain `fcitx_addon_factory_instance`) — or switch to `FCITX_ADDON_FACTORY_V2` |

---

## 5. Storage, Config UI & Linux ↔ Android `rules.toml` Sync

### 5.1 Where everything lives on Android `[Observed]` (`native-lib.cpp:531-581`)

| Env var | Android path | SmartKey mapping |
|---|---|---|
| `HOME` | `{extData}` (= `getExternalFilesDir(null)`) | backstop only |
| `XDG_CONFIG_HOME` / `FCITX_CONFIG_HOME` | `{extData}/config` | **`{extData}/config/smartkey/rules.toml`**, `learner_state.json` |
| `XDG_DATA_HOME` / `FCITX_DATA_HOME` | `{extData}/data` | `{extData}/data/smartkey/dicts/en.skdict` (v3 packs) |
| `XDG_DATA_DIRS` | `{dataDir}/usr/share` (merged read-only tree from app+plugin `descriptor.json`) | **bundled `en.skdict` ships as plugin asset `usr/share/smartkey/dicts/en.skdict`** → merged tree |
| `FCITX_ADDON_DIRS` | app + plugin `nativeLibraryDir` | where `libsmartkey.so` gets dlopen'd from |
| `XDG_CACHE_HOME` / `XDG_RUNTIME_DIR` | `extCache` | scratch |
| `XDG_STATE_HOME` | **not set** | inject log dir explicitly: `{extCache}/smartkey/logs` or `{extData}/data/smartkey/logs` via `smartkey_engine_set_log_dir` |

### 5.2 Config UI: zero Kotlin needed `[Observed]`

`FCITX_CONFIGURATION` options on our addon → `getConfig()` → `mergeConfigDesc` → RawConfig `desc`+`cfg` (`native-lib.cpp:219-237, 893-929`) → `PreferenceScreenFactory` renders stock androidx preferences (`AddonConfigFragment.kt:19-38`; `PreferenceScreenFactory.kt:50-68`) → `setConfig` → our `setConfig` override does `safeSaveAsIni(config_, "conf/smartkey.conf")` (unikey precedent: `UNIKEY/src/unikey-im.h:41-45`). This replaces the desktop CLI for on-device edits; the CLI's `rules CRUD` semantics (`smartkey rules add block ...`) become `rules.toml` file edits — users reach files through the app's **DocumentsProvider** root (`FcitxDataProvider.kt:22, 82, 140-156`; `Intent.kt:36-52`).

### 5.3 Sync strategy `[Observed]` + `[Inferred]`

1. **Built-in backup/restore zip** covers the whole `getExternalFilesDir` tree incl. `config/` and `data/` (`UserDataManager.kt:60-81, 93-110`; `AdvancedSettingsFragment.kt:48-97`) — device migration for free.
2. **Syncthing / file sync:** `rules.toml` is the single shareable artifact. Point Syncthing (or any SAF-sync app) at the plugin's `Android/data/<plugin.id>/files/config/smartkey/` — reachable via the DocumentsProvider or "browse user data dir" (`AdvancedSettingsFragment.kt:101-117`). `[Inferred: Syncthing's Android restriction on Android/data on Android 13+ may force the SAF route or an explicit copy button in a future SmartKey settings pane]`
3. **Schema compat:** `rules.toml` is comment-preserving `toml_edit` on both sides (`store.rs`); `[lang.vi]` scoping already exists, so one file serves Linux EN+VI and Android EN(+VI). Conflicts = last-writer-wins; keep `learner_state.json` device-local (it's a cache, not a config). `[Inferred]`

---

## 6. Build & Packaging Blueprint

**Toolchain facts** `[Observed]`: AGP `9.4.1`, Kotlin `2.4.10`, `compileSdk/targetSdk 36`, **`minSdk 23`**, CMake `3.31.6`, **NDK `28.0.13004108`** (`APP/gradle/libs.versions.toml:2-3`; `APP/build-logic/convention/src/main/kotlin/Versions.kt:10-17`); ABIs `{armeabi-v7a, arm64-v8a, x86, x86_64}`, ABI-coded versionCode, per-ABI APK splits + `buildABI` override (`Versions.kt:23-40`; `NativeAppConventionPlugin.kt:30-40`; `APP/gradle.properties:26`). fcitx5 core on Android = **5.1.22** @ `1e00551` (`CORE/CMakeLists.txt:2`; pin matches `APP/.gitmodules` submodule SHA).

1. **Rust:** `cargo ndk -t arm64-v8a -t x86_64 (emulator; add armeabi-v7a/x86 later) -p 23 -- build --release -p smartkey-ffi` → `libsmartkey_ffi.a` per ABI. `bundled-en` **off** (dict ships as asset); keep `--exclude-libs ALL` at link so Rust/std symbols stay hidden (desktop precedent: `SK/adapters/fcitx5-smartkey/CMakeLists.txt:75-78`). `[Inferred: pin `-p 23`/linker API to minSdk and verify rustc android target support — PoC item]`
2. **Project skeleton:** generate from `plugin-scaffold` (`SCF/build.cfg:1-23`, templates `SCF/data/templates/…`), add `native-app-convention` + `fcitx-component` + `data-descriptor` gradle plugins (aliases pre-registered in `SCF/data/templates/settings.gradle.kts.mustache:30-39`), mirroring `APP/plugin/unikey/build.gradle.kts:1-53`.
3. **fcitx5 prefab gap:** `lib:fcitx5` is **not** published to the `maven.fcitx.workers.dev` repo (only `common`, `plugin-base`, conventions are — `APP/.github/workflows/publish.yml:45-47`; no `maven-publish` in `APP/lib/fcitx5/build.gradle.kts`). Options: (a) vendor `APP/lib/fcitx5` module + its `prebuilt` submodule into the plugin repo and build once into mavenLocal; (b) upstream a publish request. Vendoring is the proven local path (unikey does `implementation(project(":lib:fcitx5"))` in-tree, `unikey/build.gradle.kts:50-53`). `[Observed]`
4. **CMake (plugin native module):** `find_package(fcitx5 REQUIRED CONFIG)` → `Fcitx5Core/Fcitx5Utils` + `add_subdirectory(smartkey-ffi-link)`; custom command runs cargo-ndk per ABI (like desktop `smartkey_ffi_build` target, `SK/.../CMakeLists.txt:29-44`); link `libsmartkey_ffi.a` into `libsmartkey.so` (`-Wl,--exclude-libs,ALL`).
5. **Packaging excludes** (mandatory to avoid STL/fcitx symbol clashes): `packaging.jniLibs.excludes += "**/libc++_shared.so", "**/libFcitx5*"` — unikey precedent (`APP/plugin/unikey/build.gradle.kts:40-47`); app builds with `-DANDROID_STL=c++_shared` (`NativeBaseConventionPlugin.kt:20-39`).
6. **Assets & descriptor:** addon conf (`Category=Module` Phase 0 / `Category=InputMethod` Phase 1, `Type=SharedLibrary`, `Library=libsmartkey`, `OnDemand=False`, `Configurable=True`, `Dependencies: 0=core:@REQUIRED_FCITX_VERSION@` pinned to **5.1.22**); `usr/share/smartkey/dicts/en.skdict`; gradle plugins hash everything into `descriptor.json` (`FcitxComponentPlugin.kt:36-98`; `DataDescriptorPlugin.kt:47-158`).
7. **APK budget:** code `.so` ≈ 0.5–1 MB (Rust static lib + C++ addon); `en.skdict` = **5,143,264 bytes** (`SK/crates/smartkey-core/assets/en_lexicon.skdict`) — loaded at runtime from the merged tree via `smartkey_engine_new_with_lexicon_bytes`, so it compresses in the APK and never enters the `.so`. `[Inferred: a Phase-1 "mobile tier" dict (top ~50k lemmas) could cut this ~4×; evaluate with `smartkey-dev build-lexicon`]`
8. **Signing:** self-signed release keystore; note debug builds of the app use applicationId `org.fcitx.fcitx5.android.debug` and expect plugins with `.debug` suffix + `org.fcitx.fcitx5.android.debug.plugin.MANIFEST` (`PluginDescriptor.kt:40-41`; `APP/lib/plugin-base/src/debug/AndroidManifest.xml:27-33`) — build a debug-matching variant for emulator testing.

---

## 7. Top Risks, Unknowns & Day-1 Prototype Plan

### 7.1 Risks & unknowns

| # | Risk | Severity | Evidence / mitigation |
|---|---|---|---|
| R1 | Self-signed plugin load on official F-Droid/GitHub builds breaks silently on some vendor ROM (package-visibility, installer quirks) | Med | Loading is intent+prefix+apiVersion gated, no signature check (§2.1-2.2) `[Observed]`; PoC on emulator + one real device on release APK |
| R2 | No `SurroundingText` capability; our delete-guard must change; delete+commit not batched → visible flicker/race in Chrome/WebView/Telegram | Med-High | §2.6 `[Observed]`; §2.4 batch-edit absence `[Observed]`; PoC must measure; fallback = filter BS instead of deleteSurrounding (slower but atomic per-key) `[Inferred]` |
| R3 | Swipe-delete bypasses fcitx → learner attributes wrong pairs in EN mode | Med | §2.4 `[Observed]`; bounded by Layer-2 guards (`Delta-Span`, Guards 1-3, `skipped_edits` ring) `[Observed SK docs]`; optionally reconcile from `onUpdateSelection`-driven events later |
| R4 | fcitx5 prefab unpublished + version pin (5.1.22 ≠ desktop 5.0.20 floor) → ABI drift when app updates its submodule | Med | §6.3 `[Observed]`; pin plugin builds to app releases; CI matrix over app tags |
| R5 | unikey-vs-Lotus Telex divergence if Option 1 VI is attempted | Low (deferred) | §3.1 `[Observed]`; Option 2 sidesteps |
| R6 | `FCITX_ADDON_FACTORY_V2_BACKWARDS` symbol resolution under Android loader | Low | loader fallback documented (`androidaddonloader.h:29-56`) `[Observed]`; verify at link time |
| R7 | Android UX gap: no glide typing, no voice typing in fcitx5-android (vs HeliBoard) — user acceptance, not technical | Med (product) | `grep -rli glide` in app = 0 hits `[Observed]`; see sanity table below |
| U1 | Rust `aarch64-linux-android` + `minSdk 23` API-level/linker interaction under NDK 28 | Unknown | PoC item (§6.1) |
| U2 | Unknown-term handling UX without candidate chips (silent boundary fix vs visible undo) | Unknown | PoC: measure BS-undo discoverability in EN mode |

**HeliBoard sanity check** (commit-to-`fcitx5-android` decision):

| Dimension | fcitx5-android | HeliBoard + JNI fork | Winner for SmartKey |
|---|---|---|---|
| Plugin path without forking the IME | ✅ real (§2.1) | ❌ fork required (no plugin API) | fcitx5-android |
| Autocorrect insertion point | engine-level (owns commit) | inside AOSP `InputLogic` (patched fork) | parity, fcitx5 cleaner |
| Glide/voice/clipboard-extras | ❌ glide absent `[Observed]` | ✅ glide, richer extras | HeliBoard (UX) |
| Learner hooks | keystream + `smartkey_mobile_on_*` both viable | only `smartkey_mobile_on_*` (word-level) | fcitx5-android (keystream = full Layer 1+2) |
| Maintenance health | active (HEAD 2026-10-13, F-Droid + GH releases, v0.1.3-dev, org with updater/fdroid-build) `[Observed]` | active | parity |
| Signature/rollout | plugin APK self-signed, official-build compatible | custom APK, users leave official HeliBoard | fcitx5-android |

**No technical dealbreakers found.** The one honest product risk is R7 (glide typists won't switch).

### 7.2 Day-1 PoC (smallest end-to-end proof)

1. **Build app + plugin skeleton (~0.5 day):** clone `fcitx5-android` (full, with `lib/fcitx5` + `prebuilt` submodules), assemble a debug app for the emulator; generate scaffold → apply `native-app-convention`, vendor `lib/fcitx5` → `mavenLocal`; add a trivial Module addon that logs every `InputContextKeyEvent` via `FCITX_INFO`. **Proves:** plugin discovery, `descriptor.json` merge, dlopen, factory symbol (R1, R6).
2. **Wire real `libsmartkey.so` (~1 day):** cargo-ndk the existing `smartkey-ffi`; port `applyDeleteAndCommit` without the SurroundingText guard; inject paths from `XDG_CONFIG_HOME`. Install on official **release** APK on a real device. **Proves:** EN boundary replacement (e.g. `teh → the`) in Chrome + Telegram + Termux(bypass via `app_profiles`); `rules.toml` created at `{extData}/config/smartkey/` and persisting across app restart + reboot (U1, R2 flicker observation).
3. **Sync probe (~2 h):** export the built-in backup zip, confirm `rules.toml` inside; hand-place a desktop `rules.toml` via DocumentsProvider and confirm `smartkey rules`-style edits load after addon reload. **Proves:** §5.3 loop.
4. **Gate to Phase 1:** flicker verdict in Chrome/WebView (R2) + learner desync frequency from swipe-delete (R3) decide whether Phase 1's EN mode also moves to visible preedit.

---

*Raw scout notes (three agent reports with full citation chains) were consolidated into this document; upstream clones remain in `/tmp/smartkey-android-scout/` for re-verification.*
