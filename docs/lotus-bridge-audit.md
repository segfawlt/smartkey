# Audit: SmartKey ↔ fcitx5-lotus Bridge Integration

**Date:** 2026-11-10
**Scope:** Problems visible when SmartKey works alongside fcitx5-lotus: the Pre/PostInputMethod bridge lifecycle, the Rust Telex simulation (`vi_telex.rs`), lotus config/protocol assumptions, vi-scoped learning state, and test coverage of the bridge.
**Repo state at audit:** `v0.1.1-5-gbe2c346-dirty`
**Machine state (verified live):** fcitx5 5.1.23, **fcitx5-lotus 6.0.7 installed and active** (`/usr/lib/fcitx5/liblotus.so`), lotus config `Mode="Uinput (Super Smooth)"`, `W2U=Everywhere`, `EnableMacro=True`, real lotus source at `/usr/src/debug/fcitx5-lotus/fcitx5-lotus/`.
**Companion docs:** `docs/cli-audit.md`, `docs/system-integration-audit.md` (prior findings referenced but not re-reported).
**Status:** **Findings only — no fixes applied, no design decisions made.**

Severity legend: **P1** = wrong behavior in realistic typing scenarios · **P2** = robustness/assumption gap · **P3** = polish/tradeoff.

---

## 0. Executive summary

The Telex simulation itself is a genuine 1:1 port of lotus's engine — the composition core (Transformation/flatten model, double-key undo, dd-free-style, tone retargeting) shows **no structural divergence**. The English-restore path for lotus tone-undo truncations works end-to-end, casing is preserved, and IM switching (us → lotus → bamboo mid-word) is handled safely.

The problems are in the **host contract**: the bridge's evaluation pipeline silently never runs for exactly the typo class it was built to fix (lotus-filtered boundaries), a promised feature (`telex_passthrough`) is dead code wired to nothing, the echo-suppression window is tighter than lotus's own tolerances, and the test harness encodes the same forwarding assumption production falsifies — which is why none of this ever failed a test.

---

## 1. Bridge lifecycle & phase logic

### Done correctly

- Pre/Post phase split matches fcitx5 5.1.23 dispatch exactly: **PostInputMethod watchers do not run when the IM filters the key** (verified from upstream source), and the comment at smartkey-addon.cpp:474 ("reaching PostInputMethod proves LotusState::keyEvent did NOT filter") is accurate.
- `Pause` sentinel matches lotus-server's real burst-terminator protocol (lotus-state.cpp:563–577); the echo-suppression key set matches lotus's synthetic set incl. Select-mode `Shift+Left`.
- Letters are recorded in Pre so lotus-consumed letter keys (mid-word replacement bursts, lotus-state.cpp:825–827) are never lost; lotus-server injects only backspaces + Pause + commitString, so no duplicate-letter contamination — the design correctly anticipated this.
- Per-IC state; burst state reset on focus/reset/IM-switch; terminals/password/URL bypassed in both phases. lotus disabled/uninstalled or Alt+Z switching mid-word is safe (`InputContextSwitchInputMethod`/FocusOut/Reset all reset the context, smartkey-addon.cpp:95–109, 143–154).

### Findings

**[P1] The rescue never fires for the uinput-restore typo class — the boundary key is consumed by lotus exactly when a word needs rescuing.**
When the commit differs from lotus's `oldPreBuffer_` by a *deletion* (runic preedit on screen at break), real lotus runs `performReplacement` + `keyEvent.filterAndAccept()` (lotus-state.cpp:754–760) — PostInputMethod never runs, so SmartKey never evaluates the word. The only recovery is the *next* key's `takeLotusFilteredBoundary()` → `SMARTKEY_KEY_NAV_OR_CLICK` (smartkey-addon.cpp:383–388), and `NavOrClick` in the session **drops the buffered word with no evaluation, no staging, no context update** (session.rs:657–674). The words hitting this path are precisely those lotus renders with runes then restores (key_processor.cpp:233–239, `restoreLastWord`). Scenario: type `duongx ` in Firefox — lotus composes runic preedit, the commit isn't a dict word, lotus deletes it and retypes `duongx`; `rescue_telex_typo("duongx") → "dương"` *would* work, but Post never fires and the screen stays `duongx `. Same for macro expansions (key_processor.cpp:225–231). The session even computes `needs_uinput_restore_on_break` (session.rs:1512) but only to *step aside* — there is no deferred-evaluation fallback.

**[P1] The 150 ms echo window is tighter than lotus's own burst tolerances; expiry corrupts the composing buffer.**
SmartKey arms the window on *every* key (smartkey-addon.cpp:410, 421) and expects echo BackSpaces within 150 ms (smartkey-addon.h:152–156). Real lotus bursts are `preDelay(≤20ms) + count×5ms + postDelay(10ms)` per echo group, but each echo is a kernel→compositor→app→fcitx round trip; lotus itself budgets `kReplacementAckSlackMs = 200` plus a re-arming deadline (lotus-state.cpp:35, 570–571, 618). If one echo gap exceeds 150 ms, SmartKey treats it as a user BackSpace (Pre sets `lotusFilteredBoundary_`), lotus consumes the echo, Post never fires, and the next real letter triggers the `NAV_OR_CLICK` flush — wiping the buffered word (e.g. losing `ddd` mid-`dddi`). Result: lost rescue, lost bigram context, diverging screen/session buffers.

**[P2] Burst "tracking" is a timestamp proxy, not real burst state.**
`lotusBurstDeadlineMs_` has no correlation with lotus's actual `g_isDeleting`; any key within 150 ms makes SmartKey swallow `BackSpace/Left/Delete`, and a *physical* `Pause` press clears the window mid-burst (smartkey-addon.cpp:366–369), letting subsequent echoes masquerade as user edits. The design cannot distinguish "lotus is bursting" from "user typed recently".

**[P2] Boundary-set drift against lotus's own word-break definition.**
Lotus breaks words at punctuation ∪ digits (lotus/utils.cpp:135–137; vowel_data.cpp:17–20). SmartKey's `isSmartKeyBoundaryChar` (smartkey-addon.h:130–135) omits `' - @ / _ ~ < > = + * & ^ $ % |` and digits; the session's `is_immediate_boundary` (session.rs:587–592) is smaller still. `well-known ` becomes one token to the learner and two to lotus — `prev_word`/bigram learning and `PendingTrailingDot` lookahead diverge on every hyphenated/apostrophized token. Mostly benign today (REPLACE only fires on rescue/shorthand/user-rule matches), but systematic.

**[P3] Enter never mutates** (session.rs:679–691) — a mistyped last word before Enter in a chat app is never rescued; interacts with lotus's "Send on Enter" chat flow. Escape resets consistently. lotus falls back to internal Off/Default modes that forward keys — SmartKey keeps running its vi pipeline (see §2 corruption finding).

## 2. Rust vi_telex logic

### Done correctly

- The sim is a genuine 1:1 port of lotus's engine: same Transformation/flatten model, double-key undo (`MarkTransformation(Raw)` vi_telex.rs:1072–1090 ≙ lotus's appending-key hack key_processor.cpp:195–220), `apply_uow_shortcut`, `refresh_last_tone_target`, ddFreeStyle check (vi_telex.rs:1203–1207 ≙ key_processor.cpp:511–513). No structural (non-config) divergence found in the composition core.
- English-restore for lotus tone-undo truncations works end-to-end (`off`/`issue` → screen `of`/`isue` → restored, session.rs:268–275; covered by vi_lotus_eval.rs:160–168).
- Casing preserved incl. ALL-CAPS and shift-slips; URL/email/code exposure mitigated by trailing-dot/semicolon lookahead merging (`vn.com ` survives), capability bypasses, backtick markdown immunity.
- Tone-less Vietnamese is never "fixed" into toned Vietnamese (toneless candidates fail `is_composed_vietnamese`) — correct by design; `khoong → không` (forgot `s`) is a miss, not a corruption.

### Findings

**[P1] `telex_passthrough` is unreachable in production — the promised `of → ò` / `it is → it í` protection does not exist.**
The only writer of the passthrough list is `smartkey_session_record_telex_passthrough_repair` (crates/smartkey-ffi/src/lib.rs:414–433 → session.rs:1827–1833 → learner.rs:701–720, "graduates at 2x"). **Nothing in the addon, CLI, or session flow ever calls it** — the only caller in the entire workspace is the FFI smoke test (c_abi_smoke.rs:190–191). The shipped/default list is empty (store.rs:48). Meanwhile `evaluate_vi_lotus_token` step 2 (session.rs:226–246) returns `Unchanged::ValidWord` for any token whose *simulated lotus commit* is a Vietnamese dict word **before** the English-lexicon shield at step 3 is consulted — so `of ` stays `ò ` (`ò` is in the vendored dict), `is → í` persists. The "English-Sentence Momentum Shield" promised in docs/competitor-analysis.md:16,187 has no code (no `momentum` symbol anywhere in crates/). There is no English-lexicon cross-check for this class; protection is purely the (dead) word list.

**[P1, conditional] If ever populated, the passthrough REPLACE corrupts text whenever lotus is *not* composing.**
The passthrough branch replaces with `delete = sim.screen_chars` (session.rs:230–237, 1568–1572) — valid only when lotus actually put the composed runes on screen. In lotus Off/Default mode (keys forwarded unfiltered, lotus-state.cpp:1319–1325; modes hot-switchable at runtime) the screen holds the raw ASCII (`of` = 2 chars) while `sim("of").screen_chars = 1` (`ò`) → delete 1, insert `of` → `oof`. SmartKey has no way to know lotus's current mode.

**[P1] W2U divergence: the sim models `NonStart`, this machine runs `Everywhere`.**
SmartKey only maps `w→ư` when composition is non-empty (vi_telex.rs:1175–1179 — the "W2U non-start / everywhere support" comment is wrong for initial `w`). Real lotus with `W2U=Everywhere`: `canApplyW2U` true even on empty composition (lotus/engine.cpp:163; initial-`w` handling in rebuild.cpp:151–155 applies only to NonStart). Scenario: `wa ` / `w ` under lotus → screen `ưa` / `ư` (valid dict words, kept); the sim says `wa`/`w` → `committed_at_break`, so `screen_chars` and `prev_word` diverge, and any later rescue/delete targeting that word uses wrong counts.

**[P2] Config-dependent lotus options are hardcoded in the sim.**
`std_style=false` (vi_telex.rs:913) matches default `ModernStyle=True`; free-marking tone targeting matches default `FreeMarking=True` — but lotus reads both from config (lotus-state.cpp:73–92) while the sim hardcodes them. `BracketTransform` (`[→ơ`, `]→ư`, engine.cpp:145–159) is not modeled at all. Users who flip any of these get silently divergent preedit/committed strings → wrong `screen_chars` → wrong deletes or missed rescues.

**[P2] Sim ignores lotus's macro awareness.** `shouldFallbackToEnglish` returns false for macro triggers (key_processor.cpp:505–509) and macro expansion consumes the break key (key_processor.cpp:225–231). `EnableMacro=True` on this machine; a user macro trigger that collides with a SmartKey shorthand or Telex typo (e.g. macro on `dd`, `ko`) makes `committed_at_break` wrong and the word invisible to SmartKey. SmartKey never reads lotus's macro table (lotus-engine.cpp:57).

**[P2] The Vietnamese dictionary is a vendored fork with no pinning.**
Today the bundled `data/vietnamese.cm.dict` and installed `/usr/share/fcitx5/lotus/vietnamese.cm.dict` sets are **identical** (0 differing words; ordering only). Both sides sort before lookup, so 1:1 holds *today*; a lotus package update silently changes `is_vietnamese_dict_word`/`has_vietnamese_dict_prefix` — which drive `must_fallback` modeling and rescue confidence (vi_telex.rs:78–83, 1217–1223) — with no version check.

**[P3] Single-letter vi shorthand expands English one-letter tokens under lotus.**
`shorthand_vi.tsv` contains `j→gì, s→sao, u→ừ, k, h, r`; step 1b (session.rs:181–221) fires before any English check, with only all-caps-acronym and mid-sentence-capital guards. Under lotus, session lang = `vi` for *everything typed*, so `meet u tomorrow ` becomes `meet ừ tomorrow `. The 1× BackSpace + pair-block learning mitigates repeats but not the first hit.

**[P3] EN-scorer fallback inside vi mode** (session.rs:307–310) can autocorrect a broken-Telex token into an English word when `sim.screen_chars == token len` and no vi rescue matched — deliberate bilingual choice, partially guarded (session.rs:296–305).

## 3. Lotus config / protocol assumptions

**[P2] No protocol or version check on lotus / lotus-server.**
The burst protocol (Pause terminator, echo keys, timing) is hardcoded from lotus 6.0.7 behavior with zero negotiation — no read of the addon `Version` (`/usr/share/fcitx5/addon/lotus.conf`), no liveness probe of `fcitx5-lotus-server` (a *system* unit on this machine: `fcitx5-lotus-server@domixi.service`). If the server dies, lotus's send fails silently per message and SmartKey keeps arming/consuming windows around echoes that never come — rescue starves with no diagnostic. The FFI exposes `smartkey_engine_is_vi_lotus_enabled` (a user-plugin toggle) but nothing probes lotus/server presence.

**[P2] Mode blindness.**
Lotus has 8 runtime-switchable modes (SuperSmooth/Smooth/Select/SurroundingText/Preedit/Emoji/Off/Default, lotus-state.cpp:1299–1326, per-mode hotkeys). SmartKey's model (record letters, evaluate at forwarded boundaries) only holds for the uinput family + Off; in **Preedit** mode nearly every key is filtered (lotus-state.cpp:305–325), so the bridge is fully inert while still recording. lotus-dbus exists (lotus-dbus.cpp) but SmartKey consumes no mode signal.

**[Done correctly]** Only public API used (`inputMethod(ic)` string compare + capability flags); no fcitx5 internals, no lotus ABI linkage, no D-Bus assumptions. Bamboo coexists cleanly (correctly stepped aside). The option *defaults* the sim hardcodes all match this machine's effective config.

## 4. Data / state coupling for vi

**[P2] `[lang.vi] personal_dict` holds English homographs** (`to`, `the`, `can`, `qua`, `dung`, `nay` — user rules.toml:41). Intended as toneless-Vietnamese shields; the effect is these tokens bypass the English scorer AND rescue entirely under lotus. Desirable for `dung`/`nay`, but `is_personal_word` also falls through to the *global* (English) `personal_dict` for vi (store.rs:1418–1420), so any English personal word additionally shields its ASCII homograph from Telex rescue in vi mode — a silent two-way bleed.

**[P2] Global `active_lang` mutated per keystroke** (session.rs:653) — safe under fcitx5's single-threaded event loop, but any future async read of `store.is_personal_word`/learning from another thread reads whatever window typed last.

**[P3] Learning paths are lang-tagged correctly**: `pending_unknown` carries `LangId`; vi graduations land in `[lang.vi].personal_dict`; `staged_telex_passthrough` state exists (store.rs:182) but can never be fed (see §2 dead-code finding). `vi_lotus` is a proper tested plugin toggle (store.rs:525–553; vi_lotus_eval.rs:188–218).

## 5. Test coverage of the lotus path

**[P1] The core test harness encodes the exact assumption production falsifies.**
`type_sequence` (crates/smartkey-core/tests/vi_lotus_eval.rs:28–72) treats every `' ' , . ! ?` as a boundary lotus forwards (line 42) and unconditionally feeds it to `session.process_key`. Real lotus filters the boundary whenever the commit differs from on-screen preedit (§1 P1) — so every rescue assertion (`ttieengs`, `tieeengs`, `hnuwngx`, `dddi`, `khooong`, vi_lotus_eval.rs:74–143) passes while the *runic-preedit* subclass (`duongx`-style, macro-expanded words) has no test and no working production path. No test models `Pause`, echo BackSpaces, buffered keys, or the `NAV_OR_CLICK` flush.

**[P1] The harness also masks the known char/byte `deleteSurroundingText` bug** (system-integration-audit.md §1): deletes are applied with `apply_key_to_buffer` char-pops (vi_lotus_eval.rs:66), so `delete_chars`-as-bytes corruption on accented words can never fail these tests.

**[P2] No addon-level bridge test.** `addon_load_smoke.cpp` covers only dlopen/load/unload, `classifyProgramName`, and `isSmartKeyBoundaryChar`. The burst window state machine, `takeLotusFilteredBoundary`, echo-suppression expiry, and Pre/Post coordination are untested (they live in `onKeyEvent`/`onPostKeyEvent`, requiring a live fcitx5).

**[P2] `tests/corpus/learning_traces.toml` is 100% English** — zero vi/lotus traces, so the trace runner has no golden coverage of vi learning (untouched-unknown staging, shorthand pair-block, passthrough graduation).

**[P3] Passthrough graduation is only tested by direct FFI poking** (c_abi_smoke.rs:190–191), never through a typing flow — which is how the missing wiring went unnoticed.

**[Done correctly]** Core-level vi logic is well tested: valid-word/slang shields, 1× BackSpace revert to raw Telex, plugin toggle, shorthand expansion + pair-block, English fixes under lotus. It is the *host integration contract* that is untested.

---

## 6. Consolidated priority list

**P1 — correctness (realistic typing scenarios)**
1. **Deferred evaluation for lotus-filtered boundaries**: when PostInputMethod never arrives for an armed boundary (detectable via the existing `lotusFilteredBoundary_` flag on the next key), run the deferred `finish_token_at_boundary` evaluation instead of the `NAV_OR_CLICK` drop — the session already computes `needs_uinput_restore_on_break`, so screen state is known. Fixes the `duongx`-class misses and macro-break misses in one move. (§1)
2. **Wire up `telex_passthrough`** — decide the intended trigger point (likely at the deferred evaluation from #1), add the English-lexicon cross-check ("English-Sentence Momentum Shield" as documented), and make the REPLACE branch safe when lotus is not composing. (§2, §1)
3. **Widen/condition the echo window** to ≥ lotus's `kReplacementAckSlackMs` (200 ms) and stop arming it on non-burst keys; ideally track real burst state (Pause protocol) rather than a timestamp proxy. (§1)
4. **W2U mode support** in the sim, or read lotus's actual config (`W2U=Everywhere` on this machine). (§2)

**P2 — assumptions & robustness**
5. Read lotus's config instead of hardcoding `std_style`/free-marking; model or detect `BracketTransform`. (§2)
6. Macro-table awareness (at minimum: document that macro triggers collide with shorthand/typo rescue). (§2)
7. Pin or hash-check the vendored Vietnamese dict against the installed lotus dict; warn on drift. (§2)
8. lotus version/server-presence probe + diagnostic when rescue starves. (§3)
9. Align the boundary set with lotus's word-break definition (digits, `- ' @ / _ ~ < > = + * & ^ $ % |`). (§1)
10. Bridge-level tests: a host-contract harness that models lotus-filtered boundaries, echo bursts, and byte-accurate deletes; add vi traces to learning_traces.toml. (§5)
11. Guard `is_personal_word` fall-through so global English personal words don't shield vi homographs unintentionally. (§4)

**P3 — polish**
12. Single-letter vi shorthand vs English one-letter tokens (first-hit mitigation). (§2)
13. Document the Enter-never-rescues tradeoff in README/chat context. (§1)
14. README "steps aside" wording stale vs actual vi-pipeline behavior. (§2)
