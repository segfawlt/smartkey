# Scouting Report: Porting SmartKey to Windows via TSF

> **Date:** 2026-11-23 · **Method:** read-only recon. Local repo audited directly; upstream inspected from live Microsoft docs, GitHub API, and shallow clones in `/tmp/unikey-scout-20261123/` (ephemeral; re-clone before quoting further).
> **Pins inspected:** `windows` crate docs (current release), `chewing/windows-chewing-tsf` @ HEAD, `fcitx/fcitx5-unikey` @ HEAD, `hochanh/unikey-source` @ HEAD, `xmirror/unikey` @ HEAD, UniKey 4.6 RC2 win64 binary (unikey.org, 2023-09-19) + 4.0 RC2 win32 binary (SourceForge), `tuyenvm/OpenKey` @ master (pushed 2026-06-15).
> **Citation abbreviations:** `MS` = learn.microsoft.com, `WCRS` = microsoft.github.io/windows-docs-rs, `UK` = fcitx5-unikey clone, `UKSRC` = legacy unikey-source clone, `XUK` = x-unikey mirror, `OK` = tuyenvm/OpenKey, `SK` = this repo.
> **Premise corrections:** UniKey for Windows is **not** TSF-based; it has used global keyboard hooks since 3.x (1998) and no TSF source ever shipped. `TF_INPUTSCOPE` / `IS_INPUTSCOPE_SECURE` **do not exist**: the enum is `InputScope` (inputscope.h) with `IS_`-prefixed members; secure fields are handled via `IS_PRIVATE` / `IS_NUMERIC_PASSWORD` / `IS_ALPHANUMERIC_PIN*` plus TSF secure mode (`TF_TMF_SECUREMODE`).

---

## 1. Executive Verdict & Recommended Architecture

**Go. TSF text service (TIP) written in Rust on the `windows` crate, statically linking `smartkey-ffi`, is feasible and is the only architecture that satisfies the zero-false-positive guardrails.**

- **Mechanism:** one DLL registers as a TSF input processor. It implements `ITfKeyEventSink` (`OnTestKeyDown` / `OnKeyDown`), passes every key through with `pfEaten = FALSE` unless a correction fires, and applies corrections with a synchronous `TF_ES_READWRITE` edit session + `ITfRange::SetText`. No composition/preedit machinery is needed because corrected text is final, never in-progress.
- **Phasing:** ship the TIP under the **en-US profile first** (English autocorrect; Vietnamese users keep UniKey). A second profile under **vi-VN** turns SmartKey into a full UniKey replacement using the already-existing `SK` `vi_telex.rs` engine. One DLL, both profiles; activation order is a product decision, not a technical fork.
- **Why not hooks:** `WH_KEYBOARD_LL` + `SendInput` (the OpenKey/EkaKey path) has no field metadata, races fast typists, and cannot see password fields. `SK` already documented this as a Tier-C anti-pattern (`SK` `docs/ekakey-core-learnings.md:191`).
- **Rust precedent exists:** `chewing/windows-chewing-tsf` (251 stars) is a complete Rust TSF IME; `dtwinuz/bitg` is the closest analog (Rust core + TSF shell + Linux/macOS backends), matching `SK`'s multi-platform shape.
- **Hard external gate:** third-party IMEs must be **digitally signed** on Windows 8+; unsigned DLLs get a critical install warning and SmartScreen friction (`MS` Windows 8 Compatibility Cookbook, "Third-party input method editors").

---

## 2. Windows Input Stack & Event Flow Map

### 2.1 TSF pipeline: key sink → edit session `[Observed]`

- A TIP is a COM DLL; TSF instantiates it with `CoCreateInstance` and calls `ITfTextInputProcessor::Activate`, where the service advises its event sinks. (`MS` `ITfTextInputProcessor::Activate`)
- Keystrokes arrive via `ITfKeyEventSink`, installed with `ITfKeystrokeMgr::AdviseKeyEventSink`. Methods: `OnTestKeyDown`, `OnKeyDown`, `OnTestKeyUp`, `OnKeyUp`, `OnPreservedKey`, `OnSetFocus`. (`MS` `ITfKeyEventSink`)
- `OnTestKeyDown`'s `pfEaten` decides the route: `TRUE` = the TIP claims the key, the app never sees it; `FALSE` = pass-through. `OnKeyDown` uses the same contract for the actual event. This is the entire interception surface SmartKey needs.
- Text mutation requires an edit session: `ITfContext::RequestEditSession` with `TF_ES_READWRITE`. `TF_ES_SYNC` is documented as appropriate for keystroke handling. Failures: `TF_E_LOCKED`, `TS_E_READONLY`, `TF_E_SYNCHRONOUS`; synchronous requests fail while `OnEndEdit` / `OnLayoutChange` / `OnStatusChange` notifications are in flight. (`MS` "Edit sessions", `ITfContext::RequestEditSession`)
- Correction = `TF_ES_SYNC | TF_ES_READWRITE` session + `ITfRange` covering the mistyped word + `ITfRange::SetText` ("replaces the content covered by the range"). Deletion of the typed word is implicit in `SetText`; no synthetic `BackSpace` echo is required, which is simpler than the Linux adapter's echo/burst machinery.
- `VK_PACKET` caveat: keys injected via `SendInput` Unicode mode (and the touch keyboard) arrive at the sink as virtual key `VK_PACKET`, not the real character. The adapter must resolve `VK_PACKET` to a codepoint itself. (`MS` "Input Method Editor requirements", section "IME and touch keyboard")
- Legacy coverage: non-TSF apps are empowered through the CUAS/AIMM compatibility layer since XP SP1 (legacy accessibility doc snippet). **No document covers raw-input games or other TSF-bypassing apps**; treat them as uncovered (`NOT FOUND`).
- Mapping to `SK`: the FFI contract is already exactly this shape. `smartkey_session_process_key(session, engine, key_kind, codepoint, now_ms, out_action)` returns a `SmartKeyAction` (`pass_through` or `replace` with `delete_chars` + `insert_utf8`) (`SK` `crates/smartkey-ffi/include/smartkey.h:151,63-68`). The adapter translates `SmartKeyAction::replace` into one edit session.

### 2.2 Input scopes & secure mode `[Observed]`

- App-declared field type is read through `ITfContext::GetAppProperty(GUID_PROP_INPUTSCOPE)` → `ITfReadOnlyProperty::GetValue` → `ITfInputScope::GetInputScopes` (caller must `CoTaskMemFree` the returned array). (`MS` `ITfInputScope`)
- Password-family scopes: `IS_PRIVATE = 61`, `IS_NUMERIC_PASSWORD = 63`, `IS_NUMERIC_PIN = 64`, `IS_ALPHANUMERIC_PIN = 65`, `IS_ALPHANUMERIC_PIN_SET = 66`. `IS_PASSWORD = 31` exists but is deprecated ("All passwords fields should have text services disabled... it is not valid to have a password field with an IS_PASSWORD input scope"). (`MS` `InputScope` enum)
- System secure mode: `ITfThreadMgrEx::GetActiveFlags` returns `TF_TMF_SECUREMODE` (and `TF_TMF_IMMERSIVEMODE` for UWP since Win 8). A TIP declares the `GUID_TFCAT_TIPCAP_SECUREMODE` category and `TF_IPP_CAPS_SECUREMODESUPPORT` capability. (`MS` `ITfThreadMgrEx::GetActiveFlags`, `WCRS` TextServices index)
- Mapping to `SK`: any password-family scope, `TF_TMF_SECUREMODE`, or missing scope → `smartkey_session_set_profile(session, 0 /* bypass */)`. This satisfies the guardrail EkaKey failed (`SK` `docs/competitor-analysis.md:71`).
- Precedent for opt-in autocorrect interplay: touch-keyboard property `GUID_PROP_TKB_ALTERNATES` with `TKB_ALTERNATES_FOR_AUTOCORRECTION = 2` shows Microsoft routes autocorrection through `ITfFnReconversion` in edit controls. Not required for our path; noted as future polish.

### 2.3 Deployment, signing & tray `[Observed]`

- Registration: `ITfInputProcessorProfiles::Register` adds the service to TSF; profile registration via `ITfInputProcessorProfileMgr::RegisterProfile` — "Don't write registry entries directly." The `InstallLayoutOrTip` string form is `<LangID>:{profile-guid}{clsid}`. (`MS` IME requirements)
- Activation is exclusive per language: `ITfInputProcessorProfiles::ActivateLanguageProfile` "Sets the active text service for a specific language." Users pick the active IME in Settings; `TF_PROFILETYPE_INPUTPROCESSOR` (us) and `TF_PROFILETYPE_KEYBOARDLAYOUT` (base QWERTY) are both profile types. Pass-through `pfEaten = FALSE` means plain English typing is untouched unless a correction fires.
- **Signing is mandatory in practice:** "Third-party IMEs must be digitally signed"; unsigned IMEs produce a critical warning and SmartScreen friction; obtain an Authenticode cert and sign with `signtool.exe`. (`MS` Windows 8 Compatibility Cookbook, "Third-party input method editors")
- The DLL loads **in-process into every TSF app**: "When an app is started, TSF loads the IME .dll for the IME that the user has selected into the app process." Per-architecture builds (x64, ARM64) are implied; explicit matrix line `NOT FOUND`. Per-user vs machine registration has flags `TF_IPPMF_FORPROCESS` / `TF_IPPMF_FORSESSION` / `TF_IPPMF_FORSYSTEMALL` (`WCRS`).
- UWP/app-container: an IME loaded into a Store app inherits the app container's restrictions — affects dictionary files and on-the-fly learning. Detect via `TF_TMF_IMMERSIVEMODE` and degrade to in-memory state. (`MS` Compatibility Cookbook, "IME and Windows Store apps")
- Tray UX (Win 8+): no language bar; the Input Indicator shows instead. Register `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT` for proper branding/mode icons.

### 2.4 Rust TSF precedent `[Observed]`

- The `windows` crate exposes the full `Win32::UI::TextServices` module: `ITfKeyEventSink` (+ `_Impl` vtables), `ITfKeystrokeMgr`, `ITfTextInputProcessor`, `ITfEditSession`, `ITfContext`, `ITfRange`, `ITfInputProcessorProfiles` / `...ProfileMgr`, `ITfInputScope/2`, `ITfThreadMgrEx`, `ITextStoreACP`, plus all `TF_ES_*` / `TF_TMF_*` / `GUID_TFCAT_TIPCAP_*` constants. (`WCRS` TextServices index)
- Existing Rust TSF IMEs on GitHub (measured 2026-11-23):
  - `chewing/windows-chewing-tsf` — 251 stars, complete Rust TIP (`tip/src/text_service` implements `ITfTextInputProcessor` + `ITfKeyEventSink`). **Primary skeleton reference.** License to verify before vendoring.
  - `dtwinuz/bitg` — Rust core + Windows TSF + Linux fcitx5/IBus + macOS IMK; the closest architectural analog to SmartKey.
  - `ironsand/hangul-only-ime`, `RagibHasin/uo-keyboard`, `hronro/ime-jd`, `qingjian-team/qingjian`, `kiyoka/Sumibi-win`; `meanie-py/vibeos` ships an `msctf-rust` binding crate.
  - Code-search volume: `AdviseKeyEventSink` 112 Rust hits, `ITfKeyEventSink_Impl` 92 — the pattern is well-trodden.
- Official samples: `microsoft/Windows-classic-samples` `Samples/IME/cpp/SampleIME` (`KeyEventSink.cpp`, `EditSession.cpp`, `Composition.cpp`) and the Win7 `tsf/textservice` step-by-step series (textservice-step01…06-3, uilessmode).

---

## 3. Vietnamese IME Landscape & Coexistence (N-Way Comparison)

### 3.1 Existing IME ground truth `[Observed]`

| IME | license | integration | learning/autocorrect |
|---|---|---|---|
| UniKey 4.6 RC2 (2023) | engine LGPL-2.0+ (`UK/unikey/ukengine.cpp:1-4`), hook code GPL-2.0+ (`UKSRC/src-win/keyhook/keyhook.cpp:3-15`) | global `SetWindowsHookEx` hooks; zero TSF (binary `strings`: `SetWindowsHookExA` present, no `ITf`/`msctf`) | no |
| EvKey 6.0.5 (2018) | none; closed freeware (`github.com/lamquangminh/EVKey`: `license: None`) | user-selectable "Keyboard hook hoặc IME" (evkeyvn.com); IME-mode internals `unverified` | spellcheck + auto-restore keys; no learning |
| OpenKey | GPL-3.0 (GitHub API) | `WH_KEYBOARD_LL` + `SendInput` (`OK Sources/OpenKey/win32/OpenKey/OpenKey.cpp:167,201-298`); no `msctf` in win32 tree | "restore key if invalid word"; no learning |
| Built-in Telex/VNI | proprietary | TSF IME shipped since Win10 1903 (Insider build 18267, 2018-10-24) | no (MS doc lists none) |
| EkaKey | MIT | `WH_KEYBOARD_LL` + `SendInput` | SQLite pairs; no valid-word guard, no password shield |

- **UniKey is hook-based, never TSF.** Timeline from source and binaries: 3.x (1998-2001) `WH_KEYBOARD` + `WH_MOUSE` via `keyhook.dll` (`UKSRC/src-win/newkey/mainwnd.cpp:472-473`); 4.0 RC2 (2009) hook proc in `UKHook40.dll`; 4.6 RC2 (2023) monolithic `UniKeyNT.exe`. No 4.x Windows source is published (unikey.org ships binaries only; SourceForge CVS snapshot stops at the 3.x era). libunikey engine internals live in `UK/unikey/` (`ukengine.cpp` 96.9K, `inputproc.cpp`, `charset.cpp`, `mactab.cpp`).
- **Engine internals worth studying for the vi profile** (`UK/unikey/ukengine.cpp`): per-key dispatch `UkEngine::process` (:2326) via function-pointer table (:835-854); spell-check gate :1564-1566 (`spellCheckEnabled && !freeMarking && !info.complete` → append); word-end pipeline `processWordEnd` :2908 (macro → spell-check → auto-restore); keystroke replay on BackSpace `restoreKeyStrokes` :2813; macro expansion `macroMatch` :2683 with case-folding :2753-2779. `UkInputMethod` is an enum (`UK/unikey/keycons.h:20-26`): Telex, VNI, Viqr, MsVi, UsrIM, SimpleTelex, SimpleTelex2.
- **Market share: UNVERIFIED.** No survey, poll, or download metric ranking the four was found (searched EN+VN, 2026-11-23). Evidence is qualitative only: unikey.org claims "phổ biến nhất trên Windows"; VNReview calls UniKey "bộ gõ quốc dân"; OpenKey has the largest open-source star count (982); SourceForge shows 461 downloads/week for the legacy mirror. The working assumption "UniKey dominant, EvKey second" stands but is anecdotal. EvKey development reportedly stopped 2025-03 (VNReview, anecdotal).
- **Field awareness:** none of the four documents password-field handling; OpenKey runs as Administrator for games and advertises "không keylog" (`OK` win32 README); EvKey markets game safety copy only. Hook fragility is corroborated by RustDesk 1.4.6 breaking EVKey (`github.com/rustdesk/rustdesk/issues/14667`).

### 3.2 Options compared

| **#** | **option** | **tradeoff** |
|---|---|---|
| **A** | TIP under en-US profile only; UniKey stays for vi | smallest scope; empty niche (English autocorrect); zero conflict with UniKey; needs user education: exit/disable UniKey in English mode (hooks compete) |
| **B** | TIP under vi-VN profile | full UniKey replacement; `vi_telex.rs` already in `SK`; must match UniKey UX (macros, Simple Telex, restore keys) or users churn back |
| **C** | `WH_KEYBOARD_LL` hook like OpenKey | rejected: no field metadata, races typists, guardrail violation (`SK` `docs/ekakey-core-learnings.md:191`) |
| **D** | fork OpenKey/UniKey | OpenKey is hook-based (wrong architecture); UniKey 4.x Windows source unpublished; libunikey embeddable but LGPL + 96.9K monolith, no C ABI |
| **E** | A + B phased, one DLL, two profiles | A's scope with B's endgame; en-US gates the hard TSF work, vi-VN reuses it |

### 3.3 Recommendation

**Recommend: E.**
- **Why:** TSF allows exactly one active text service per language (`MS` `ActivateLanguageProfile`), so coexistence with UniKey is impossible for vi and replacement is the only path there; but English autocorrect is an unoccupied niche (EkaKey is the only open entrant and is documented broken) and needs none of the Telex UX parity. Phasing de-risks: en-US proves the TSF skeleton, signing, and the learner loop; vi-VN then swaps `process_key` input from QWERTY chars to Telex events handled by `vi_telex.rs` (already carries the Lotus-dict authority and passthrough logic in `SK` `crates/smartkey-core`).
- **Clear call:** engine reuse from UniKey stays at "study behavior, re-implement in Rust" — libunikey is LGPL-2.0+ so vendoring is license-legal for a GPL-3.0+ host, but it has no C ABI and a Rust port of the behavior tables keeps the dependency surface empty (`SK` core strategy, `docs/architecture-and-plan.md:6`).

### 3.4 Coexistence mechanics `[Observed]`

- Our TIP and the base QWERTY layout are distinct profile types; both can be installed for en-US, and the user's `pfEaten = FALSE` pass-through preserves normal typing.
- UniKey/EvKey/OpenKey are **outside** TSF: a global `WH_KEYBOARD_LL` hook sees keys regardless of the active TIP. If UniKey runs with Telex on while our en-US TIP is active, UniKey transforms and re-injects keys (possibly as `VK_PACKET`) before us — the two engines corrupt each other's buffers. Community practice already treats Vietnamese IMEs as mutually exclusive (guides remove the built-in and exit UniKey; `OK` README warns to shut off other IMEs).
- Consequence for the en-US phase: SmartKey's docs must require UniKey/EvKey to be off (or in English-only macro mode) while SmartKey's profile is active. The vi-VN phase removes the conflict by replacing them.

---

## 4. What Transfers As-Is vs. What Must Change

### 4.1 `crates/smartkey-core` — transfers 100% `[Observed]`

- "100% pure, synchronous, single-threaded Rust with zero OS, clock, thread, `$HOME`, or `-sys` C library dependencies" (`SK` `docs/architecture-and-plan.md:13`) — compiles for `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` unchanged.
- The shadow-buffer design ("working identically across Wayland/X11/Android without relying on flaky `surroundingText`", `SK` `docs/architecture-and-plan.md:64`) is the key Windows simplification: the TIP never needs to read the document back. It only writes the replacement range. Undo-on-BackSpace works through the key sink (the TIP sees the app's natural BackSpace and drives `Mode::JustAutocorrected` restore via `SetText`).
- `vi_telex.rs` (Telex simulator + typo rescue, runtime Lotus dict authority) ships in core today; on Windows it graduates from Linux-lotus-auxiliary to the actual vi engine in profile B.

### 4.2 `crates/smartkey-ffi` — transfers 100% `[Observed]`

- `extern "C"` ABI (`SK` `crates/smartkey-ffi/include/smartkey.h`), `catch_unwind`-guarded entry points per Milestone 3 (`SK` `docs/architecture-and-plan.md:83`); builds as a staticlib linked into the TIP cdylib. The whole 30-export surface is host-agnostic; the `smartkey_mobile_*` word-level learner APIs (`smartkey.h:215-235`) are available if profile B later embeds differently.
- Per-architecture static libs: `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc`.

### 4.3 New `adapters/windows-smartkey` — everything new `[Inferred]`

- COM class object + `ITfTextInputProcessor`/`ITfTextInputProcessorEx` (`Activate`/`Deactivate`).
- `ITfKeyEventSink` implementation: `OnTestKeyDown` fast-path pass-through, `OnKeyDown` → map vkey+scan to `SMARTKEY_KEY_*` / codepoint (`VK_PACKET` resolution), call `smartkey_session_process_key`, act on `SmartKeyAction`.
- `ITfEditSession` implementation performing the `replace` via `ITfRange::SetText`.
- InputScope polling per focus change (`ITfThreadMgrEventSink::OnSetFocus` + `GetAppProperty`) → `smartkey_session_set_profile`.
- App identity for `smartkey_engine_classify_app` / `set_app`: foreground HWND → `GetWindowThreadProcessId` → `QueryFullProcessImageName` → exe basename. (`Inferred` — standard Win32 plumbing, no TSF doc line.)
- Registration helper (`ITfInputProcessorProfileMgr::RegisterProfile` at install/first run; `UnregisterProfile` at uninstall), tray branding via `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT`.
- Precedent skeleton: `chewing/windows-chewing-tsf` `tip/` (verify its license before copying), plus `SampleIME` for the C++ ground truth.

### 4.4 What does NOT transfer `[Observed]`

- `fcitx5` adapter entirely: `Uinput` echo layer, lotus burst/echo protocol, `FcitxKey_Pause`, `pendingPostBoundaryChars`, Wayland/X11 plumbing (the Android scout already classified these dead outside Linux; `SK` `docs/scouting-android-fcitx5.md:169`).
- Linux deploy path (`install-local.sh`, DBus restart, Fcitx5 `StandardPaths`).

---

## 5. Storage, Config & Linux ↔ Windows Sync

- **Paths (proposed, XDG-analog):** rules `…\AppData\Roaming\smartkey\rules.toml`, learner state + event log `…\AppData\Local\smartkey\state\`. The `toml_edit` comment-preserving store and the JSONL event logger are path-agnostic already (`SK` `crates/smartkey-core/src/store.rs`, `event_log`).
- **Sync:** `rules.toml` stays a plain Git/Syncthing-friendly text file; cross-machine sync works unchanged (Android plan §5.3 strategy applies verbatim).
- **Store-app caveat:** inside app containers, file writes may be blocked; detect `TF_TMF_IMMERSIVEMODE` and run with in-memory session state (learning disabled) rather than failing.
- **CLI:** `smartkeyctl` builds on Windows with the same flags (`status`, `reload`, `toggle`, diagnostics). Hot-reload semantics (mtime stat + `reload_if_modified`) are OS-neutral; only the path resolution needs a Windows branch.

---

## 6. Build & Packaging Blueprint

- **Targets:** `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc` (UniKey shipped ARM64 in 2025-06 — the Snapdragon cohort exists; unikey.org timeline).
- **Artifacts:** `smartkey-ffi` → staticlib; TIP crate → `cdylib` (`smartkey_tsf.dll`) statically embedding core+ffi; `smartkeyctl.exe`.
- **Install:** portable zip + `smartkeyctl install` (calls `RegisterProfile` per docs — no raw registry writes); self-sign during dev (`signtool` with test cert), EV/Authenticode cert for release. Unsigned release builds carry the critical-warning + SmartScreen penalty.
- **CI:** GitHub Actions `windows-latest` job: build both targets, run `cargo test --workspace` (core tests are host-neutral), boot a VM for register/activate smoke if available. Linux dev box cannot run a TSF DLL — every "works" claim on this port needs a Windows VM or physical machine (same lesson as the Android PoC).
- **Version parity:** extend `SK` `scripts/check-versions.sh` to cover the TIP crate + installer manifest.

---

## 7. Top Risks, Unknowns & Day-1 Prototype Plan

### 7.1 Risks & unknowns

1. **Signing gate (hard, external):** EV/Authenticode cert costs money and identity verification; unsigned DLLs = critical warning + SmartScreen. Blocks distribution, not development.
2. **In-process blast radius:** our DLL loads into every TSF app; a crash takes the app down. Mitigate: `panic = "abort"` boundaries, `catch_unwind` at every FFI entry (already in `smartkey-ffi`), no allocation across COM boundaries, fuzz the key path (Tier B `MockInputContext` analog: a TSF mock harness).
3. **Uncovered apps:** console windows, raw-input games, elevated apps (UIPI) may never route keys through TSF. Zero-FP policy: no coverage is safe coverage; document as scope, mirror `unknown_app_policy` bypass.
4. **UniKey conflict window (en-US phase):** global hooks fight the TIP. Mitigation: doc requirement + a detection hint (foreground app is UniKey.exe → log, never correct).
5. **vi-VN UX parity:** users expect macros, Simple Telex, restore keys, per-app memory (UniKey 4.6 features). Deferring any of these churns users back to UniKey. `unverified` market-share premise adds risk to betting the roadmap on vi-VN.
6. **`VK_PACKET` / touch keyboard:** Unicode-injected keys need codepoint resolution; also affects our own pass-through correctness in Chromium's TSF-on-by-default stack (Chromium issue 40489775 confirms TSF adoption).
7. **No Windows host in the dev loop:** cannot run, register, or observe a TIP from this Linux box; all live verification needs a VM. *This is the same class of risk the Android scout flagged; plan the VM before writing code.*
8. **Open questions for round 2:** preserve-key handling (`TF_PRESERVEDKEY`) vs plain sink; `ITfFnReconversion` interplay with touch-keyboard autocorrect; whether `TF_IPPMF_FORPROCESS` registration suffices for a no-installer portable mode; EvKey IME-mode internals (TSF or IMM32); built-in vi IME DLL internals.

### 7.2 Day-1 PoC (smallest end-to-end proof)

1. Shallow-clone `chewing/windows-chewing-tsf` into a fresh `/tmp` dir; read `tip/src/text_service`; verify its license before any copy.
2. Strip the Chewing engine; keep the TSF skeleton (class object, `Activate`, key sink, edit session, registration).
3. Link `smartkey-ffi` staticlib; wire `OnTestKeyDown` = pass-through, `OnKeyDown` = `smartkey_session_process_key` for chars / `BackSpace` / `Enter`; on `SmartKeyAction::replace` run the `TF_ES_SYNC | TF_ES_READWRITE` session with `ITfRange::SetText`.
4. Password gate: on `OnSetFocus`, read `GUID_PROP_INPUTSCOPE`; any `IS_PRIVATE` / `IS_*PIN*` / `IS_NUMERIC_PASSWORD` or `TF_TMF_SECUREMODE` → `set_profile(0)` (bypass).
5. Register under en-US via `RegisterProfile`; test matrix: Notepad (correct `fomr␣` → `form␣`), Chromium address bar (bypass), VS Code, Word, elevated Notepad, one UWP app (degraded mode), touch keyboard (`VK_PACKET` path).
6. **Gate to call it proven:** correction fires in a rich-edit app AND zero effects in the password field AND the app survives 30 min of typing with the DLL loaded.
