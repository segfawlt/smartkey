# SmartKey — Architecture & Implementation Plan (v3.2 Final Spec)

> **Status:** v3.2 Locked — Implementation Active  
> **License:** `GPL-3.0-or-later`  
> **Companion doc:** [competitor-analysis.md](./competitor-analysis.md)  
> **Core Strategy:** Pure Rust engine (`smartkey-core`, zero `-sys` C deps, zero `$HOME` assumptions) combining **`crate-ci/typos` (`95,924` pairs) + `fst` + `levenshtein-automata(2, true)` + `POPCNT` smash rescue + prose bigrams** with a **deterministic, comment-preserving `toml_edit` learning loop (`rules.toml`)**, integrated first into **`fcitx5-lotus`** on Linux Wayland and portable via JNI/word-level hooks to **`HeliBoard`** on Android.

---

## 1. Core Architectural Principles

1. **Functional Core, Imperative Shell (`SmartKeyEngine` + Per-Window `SmartKeySession`):**
   - `smartkey-core` is **100% pure, synchronous, single-threaded Rust** with zero OS, clock, thread, `$HOME`, or `-sys` C library dependencies (guarantees seamless compilation across Linux `x86_64` and all 4 Android HeliBoard NDK ABIs down to `minSdk = 21`).
   - Split cleanly into:
     - **`SmartKeyEngine`** (shared per process): owns the zero-copy `fst` lexicon, known-smash index, UK↔US dialect map, bigram table, and `toml_edit`-backed `RuleStore`.
     - **`SmartKeySession`** (1 lightweight struct per window / `InputContext`): owns the in-memory shadow buffer (`current_word` + `last_committed_word`), `prev_word`, `InputOrigin`, `lang` tag, and `JustAutocorrected` undo state.
   - Exposes **two clean entry layers**:
     - **Keystroke stream API (`process_key`)** for Linux (`fcitx5-lotus` / CLI REPL).
     - **Word-level learner API (`rerank_candidates`, `on_auto_commit`, `on_undo_revert`, `on_manual_edit`, `on_chip_pick`)** directly underneath `process_key` so Android `HeliBoard` (`Suggest.kt` / `InputLogic.java`) plugs in without adapter hacks.
2. **PC vs. Mobile Physics Separation:**
   - **PC Layer 1** optimizes first-class for 10-finger physical switch mistakes: two-hand transpositions (`chagnes`, `soemtimes`), switch chatter (`thhe`, `nnext`), pinky `;` slips (`don;t`), Shift/CapsLock timing slips (`THat's`, `situatioN:`, `tHIS`), and thumb-spacebar bottom-row clips (`inbthe`).
   - **Shared Portable Brain (`rules.toml`):** `personal_dict`, `blocklist`, and the Undo/Edit learner share identical behavior across Linux and HeliBoard, with Cargo feature `bundled-en` optional so `libsmartkey_jni.so` compiles down to `~300 KB` inside HeliBoard's 25 MB APK budget.
3. **Respect User Case, Only Fix Finger Slips:**
   - The lexicon stores lowercase lemmas only.
   - Intentional mid-sentence capitals (`Apple`, `Rust`, `GitHub`) are **never** altered.
   - Lowercase brands/nouns (`github`, `linux`) are **never** nag-capitalized (`github` stays `github`; typo `githbu` fixes to `github`).
   - Casing only adjusts on standalone `i → I` / `im → I'm`, unmistakable Shift/CapsLock timing slips (`THat's`, `situatioN:`, `tHIS`), and optional `auto_capitalize_sentence` (with abbreviation shield for `e.g.`, `i.e.`, `etc.`, `vs.`, `Dr.`).
4. **Unicode-First Language Seam (`English Core` Now → `fcitx5-lotus` Bilingual Next):**
   - Every buffer slice and deletion count uses `char::is_alphabetic()` and `.chars().count()` (never ASCII range checks or UTF-8 byte `.len()`).
   - `smartkey-core` carries zero Vietnamese dictionaries in Phase 1, exposing only `InputOrigin` and an opaque `lang` tag (`"en"` default; `"vi"` set by the Phase 2 `fcitx5-lotus` adapter so Vietnamese slang learns into `[lang.vi]` without polluting English).

---

## 2. Evaluation Waterfall (Layer 1)

Triggered on word boundaries (`Space`, `,`, `!`, `?`, closing `)`, `]`, `}`, `"`, `”`), plus provisional lookahead on `.` / `:` (`PendingTrailingDot` protecting `teh.com` & `config.json.`) and `;` (`PendingInternalSemicolon` catching `don;t → don't` while letting `"done; "` pass through).

1. **Step 0 — Safety & Token Guards:**
   - Skip if `AppProfile::Bypass` (Password, Terminal, URL, Email, Code Editor), inside Markdown `` `inline_code` ``, or tokens containing digits, underscores, internal dots/slashes, `@mention`, `#tag`, `/slash`, `ALL_CAPS`, or mid-word `camelCase` (`useState`, `GitHub`, `iOS`, `mRNA`, `dBm`).
   - Strip outer single quotes (`'apple'` → `' + apple + '`) while preserving leading-apostrophe contractions (`'em`, `'cause`, `'til`, `'90s`).
   - **Expressive stretch stemmer (`soooo`, `trờiiii`):** If a token ends in `>= 3` identical letters and collapsing that tail yields a valid word, keep the stretched word verbatim without polluting `personal_dict`.
2. **Step 1 — Physical Shift / CapsLock Timing Normalizers:**
   - **Held-Shift (`THat's → That's`, `THis → This`):** 2 uppercase + `>= 2` lowercase chars (excluding plural acronyms `IDs`, `PRs`, `UIs`, `OSes`, `VMs`, `URLs`).
   - **Early-Shift before shifted punctuation (`situatioN: → situation:`):** All-lowercase + 1 final uppercase char immediately before `: ! ? " )`.
   - **Caps Lock Inversion (`tHIS → This`, `hELLO → Hello`):** 1 lowercase + `>= 2` uppercase chars (excluding `iOS`, `mRNA`, `tRNA`, `rRNA`, `dBm`).
   - **Sentence-Start Late-Shift (`tHis → This`, `wHat → What`):** Restricted strictly to sentence-start so mid-sentence `mMap`, `iNode`, `vTable`, `oAuth` are never touched.
3. **Step 2 — User `rules.toml` Priority Layer:**
   - `[snippets]` → `[context_corrections]` → `[corrections]` (filtered against `blocklist`).
4. **Step 3 — Valid-Word Fast Path, Dialect Mode & Contractions:**
   - Check `personal_dict` + session shield + lexicon under active `dialect`:
     - `mixed` *(Default)*: Both US & UK valid words pass untouched; typos resolve to US spelling.
     - `en_US` / `en_GB`: Normalizes cross-dialect spellings (`colour ↔ color`, `behaviour ↔ behavior`) and fixes typos to the chosen dialect.
   - Apply unblocked contractions (`im → I'm`, `dont → don't`, `doesnt → doesn't`, `cant → can't`, `wont → won't`, `ive → I've`, `didnt → didn't`, standalone `i → I`) and unmerged compounds (`alot → a lot`, `aswell → as well`, `atleast → at least`, `infront → in front`).
5. **Step 4 — Non-Word Decoder (`typos-dict` + `fst` + `levenshtein-automata` + `POPCNT`):**
   - **Known smashes (`95,924` pairs):** Instant lookup in `crate-ci/typos` (using prose bigrams to disambiguate multi-target entries like `ther → there/their/the/other`).
   - **Switch-chatter pre-filter (`thhe → the`, `reallly → really`, `wword → word`, `nnext → next`):** Collapses invalid word-initial double consonants and mid-word triple letters.
   - **Thumb-spacebar & missed-space splitter (`ofthe → of the`, `inbthe → in the`, `andnthe → and the`):** Splits `w1 + (" " | b/n/v/m/c) + w2` when `(w1, w2)` is a high-frequency function-word bigram.
   - **Damerau–Levenshtein FST (`d <= 2`, transpositions enabled)** + **`u32` `POPCNT` multiset smash rescue (`hstiautoin → situation`)**, scored by word frequency + `prev_word` bigram + QWERTY adjacency + transposition bonus, projecting user TitleCase (`Chagnes → Changes`, `githbu → github`).

---

## 3. Deterministic Learning Matrix & Anti-Poisoning Guards (Layer 2)

Tracked entirely via SmartKey's in-memory shadow buffer (`current_word` + `last_committed_word`), working identically across Wayland/X11/Android without relying on flaky `surroundingText`. Navigation keys (`Left`/`Right`/`Up`/`Down`/`Home`/`End`) and `fcitx5-lotus`'s `g_mouseClicked` monitor immediately reset compose state to `Idle`.

| User Behavior | Example | Immediate Session Effect | Saved to `rules.toml` | Guard / Rationale |
|---|---|---|---|---|
| **Undo + Edit to a 3rd word** | `"fomr "` → `"foam "` → `BS` → `"form "` | Blocks `fomr → foam` | **`1x`** `blocklist` (`fomr ≠ foam`); **`2x`** `[corrections]` (`fomr = form`) | Undoing and typing a third word is 100% deliberate. |
| **Undo + Keep verbatim (`Space`)** | `"hok "` → `"hot "` → `BS` → `Space` | **Instant (`1x`)** session shield on `"hok"` | **`2x`** → `blocklist` + `personal_dict` | Stops nagging immediately today while preventing 1 accidental `BS + Space` from ruining `"teh → the"`. |
| **Undo + Retype same suggestion** | `"chagnes "` → `"changes "` → `BS` → `"changes "` | **Cancelled (`Self-Confirm Cancel`)** | **Never** | `final == rejected` proves the `BackSpace` was accidental. |
| **Manual typo fix (post- or pre-`Space`)** | `"ttoally "` → `BS` → `"totally "`, or `"cna"` → `BS` → `"can "` | Staged (`count = 1`) | **`2x`** → `[corrections]` (or `[context_corrections]` if `orig` is valid) | Must pass `Delta-Span Check` + Guards 1–3. |
| **Untouched unknown word ("do nothing")** | Typed `"khum "` or `"promptcord "` and kept going | Staged (`count = 1`) | **`3x`** → `personal_dict` (scoped to active `lang`) | `3x` avoids learning a `1x` typo you were too lazy to go back and fix. |
| **`>1` word erase OR `XXYYY → XXZZZZ`** | Erasing across 2 words, or `"prodcut"` → `"production"` | **Reset to `Idle` / Ignored** | **Never** | Phrase rewrite or suffix/idea change (`Delta-Span`: `"cut"` → `"uction"` has length diff `+3` & unrelated chars). |

Ambiguous repeated edits rejected by Guards 1–3 are logged to a bounded `skipped_edits` ring buffer in `learner_state.json` for optional human/agent review into `rules.toml`.

---

## 4. Build Milestones

1. **Milestone 1:** `smartkey-core` Layer 1 PC Autocorrect + `tests/corpus/{must_fix.tsv,must_not_touch.txt}` + `smartkey-cli` (`eval`, `explain`, `repl`, `build-lexicon`).
2. **Milestone 2:** `SmartKeySession` Layer 2 Learning State Machine + `Delta-Span` / Guards 1–3 + comment-preserving `toml_edit` `RuleStore` + presets + `learning_traces.toml` + `proptest`.
3. **Milestone 3:** `smartkey-ffi` (`cbindgen` C staticlib + `catch_unwind` + HeliBoard word-level hooks).
4. **Milestone 4:** `fcitx5-lotus-smartkey` Linux Wayland integration (`uinput` replacement, `g_mouseClicked` reset, Vietnamese frequency check `"is"` over `"í"`, `telex_passthrough` learning).
5. **Milestone 5 (Future):** `heliboard-smartkey` Android JNI fork (`cargo-ndk`, `--no-default-features`).
