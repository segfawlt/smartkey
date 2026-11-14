# EkaKey Core Learnings — Verified Audit & Concrete Proposals

**Date:** 2026-11-15  
**Scope:** Core engine and CLI rule-lifecycle management (`crates/smartkey-core`, `crates/smartkey-cli`). UI surfaces (toasts, suggestion tiles, visual effects) remain deferred.  
**Status:** **Implemented (`F1`–`F4` with casing fixes & confidence tiers).**  
**Companion docs:** `docs/competitor-analysis.md` §3.2, `docs/rfc-multilang-dict-packs.md` (covers Tier A/B language packs), `docs/findings-2026-11-11-event-logging-audit.md` (§L3 overlaps with F2).

---

## 1. Calibration: EkaKey's README vs. Actual Code

Benchmark against EkaKey's source (`RanvirRox/EkaKey-autocorrect-globally`), not its README:

- **Contractions (`don;t → don't`, `doesnt → doesn't`) do not exist in code.** `bin/sanitize_dictionary.dart` strips everything outside `^[a-zA-Z]+$`, so `don't` cannot enter the dictionary. SmartKey's `MatchKind::Contraction`, `SemicolonContraction`, and `TransposedApostrophe` already exceed what EkaKey ships.
- **Key injection:** EkaKey's README says `SendInput`; `FastKeyboard` (`lib/ekakey_process.dart:1017-1136`) uses the older `keybd_event` API.
- **Test coverage:** EkaKey has a single 30-line widget test with zero eval corpus or false-positive shield. SmartKey enforces `tests/corpus/must_fix.tsv` (117 lines) and `tests/corpus/must_not_touch.txt` (158 lines).
- **Known bugs in EkaKey:** `lib/ekakey_process.dart:1162-1164` (`//bug####` — edit-mode backspace boundary unhandled).
- **Language packs:** Community pack workflows are already covered cleanly by `docs/rfc-multilang-dict-packs.md` (`pack.toml` + `.skdict` v3, zero Rust edits per Tier A language) and need no separate plan.

---

## 2. Verified Engine Audit (Why Original Proposals Changed)

Re-verifying every item from the initial EkaKey comparison against `smartkey-core` and live `smartkey explain` behavior eliminated two false premises and pinpointed four concrete improvements:

### 2.1 Rejected for Silent Space Autocorrect — Double Metaphone (`MatchKind::Phonetic`)

Adding Double Metaphone to silent Space-triggered autocorrect is either redundant or breaks SmartKey's 0% false-positive guarantee:

1. **Classic sound-alike typos already autocorrect via `KnownTypoDict` (`95,924` entries from `crate-ci/typos`):** Observed via `smartkey explain`: `definately → definitely` (`score=0.60`), `enviroment → environment` (`0.70`), `seperate → separate` (`0.72`), `occured → occurred` (`0.80`), `phoen → phone` (`0.94`).
2. **`FuzzyLexicon` (`d=1`) already finds near-phonetic candidates and intentionally declines high-cost ones:** `smartkey explain korrect` ranks `"correct"` `#1` (`edit=1.38`), and declines it (`score=-0.36`) because `k ↔ c` is distant on QWERTY. `smartkey explain fone` corrects to QWERTY-adjacent `"fine"` (`score=0.19`, `edit=0.68`, `i ↔ o`).
3. **Why SmartKey's existing shields do NOT save OOV slang/names from phonetics:** Unseen names, slang, and CLI identifiers are out-of-vocabulary on 1st and 2nd use (`valid-word = false`, and `personal_dict` requires `3x`). What protects `khum` (`tests/corpus/must_not_touch.txt:105`, Double Metaphone `KM` == `"come"`) is QWERTY `edit_cost` damping (`scorer.rs:886-909`, `score=-0.33 < 0.14`). Keeping `edit_cost` makes phonetic retrieval dead code (`korrect` still scores `-0.36`); discounting `edit_cost` for phonetic matches turns OOV slang, foreign words, and surnames into false positives.
4. **Internal/RFC contradictions:** EkaKey indexes `(SoundexCode, FirstLetter, Length)` (`lib/ekakey_process.dart:424-446`), so raw `FirstLetter` filtering misses `fone→phone` (`f≠p`) and `korrect→correct` (`k≠c`) anyway, while dropping `FirstLetter` balloons bucket collisions. Double Metaphone is also English-specific, conflicting with the language-agnostic Tier A architecture in `docs/rfc-multilang-dict-packs.md`.

**Decision:** Do **not** add Double Metaphone to silent autocorrect or `.skdict` v3. Cover genuinely common orthographic misspellings via curated pack overlays; revisit phonetics only if an explicit non-silent spell-hint popup is ever added.

### 2.2 Deferred — Full-Lexicon Word Counters & FST Prefix Completion

- `LearnerState::trouble_words` (`store.rs:192`) + `trouble_word_boost` (`scorer.rs:895`) already learn personal frequency priors for words the user repeatedly fixes.
- Incrementing a disk counter on *every* committed dictionary word turns zero-write valid words (~97% of keystrokes) into constant `learner_state.json` churn and persists a plaintext unigram profile of everything typed across apps.
- Prefix iteration over lexicographic `fst::Map` for 1–2 char prefixes visits ~10k–18k keys without top-k branch bounds, for a completion surface explicitly deferred (`docs/competitor-analysis.md:174-175`).

**Decision:** Defer general word counting and prefix completion APIs until a completion UI (e.g., HeliBoard suggestion strip) exists. Adopt **bounded `personal_dict` usage weighting** now (Proposal **F3**).

### 2.3 Corrected — Rule Provenance & Decay (`store.rs`)

The original draft inverted how `smartkey` actually stores rules and counters:

- `rules.toml` entries carry **no counters**; dynamic counts live in `learner_state.json`.
- `RuleStore` **already** writes `toml_edit` provenance comments (`# learned 2x YYYY-MM-DD (<app>) — was: "..."`) on `[corrections]` (`store.rs:1554-1558`) and `[context_corrections]` (`store.rs:1606-1610`) via `format_provenance_comment` (`store.rs:1229-1241`) when `diagnostics.annotate_rules = true` (`store.rs:78`). `add_permanent_block`, `add_personal_word_with_stage`, and `add_telex_passthrough` lack comments and `RuleLearned` events.
- Mutating `rules.toml` comments on every rule hit (`# ... · last hit ... · 3x`) would break the Git/Syncthing config-vs-state split with constant `rules.toml` diffs and `mtime` reload churn.
- Note trap: `RuleStore::clear_diagnostic_logs` (`store.rs:1243-1252`) wipes `state.applied_counts`, so pruning must read dedicated lifecycle stats rather than `applied_counts`.

### 2.4 Corrected — Instant `1x` Protection Scope (`learner.rs`)

The original draft conflated three separate guards:

- **Valid lexicon words and proper nouns are already shielded at `0x`** by Step 4b (`scorer.rs:752-834`); `record_untouched_unknown` (`learner.rs:783-789`) skips them intentionally.
- **`Monday → Tuesday` is blocked by Guard 1** (`MindChangeDistance` / `DeltaSpanRewrite`, `learner.rs:508-551`), whereas **Guard 2 (`Valid-Word Shield`, `learner.rs:585-590`) checks `orig`** (`form → from`), not `fixed`.
- **`Undo + Keep Verbatim` already shields at `1x`** (`store.add_session_word_shield` + `store.add_session_pair_block` at `learner.rs:250-258`) and fast-tracks `record_untouched_unknown` from `3x` to `1x` next use (`learner.rs:798-804`).
- **Actual gap (`learner.rs:623-637`):** When a user manually edits a typo into a **novel OOV word** (`target_is_novel_unknown == true`, e.g. `"Hyprlnad" → BS → "Hyprland"`), `record_manual_edit_inner` only increments `untouched_unknowns` to `1/3` with **no session shield** — typing `"Hyprland"` again in the same session can still get autocorrected away.

---

## 3. Concrete Feature & Function Proposals (`F1`–`F4`)

### F1 — `1x` Novel-OOV Session Shield, `2x` Fast-Track & Casing-Learning Fixes

**Goal:** Give deliberate manual repairs to novel OOV words (`"Hyprlnad" → BS → "Hyprland"`) the same immediate protection and `1x + 1x` graduation cadence that `Undo + Keep Verbatim` already has, while fixing intentional capitalization repairs (`"hyprland" → BS → "Hyprland"`, `"domixi" → "Domixi"`) without writing single-occurrence typos to `rules.toml`.

- **Files:** `crates/smartkey-core/src/learner.rs`, `crates/smartkey-core/src/store.rs`
- **Functions & Data Changes:**
  - `LearnerState` (`store.rs`): add `pub staged_personal_canonical: BTreeMap<String, String>` keyed by `"<lang>:<lower>"` to preserve canonical casing (`"Hyprland"`, `"PyTorch"`, `"Domixi"`) across a `1x` manual repair or untouched use followed by lowercase typing.
  - Case-only manual repair (`record_manual_edit_inner`, `learner.rs`): when `orig_lower == fixed_lower` and `fixed_trimmed` upgrades capitalization intentionally (`has_internal_upper` like `"PyTorch"`, `FLAG_TECH` lexicon entries like `"Hyprland"`, or novel OOV words like `"Domixi"` — excluding ordinary dictionary words like `"hello" → "Hello"` at sentence start), stage under `"case:<lang>:<lower>"` (`2x → personal_dict` via `"case_fix_2x"`, lifting any stale case-only block, and `1x` novel OOV also shields the session + fast-tracks `+2` in `untouched_unknowns`).
  - `record_manual_edit_inner` (`target_is_novel_unknown` branch):
    1. Immediately call `store.add_session_word_shield(&fixed_lower);` (only reached after passing length `>= 4`, `DeltaSpanRewrite`, `MindChangeDistance`, `is_key_bounce_extension`, `!is_expressive_stretch`, and `lookup_known_typos().is_none()`).
    2. Record canonical casing (`target_to_save`) in `state.staged_personal_canonical` when non-lowercase.
    3. Credit `+2` instead of `+1` into `state.untouched_unknowns[unk_key]` (graduating at `>= 3`), so **1 manual repair + 1 untouched confirmation = permanent `personal_dict` graduation**.
  - `record_untouched_unknown` (`learner.rs`):
    - Allow lowercase-first `personal_dict` entries (`"domixi"`) to upgrade canonical casing (`"Domixi"`) at `2x` intentional cased uses (`"case_upgrade_2x"`) instead of exiting early when `is_personal_word(&lower)` is true.
    - Prefer `state.staged_personal_canonical.remove(&key)` on graduation if the final confirming token is lowercase.
  - `RuleStore::clear_untouched_unknown` (`store.rs`): also clear `staged_personal_canonical` for that key.
- **Invariant:** Zero risk of `1x` typo-to-typo poisoning (`personal_dict` still requires a second independent confirmation) and zero sentence-start capitalization hijack on normal English words.

---

### F2 — Lifecycle `RuleStats`, Confidence Tiers, Write-Once Provenance, and `smartkey rules prune`

**Goal:** Complete write-once provenance across all learned rule types (`rules.toml` + `RuleLearned` JSONL), track creation, origin, and hit timestamps cleanly in `learner_state.json`, separate rules by user-inspectable confidence tiers (`Manual` / `Confirmed` / `Provisional` / `Suspect`), and expose safe pruning of stale, unused machine-learned rules.

- **Files:** `crates/smartkey-core/src/store.rs`, `crates/smartkey-core/src/lib.rs`, `crates/smartkey-cli/src/main.rs`
- **Data Model (`store.rs`):**
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
  #[serde(rename_all = "snake_case")]
  pub enum RuleOrigin {
      #[default]
      Manual,       // Explicit CLI / hand-authored rules.toml
      ManualFix2x,  // 2x manual repair (`[corrections]`, `[context_corrections]`, `personal_promoted_2x`, `case_fix_2x`, `case_upgrade_2x`, `telex_passthrough`)
      UndoReject,   // 1x/2x Undo rejection (`blocklist`, `undo_keep_2x`)
      Untouched3x,  // 3x untouched OOV word graduation into `personal_dict`
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum ConfidenceTier {
      Suspect,     // Untouched3x with 0 post-graduation uses, OR any learned rule with >= 1 undo/demote strike
      Provisional, // Machine-learned via 2x fix or 2x undo-keep, < 3 post-graduation hits, 0 strikes
      Confirmed,   // Machine-learned with >= 3 hits and 0 strikes
      Manual,      // Explicit user/CLI entry (`RuleOrigin::Manual`) or `pinned`, or untracked legacy hand-edited rule
  }

  #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
  #[serde(default)]
  pub struct RuleStat {
      pub origin: RuleOrigin,
      pub created_epoch_ms: u64,
      pub last_hit_epoch_ms: Option<u64>,
      pub hits: u32,
  }
  ```
  - Add `pub rule_stats: BTreeMap<String, RuleStat>` to `LearnerState` (`store.rs`).
  - Canonical key convention:
    - `"fix:<lang>:<typed_lower>"` (`[corrections]` / `[lang.<id>.corrections]`)
    - `"ctx:<prev_lower> <typed_lower>"` (`[context_corrections]`)
    - `"word:<lang>:<word_lower>"` (`personal_dict` / `[lang.<id>.personal_dict]`)
    - `"block:<typed_lower>-><rejected_lower>"` (`blocklist`)
    - `"pt:<raw_ascii_lower>"` (`telex_passthrough`)
  - Expose `RuleStore::classify_rule_confidence(&self, key: &str) -> (ConfidenceTier, Option<&RuleStat>, u32, u32)` combining stored + pending hits and undo/demote strikes.
- **Write-Once Provenance & Lifecycle Tracking (`store.rs`):**
  - Extend `log_rule_learned` (`DiagnosticEvent::RuleLearned`) across all machine-learned graduations (also resolving `findings-2026-11-11-event-logging-audit.md` §L3): `add_permanent_block` (`block_permanent`), `add_personal_word_with_stage` (including canonical casing upgrades), and `add_telex_passthrough` (`telex_passthrough_2x`). Keep write-once `# learned 2x ...` suffix comments on `[corrections]` and `[context_corrections]`, and **never rewrite `rules.toml` when an existing rule fires**.
  - Stamp `rule_stats` entries with `origin` (`Manual` for explicit CLI setters `set_user_correction`, `set_user_context_correction`, `add_user_personal_word`, `add_user_blocklist_pair`, `add_user_telex_passthrough`; machine `RuleOrigin` for learner graduations).
  - Update `RuleStore::clear_diagnostic_logs` (`store.rs`): explicitly **preserve `self.state.rule_stats`** so `smartkey logs clear` does not cause `rules prune` to mistake active rules for dead ones.
  - Update hit tracking:
    - `RuleStore::log_autocorrect_applied_with_context` (`store.rs`): increment `rule_stats` `hits` and `last_hit_epoch_ms` on `MatchKind::UserCorrectionRule`, `UserContextRule`, and `PersonalDict`.
    - `RuleStore::record_personal_word_use` (`store.rs`): in-memory bump on valid `personal_dict` hits, flushed lazily on focus reset / `Drop`.
- **Pruning API (`store.rs`):**
  - `RuleStore::prune_rules(&mut self, now_ms: u64, opts: &PruneOptions) -> io::Result<PruneReport>`:
    - Matches entries present in `rule_stats` where `(now_ms - stat.created_epoch_ms) >= opts.older_than_ms`, `effective_hits <= opts.max_hits`, and (`opts.include_manual || stat.origin.is_machine_learned()`).
    - Pre-existing rules with no `rule_stats` record are counted in `skipped_untracked` and never deleted blindly.
    - Removes matched items from `rules.toml` (`toml_edit::DocumentMut` + `CompiledRules`) and `rule_stats` when `!opts.dry_run`.
- **CLI Surface (`crates/smartkey-cli/src/main.rs`):**
  - `smartkey rules` (`summary`): prints `Rule Confidence      : <M> manual | <C> confirmed | <P> provisional | <S> suspect` with a tip toward `smartkey rules list --suspect` whenever suspect items exist.
  - `smartkey rules list [--suspect | --tier <suspect|provisional|confirmed|manual>] [--json] [--dir <path>]`: separates entries by confidence tier (`=== Confidence Breakdown ===` in default list, or filtered focus view for `--suspect` / `--tier`).
  - `smartkey rules prune [--older-than <Nd>] [--zero-hits | --max-hits <N>] [--kind <fix,context,word,block,passthrough|all>] [--include-manual] [--dry-run] [--json] [--dir <path>]` (safe defaults: `--older-than 30d --zero-hits --kind fix,context`, `include_manual = false`).

---

### F3 — Bounded `personal_dict` Usage Frequency Weighting

**Goal:** Replace the flat `freq = 175` assigned to every `personal_dict` word in `EnglishScorer` (`scorer.rs:1173`) with bounded, personalized frequency weighting derived from real usage — without logging normal dictionary words or writing to disk per word boundary.

- **Files:** `crates/smartkey-core/src/scorer.rs`, `crates/smartkey-core/src/store.rs`, `crates/smartkey-core/src/session.rs`
- **Functions & Trait Additions:**
  - `UserRuleView` (`scorer.rs:13`):
    ```rust
    fn personal_word_freq(&self, lower_word: &str) -> u8 {
        let _ = lower_word;
        175
    }
    ```
  - `RuleStore` (`store.rs`):
    - Implement `personal_word_freq(&self, lower_word: &str) -> u8`: reads `rule_stats["word:<lang>:<lower_word>"].hits` and computes a bounded logarithmic boost in `175..=225`:
      `175u8.saturating_add((8.0 * (1.0 + hits as f32).ln()).round() as u8).min(225)`
      (cap at `225` prevents a personal word from overriding high-frequency dictionary words like `"the"`/`"and"` on unrelated `d=1` typos).
    - `pub fn record_personal_word_use(&mut self, lang: &LangId, lower_word: &str)`: updates `rule_stats` (`hits += 1`, `last_hit_epoch_ms = Some(now)`) **in memory only** and sets `self.dirty_rule_stats = true`.
    - `pub fn flush_dirty_state(&mut self) -> io::Result<()>`: flushes `learner_state.json` only when `dirty_rule_stats` is true; called from `SmartKeySession::reset` (`session.rs:734`, i.e., focus-out / window switch) and `Drop for RuleStore` (best-effort). Normal `flush_state()` clears `dirty_rule_stats`.
  - `EnglishScorer::eval_internal` Step 5f (`scorer.rs:1170-1177`): pass `rules.personal_word_freq(pword)` instead of `175`.
  - `SmartKeySession` (`session.rs`): when an untouched valid word hits `in_personal == true` (in `commit_active` / `eval_word`), call `engine.store.record_personal_word_use(&self.lang, &core_lower)`.
- **Invariants:** Zero disk I/O per committed word; zero storage of non-personal words; existing `tests/corpus` valid-word behavior untouched.

---

### F4 — Staged Learning Inspector (`smartkey rules staged`)

**Goal:** Expose the pending (`1x`) staging queues already tracked in `LearnerState` (`staged_corrections`, `staged_context`, `staged_undo_keep`, `staged_contraction_rejects`, `staged_telex_passthrough`, `untouched_unknowns`) so users can inspect what is close to graduating before it lands in `rules.toml`.

- **Files:** `crates/smartkey-cli/src/main.rs` (read-only formatting/JSON serialization over `store.state()`), optional helper `RuleStore::staged_summary(&self)` in `store.rs`.
- **CLI Surface:**
  - `smartkey rules staged [--json] [--dir <path>]`
  - Lists each pending queue with current count vs. graduation threshold (`1/2`, `2/3`, etc.) and staged canonical casing if present.

---

## 4. Tier C — Verified Anti-Patterns (Do NOT Adopt)

| Anti-pattern | Where in EkaKey | Why SmartKey Rejects It |
|---|---|---|
| Keystroke replay + async self-ignore counters (`_ignoreKeysForClearing`) | `lib/ekakey_process.dart:350,411,452` | Global hook state must count its own injected keys to avoid self-triggering. SmartKey's per-`InputContext` IME path uses synchronous `deleteSurroundingText` (or fallback `forwardKey(BackSpace)`) + `ScopedSelfCommit` `commitString` (`adapters/fcitx5-smartkey/src/smartkey-addon.cpp:433-452`). |
| Blind phonetic best-match without valid-word / cost checks | `lib/ekakey_process.dart:441-459` | Rewrites valid OOV names, slang, and technical terms to the nearest phonetic dictionary neighbor. |
| Passive listen + post-space rewrite (`WH_KEYBOARD_LL`) | `windows/runner/main.cpp:70` | Races fast typists, flickers, and lacks password/URL/terminal field capabilities (`CapabilityFlag`). |
| SQLite as primary user rule store | `user_corrections.db` | Opaque binary DB, non-diffable. Plain-text comment-preserving `rules.toml` + `learner_state.json` stays Git/Syncthing-friendly. |
| 26-char `^[a-zA-Z]+$` dictionary sanitizer | `bin/sanitize_dictionary.dart` | Destroys contractions (`don't`, `I'm`) and every non-ASCII script (Vietnamese). |
| 50ms debounce per keystroke | `delayBeforeSearch`, `lib/ekakey_process.dart:92` | Unnecessary latency workaround for per-keystroke Flutter + SQLite roundtrips; SmartKey scores at word boundaries within a sub-20µs budget. |
