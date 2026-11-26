# SmartKey `vi-lotus` Fix & Deployment Plan (`origin/main` @ `3406e2b`)

## 1. Baseline & Remote Commit Audit

- **Installed binary on `precision`:** `fcitx5-smartkey 0.4.0-1` (`/usr/lib/fcitx5/libsmartkey.so`), running alongside `fcitx5-lotus-bin 7.0.1-1` (`Mode="Uinput (Super Smooth)"`).
- **Remote `origin/main` (`3406e2b`):** 3 commits past `v0.4.0` (`fa07a27`, `474d4d1`, `3406e2b`):
  - **Already fixed on `origin/main` (not yet in installed `v0.4.0`):**
    - `fa07a27`: Expanded `dicts/en/{dev_terms.txt,proper_nouns.tsv,slang.txt}` and `dicts/vi/slang.txt` (+319 entries: `ai`, `os`, `iso`, `acp`, `mr`, `gpt`, `llm`, `rag`, `mcp`, `tui`, `lsp`, `oke`, `abt`, `ae`, `thg`, `trg`, `og`, `mik`, `chatgpt`, `claude`, etc.).
    - `474d4d1`: Fixed `ImeBridge` (`vi`) learner validation (`is_lexicon_known_word` checking `syllable_dict()` + `slang_set()`), guarded `untouched_3x` against `<3`-char fragments and built-in contractions/shorthands, dropped lone trailing quotes on contractions, and excluded `aI`/`oS` from 2-char CapsLock inversion.
  - **Still unfixed on `origin/main` (`3406e2b`) — proven causes of the `2026-11-26 11:37–11:38` & `vi` log bugs:**
    1. **3-letter English `FuzzyLexicon` hijacking in `vi` mode (`crates/smartkey-core/src/session.rs`):** Step 5 of `evaluate_vi_lotus_token` runs full `EnglishScorer` on `<= 3`-char tokens, rewriting Vietnamese shorthands/transpositions/acronyms into bogus English words (`cno -> con` 9x, `cso -> soc` 5x, `nso -> son` 2x, `ceo -> eco` 2x, `prm -> rpm` 2x), while `cno` and `elo` are missing from `dicts/vi/slang.txt`.
    2. **Pure-ASCII English `-s` transpositions hijacked by `VietnameseTelexRescue` (`crates/smartkey-core/src/session.rs` + `vi_telex.rs`):** Words with zero mid-word Lotus runes like `dcos` (`docs`, English score `0.625`), `lsit` (`list`), `msut` (`must`), and `csat` (`cast`) get swapped into markless acute-tone Vietnamese syllables (`dóc`, `lít`, `mút`, `cát`) before `EnglishScorer` runs.
    3. **Mid-word suffix replacement & Wayland Lotus `SurroundingText` sync loss (`adapters/fcitx5-smartkey/src/smartkey-addon.{h,cpp}`):**
       - **3a (`aborted_midword_prefix` guard):** When cursor navigation/click or a missed boundary desyncs `SmartKeySession`, SmartKey can evaluate a suffix (`"cj"` of `"hocj"`, `"ert"` of `"insert"`, `"ms"` of `"100ms"`) and replace it mid-word (`"hochị"`, `"insret"`, `"100mới"`). `applyReplaceAction` currently does not check whether the Unicode scalar immediately preceding `[cursor - delete_chars .. cursor]` in synced `SurroundingText` is a word character.
       - **3b (`boundaryAbortedNav` orphan replay):** In `SmartKeyAddon::onKeyEvent`, when `hasPendingBoundary()` times out (`boundaryAbortedNav`), `clearPendingBoundary()` discards `pendingPostBoundaryChars`, dropping the prefix of the next word typed during the 350 ms window (`vi:iwf` after `vi:thogiwf`, `vi:vity` from `antigravity`, `vi:lution` from `solution`).
       - **3c (`lotus_defer_boundary` premature `markClientMutated()`):** Calling `state->markClientMutated()` unconditionally in `PreInputMethod` on `lotus_defer_boundary` clears `surroundingSynced = false` microseconds before `onPostKeyEvent` runs when Lotus forwards `Space` untouched. That forces every Wayland Lotus replacement (`hqua -> hôm qua`, `cj -> chị`, `ddeenes -> đến`) off atomic `deleteSurroundingText` onto `forwardKey(BackSpace)` + 15 ms timer, and disables `hasPrecedingWordChar` / `PendingDeleteCheck` on Lotus boundaries. Meanwhile `lotus_pause` and `lotus_echo_bs` lacked `state->markClientMutated()`.
  - **Deferred until `smartkey trace` captures a `vi`/`lotus` repro:**
    - `fcitx5-lotus 7.0.1-1` `Uinput (Super Smooth)` synthetic ASCII echo leaks (`vi:casicasi`, `vi:cnocno`, `vi:babaor`) and whether `"ddeenes"` at `11:38:26` had an echo-induced `delete_chars` mismatch. (`smartkey trace on` is already enabled on `precision`.)

---

## 2. Step-by-Step Implementation (against `origin/main` @ `3406e2b`)

Start from a clean checkout of `main` at `3406e2b` on your build machine:

```bash
git checkout main && git pull --ff-only origin main
```

### 2.1 `dicts/vi/slang.txt` & rebuild `vi_lexicon.skdict`

Append `cno` and `elo` at the end of `dicts/vi/slang.txt` (after `zalopay`):

```text
grab
tiki
zalopay
cno
elo
```

Then rebuild **only** the Vietnamese pack (builds 100% offline from `dicts/vi/`; do not rebuild `en` unless `data/sources/` is fetched):

```bash
cargo run --release -p smartkey-cli --bin smartkey-dev -- build-pack vi
```

This updates `crates/smartkey-core/assets/vi_lexicon.skdict`.

### 2.2 `crates/smartkey-core/src/vi_telex.rs`

At line 1644, change `has_vietnamese_mark` visibility to `pub(crate)`:

```rust
pub(crate) fn has_vietnamese_mark(word: &str) -> bool {
```

### 2.3 `crates/smartkey-core/src/session.rs` (`evaluate_vi_lotus_token`, lines 719–755)

Replace Step 4's `rescue_vi_telex` return block and Step 5's `EnglishScorer` fallback block with:

```rust
        if can_rescue_vi && !shield_from_vi_rescue {
            if let Some(rescue) = self.rescue_vi_telex(token, lotus_opts) {
                if !self
                    .store
                    .is_pair_blocked(&lower, &rescue.replacement.to_lowercase())
                {
                    let has_midword_vi_rune =
                        sim.needs_uinput_restore_on_break || sim.is_composed_vietnamese;
                    if !has_midword_vi_rune
                        && !crate::vi_telex::has_vietnamese_mark(&rescue.replacement)
                    {
                        if let Some(scorer) = self.scorer() {
                            if let outcome @ EvalOutcome::Autocorrect { kind, score, .. } =
                                scorer.evaluate(token, ctx, self.store.scorer_config(), &self.store)
                            {
                                if score >= 0.50
                                    && !(token.chars().count() <= 3
                                        && kind == MatchKind::FuzzyLexicon)
                                {
                                    return outcome;
                                }
                            }
                        }
                    }
                    return EvalOutcome::Autocorrect {
                        replacement: rescue.replacement,
                        kind: MatchKind::VietnameseTelexRescue,
                        score: 9.2,
                        margin: 9.2,
                    };
                }
            }
        }

        // If Lotus's 3x mark undo changed the on-screen character count (`tieeeng` or `feeel -> feel`),
        // do not run fuzzy English guesses on the raw keystrokes. But when the token is not a known
        // English word, still let it be learned/logged (`do_not_learn: false`) so uinput-restore
        // words that no rescue candidate matched (e.g. `duonxg`) feed `log_unfixed_unknown` and
        // `3x -> personal_dict` staging exactly like the forwarded-gibberish path does.
        if lotus_composing && sim.screen_chars != token.chars().count() {
            return EvalOutcome::Unchanged {
                reason: if is_known_english {
                    MatchKind::ValidWord
                } else {
                    MatchKind::Bypass
                },
                do_not_learn: is_known_english,
            };
        }

        // 5. Fallback to Layer 1 English scorer (`dont -> don't`, `teh -> the`, `chagnes -> changes`):
        //    In `vi` mode, suppress `<= 3`-char statistical `FuzzyLexicon` guesses (`cno -> con`,
        //    `cso -> soc`, `nso -> son`, `ceo -> eco`, `prm -> rpm`) while preserving `KnownTypoDict`,
        //    contractions, shorthands, shift-slips, and `PersonalDict`.
        if let Some(scorer) = self.scorer() {
            let outcome = scorer.evaluate(token, ctx, self.store.scorer_config(), &self.store);
            if token.chars().count() <= 3
                && matches!(
                    outcome,
                    EvalOutcome::Autocorrect {
                        kind: MatchKind::FuzzyLexicon,
                        ..
                    }
                )
            {
                return EvalOutcome::Unchanged {
                    reason: MatchKind::Bypass,
                    do_not_learn: false,
                };
            }
            return outcome;
        }
```

### 2.4 `crates/smartkey-core/tests/vi_lotus_eval.rs`

Append this regression test at the bottom of `crates/smartkey-core/tests/vi_lotus_eval.rs`:

```rust
#[test]
fn test_vi_lotus_shields_short_fuzzy_english_and_ascii_english_transpositions() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Short <=3-char tokens (`cno`, `cso`, `nso`, `ceo`, `prm`, `elo`) must NOT get hijacked by
    //    English `FuzzyLexicon` guesses (`con`, `soc`, `son`, `eco`, `rpm`), while curated
    //    `KnownTypoDict` (`teh -> the`) and contractions (`dont -> don't`) still work.
    let (short_buf, _) = type_sequence(&mut session, &mut engine, "cno cso nso ceo prm elo teh dont ");
    assert_eq!(short_buf, "cno cso nso ceo prm elo the don't ");

    // 2. Pure-ASCII English transpositions (`dcos`, `lsit`, `msut`) must autocorrect to English
    //    (`docs`, `list`, `must`) rather than markless acute-tone Vietnamese (`dóc`, `lít`, `mút`),
    //    while genuine Vietnamese Telex rescues (`anfy -> này`, `hoagnf -> hoàng`, `ddeenes -> đến`)
    //    still rescue to Vietnamese.
    let _ = session.reset(&mut engine);
    let (mixed_buf, _) = type_sequence(
        &mut session,
        &mut engine,
        "dcos lsit msut anfy hoagnf ddeenes ",
    );
    assert_eq!(mixed_buf, "docs list must này hoàng đến ");
}
```

### 2.5 `adapters/fcitx5-smartkey/src/smartkey-addon.h`

Keep all helpers header-only C++ with zero FFI calls (so `addon_load_smoke` links cleanly without `libsmartkey_ffi`). Right after `stepUtf8Codepoint(...)` (around line 268), add:

```cpp
inline uint32_t decodeUtf8Codepoint(std::string_view text, std::size_t idx,
                                    std::size_t &nextIdx) {
    if (idx >= text.size()) {
        nextIdx = text.size();
        return 0xFFFD;
    }
    const unsigned char b0 = static_cast<unsigned char>(text[idx]);
    if (b0 < 0x80) {
        nextIdx = idx + 1;
        return b0;
    }
    if (b0 >= 0xC2 && b0 <= 0xDF && idx + 1 < text.size()) {
        const unsigned char b1 = static_cast<unsigned char>(text[idx + 1]);
        if ((b1 & 0xC0) == 0x80) {
            nextIdx = idx + 2;
            return ((static_cast<uint32_t>(b0 & 0x1F) << 6) |
                    static_cast<uint32_t>(b1 & 0x3F));
        }
    } else if (b0 >= 0xE0 && b0 <= 0xEF && idx + 2 < text.size()) {
        const unsigned char b1 = static_cast<unsigned char>(text[idx + 1]);
        const unsigned char b2 = static_cast<unsigned char>(text[idx + 2]);
        const bool validLead =
            (b0 != 0xE0 || b1 >= 0xA0) && (b0 != 0xED || b1 <= 0x9F);
        if (validLead && (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80) {
            nextIdx = idx + 3;
            return ((static_cast<uint32_t>(b0 & 0x0F) << 12) |
                    (static_cast<uint32_t>(b1 & 0x3F) << 6) |
                    static_cast<uint32_t>(b2 & 0x3F));
        }
    } else if (b0 >= 0xF0 && b0 <= 0xF4 && idx + 3 < text.size()) {
        const unsigned char b1 = static_cast<unsigned char>(text[idx + 1]);
        const unsigned char b2 = static_cast<unsigned char>(text[idx + 2]);
        const unsigned char b3 = static_cast<unsigned char>(text[idx + 3]);
        const bool validLead =
            (b0 != 0xF0 || b1 >= 0x90) && (b0 != 0xF4 || b1 <= 0x8F);
        if (validLead && (b1 & 0xC0) == 0x80 && (b2 & 0xC0) == 0x80 &&
            (b3 & 0xC0) == 0x80) {
            nextIdx = idx + 4;
            return ((static_cast<uint32_t>(b0 & 0x07) << 18) |
                    (static_cast<uint32_t>(b1 & 0x3F) << 12) |
                    (static_cast<uint32_t>(b2 & 0x3F) << 6) |
                    static_cast<uint32_t>(b3 & 0x3F));
        }
    }
    nextIdx = idx + 1;
    return 0xFFFD;
}

inline bool isUnicodeWordCodepoint(uint32_t cp) {
    if (cp < 0x80) {
        return (cp >= 'a' && cp <= 'z') || (cp >= 'A' && cp <= 'Z') ||
               (cp >= '0' && cp <= '9') || cp == '_';
    }
    if (cp < 0x00C0 || cp == 0x00D7 || cp == 0x00F7) {
        return false;
    }
    if ((cp >= 0x2000 && cp <= 0x206F) || (cp >= 0x20A0 && cp <= 0x2BFF) ||
        (cp >= 0x3000 && cp <= 0x303F) || (cp >= 0xFF01 && cp <= 0xFF0F) ||
        (cp >= 0xFF1A && cp <= 0xFF20) || (cp >= 0xFF3B && cp <= 0xFF40) ||
        (cp >= 0xFF5B && cp <= 0xFF65) || (cp >= 0xFFF0 && cp <= 0xFFFF)) {
        return false;
    }
    return true;
}

inline bool hasPrecedingWordChar(std::string_view text, uint32_t cursorChars,
                                 uint32_t deleteChars) {
    if (deleteChars == 0 || cursorChars <= deleteChars ||
        cursorChars > 16384) {
        return false;
    }
    const uint32_t precedingCharIdx = cursorChars - deleteChars - 1;
    std::size_t byteIdx = 0;
    for (uint32_t cp = 0; cp < precedingCharIdx; ++cp) {
        if (byteIdx >= text.size()) {
            return false;
        }
        byteIdx = stepUtf8Codepoint(text, byteIdx);
    }
    if (byteIdx >= text.size()) {
        return false;
    }
    std::size_t nextIdx = byteIdx;
    const uint32_t prevCp = decodeUtf8Codepoint(text, byteIdx, nextIdx);
    return isUnicodeWordCodepoint(prevCp);
}
```

### 2.6 `adapters/fcitx5-smartkey/src/smartkey-addon.cpp`

Apply these 4 edits:

1. **`applyReplaceAction` (around line 919):** Abort mid-word suffix replacements when synced `SurroundingText` shows a word character immediately before `[cursor - delete_chars .. cursor]`:

```cpp
    if (act.delete_chars > 0) {
        const bool fallbackAvailable =
            canForwardBackspace(ic->frontendName(), ic->capabilityFlags());
        state->checkUnansweredBeforeDelete(fallbackAvailable);
        const bool surroundUsable = state->canUseDeleteSurrounding(
            ic->capabilityFlags().test(CapabilityFlag::SurroundingText),
            ic->surroundingText().isValid(), ic->surroundingText().cursor(),
            act.delete_chars, ic->frontendName());
        auto abortReplace = [&](std::string_view traceOut) {
            flushPendingDelayedCommit(ic, state);
            state->markClientMutated();
            disarmSessionJustAutocorrected(state, monotonicNowMs());
            if (canTraceContext(ic, normalizeProgramId(ic->program()))) {
                TraceEventFields f{};
                f.ev = "replace";
                f.out = traceOut;
                f.hasDel = true;
                f.del = act.delete_chars;
                if (act.insert_utf8 != nullptr && act.insert_byte_len > 0) {
                    f.hasTxt = true;
                    f.txt.assign(act.insert_utf8, act.insert_byte_len);
                }
                f.s0 = snapshotSessionCompact(state->session());
                const std::string appName = normalizeProgramId(ic->program());
                const std::string imName =
                    instance_ != nullptr ? instance_->inputMethod(ic) : "";
                emitTraceRecord(trace_, ic, state, appName, imName,
                                monotonicNowMs(), f);
            }
        };
        if (surroundUsable &&
            hasPrecedingWordChar(ic->surroundingText().text(),
                                 ic->surroundingText().cursor(),
                                 act.delete_chars)) {
            abortReplace("aborted_midword_prefix");
            return;
        }
        if (!surroundUsable && !fallbackAvailable) {
            abortReplace("aborted_no_delete");
            return;
        }
    }
```

2. **`lotus_pause` & `lotus_echo_bs` (around lines 1044–1076):** Add `state->markClientMutated();` inside both handlers:
   - In `if (rawKey.sym() == FcitxKey_Pause)`: add `state->markClientMutated();` next to `state->markLotusActivity();`.
   - In `if (state->expectsLotusEchoBackspace(nowMs) && ...)`: add `state->markClientMutated();` next to `state->markLotusActivity();`.

3. **`boundaryAbortedNav` (around lines 1150–1176):** Preserve queued post-boundary prefix chars when `hasPendingBoundary()` expires on a subsequent key:

```cpp
        bool boundaryAbortedNav = false;
        std::vector<uint32_t> orphanPostChars;
        if (state->hasPendingBoundary()) {
            if (keyKind == SMARTKEY_KEY_CHAR &&
                !isSmartKeyBoundaryChar(codepoint) &&
                state->isInLotusBurst(nowMs)) {
                state->pushPostBoundaryChar(codepoint);
                if (isTelexBurstTriggerChar(codepoint) ||
                    state->hasLotusActivityInWord()) {
                    state->armLotusBurst(nowMs);
                }
                state->markClientMutated();
                state->setSuppressPostKey(true);
                if (trace) {
                    emitPreKey("queued_post_boundary", s0,
                               snapshotSessionCompact(state->session()),
                               chUtf8);
                }
                return;
            }
            boundaryAbortedNav = true;
            orphanPostChars = state->takePostBoundaryChars();
            state->clearPendingBoundary();
            state->takeLotusActivityInWord();
            state->markClientMutated();
            SmartKeyAction unused{};
            smartkey_session_process_key(state->session(), engine_,
                                         SMARTKEY_KEY_NAV_OR_CLICK, 0, nowMs,
                                         &unused);
            if (keyKind == SMARTKEY_KEY_CHAR && !orphanPostChars.empty()) {
                replayPostBoundaryChars(state, engine_, orphanPostChars, nowMs);
            }
        }
```

4. **`lotus_defer_boundary` vs `lotus_defer_other` (around lines 1237–1259):** Move `state->markClientMutated();` into the `else` (`lotus_defer_other`) branch so `lotus_defer_boundary` preserves `surroundingSynced` across the synchronous `PreInputMethod -> PostInputMethod` handoff when Lotus forwards `Space`/punctuation untouched:

```cpp
        // For word-break/lookahead chars (`Space`, `,`, `.`, `!`, `?`, etc.), `BackSpace`,
        // `Enter`, `Tab`, and cursor nav: defer to `onPostKeyEvent` (if Lotus forwards) or
        // `onCommitString` (if Lotus filters the boundary to restore/commit).
        std::string_view outLabel;
        if (keyKind == SMARTKEY_KEY_CHAR && isSmartKeyBoundaryChar(codepoint)) {
            state->armLotusBurst(nowMs);
            state->setPendingBoundary(codepoint);
            outLabel = boundaryAbortedNav
                           ? "boundary_aborted_nav+lotus_defer_boundary"
                           : "lotus_defer_boundary";
        } else {
            state->markClientMutated();
            state->clearPendingBoundary();
            outLabel = boundaryAbortedNav
                           ? "boundary_aborted_nav+lotus_defer_other"
                           : "lotus_defer_other";
        }
```

### 2.7 `adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp`

Add `verifyMidwordPrefixReplaceGuard()` right before `} // namespace` (around line 650) and call it inside `main(...)`:

```cpp
void verifyMidwordPrefixReplaceGuard() {
    using fcitx::smartkey::hasPrecedingWordChar;

    // Mid-word suffixes (must abort replacement):
    assert(hasPrecedingWordChar("hocj", 4, 2));          // "ho" + "cj"
    assert(hasPrecedingWordChar("đến hocj", 8, 2));      // multi-byte prefix + "ho" + "cj"
    assert(hasPrecedingWordChar("insert", 6, 3));        // "ins" + "ert"
    assert(hasPrecedingWordChar("100ms", 5, 2));         // "100" + "ms"
    assert(hasPrecedingWordChar("tiếngteh", 8, 3));      // non-ASCII letter 'g'/'ế' before "teh"
    assert(hasPrecedingWordChar("Đcj", 3, 2));           // 'Đ' before "cj"

    // Whole-word boundaries (safe to replace):
    assert(!hasPrecedingWordChar("cj", 2, 2));           // buffer start
    assert(!hasPrecedingWordChar("đến cj", 6, 2));       // after space
    assert(!hasPrecedingWordChar("(cj", 3, 2));          // after '('
    assert(!hasPrecedingWordChar("\"cj", 3, 2));         // after '"'
    assert(!hasPrecedingWordChar("“cj", 3, 2));          // after U+201C curly quote
    assert(!hasPrecedingWordChar("foo-bar", 7, 3));      // after '-'
    assert(!hasPrecedingWordChar("\xFFcj", 3, 2));       // malformed UTF-8 -> 0xFFFD fails open
    assert(!hasPrecedingWordChar("cj", 2, 0));           // deleteChars == 0
    assert(!hasPrecedingWordChar("cj", 20000, 2));       // cursor > 16384 bound
}
```

Call `verifyMidwordPrefixReplaceGuard();` right after `verifyBackspaceCapabilityAndDroppedDeleteGuard();` in `main()`.

---

## 3. Verification, Packaging & Deployment Checklist

### On the build machine (with Rust + CMake + Fcitx5 dev headers)

```bash
export PATH="$HOME/.cargo/bin:$PATH"

# 1. Workspace unit & integration tests + corpus eval + version check
cargo test --workspace
cargo run --release -p smartkey-cli --bin smartkey-dev -- eval
./scripts/check-versions.sh

# 2. Build C++ Fcitx5 addon & run CTest (addon_load_smoke)
./adapters/fcitx5-smartkey/scripts/install-local.sh
ctest --test-dir adapters/fcitx5-smartkey/build --output-on-failure

# 3. Verify explain outputs for the 11:37-11:38 & VI log regressions
smartkey explain --lang vi cno
smartkey explain --lang vi cso
smartkey explain --lang vi dcos
smartkey explain --lang vi anfy
smartkey explain --lang vi ddeenes
```

Expected `explain` outcomes:
- `cno`, `cso`: `UNCHANGED` (`cno` via VI slang shield, `cso` via `<=3`-char `FuzzyLexicon` suppression).
- `dcos`: `AUTOCORRECT -> "docs"`.
- `anfy` -> `"này"`, `ddeenes` -> `"đến"`.

### Build Arch package (or cut a release) for `precision`

- Either build locally with `./scripts/build-arch-pkg.sh` (or cut a release with `./scripts/release.sh patch`) after committing changes on `main`.
- Then on `precision`:
  - Install the updated package (`sudo pacman -U ./fcitx5-smartkey-*.pkg.tar.zst` or `./scripts/install.sh update --yes`) and restart Fcitx5 (`fcitx5 -r -d`).
  - Confirm `smartkey status` shows no dual-install conflict (`/usr/lib/fcitx5/libsmartkey.so` only) and `Trace Recorder : ENABLED`.
  - Clean the 4 corrupted typo entries from `~/.config/smartkey/rules.toml` `personal_dict`:

```bash
smartkey rules remove word ssytem
smartkey rules remove word anyhting
smartkey rules remove word antigra
smartkey rules remove word archli
smartkey reload && smartkey status
```
