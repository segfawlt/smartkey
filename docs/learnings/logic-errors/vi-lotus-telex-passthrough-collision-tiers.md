---
title: "3-Tier vi-lotus Telex Passthrough Collision Prevention and Learner Guards"
category: logic-errors
module: crates/smartkey-core/src/session.rs
tags: [vi-lotus, telex, passthrough, collision, learner, undo]
problem_type: logic-error
date: 2026-11-25
---

# 3-Tier `vi-lotus` Telex Passthrough Collision Prevention and Learner Guards

## Problem

Typing an English double-key Telex escape twice in `fcitx5-lotus` mode (`Didd ` → `Did`, `iss ` → `is`, `thiss ` → `this`) silently auto-graduated that word into unconditional `telex_passthrough` in `rules.toml`. Because `vietnamese.cm.dict` is an unweighted 7,886-entry syllable list containing both common Vietnamese words (`đi`, `í`, `thí`) and impossible single-vowel prefix artifacts (`ắ`, `ằ`, `ấ`), unconditional `telex_passthrough` hijacked everyday Vietnamese typing (`anh đi` → `anh did`, `nhà í` → `nhà is`). Meanwhile, `maybe_record_vi_passthrough_repair` also poisoned pure Vietnamese Telex edits (`thì` → BackSpace → `thif`) into `staged_telex_passthrough`, and undoing a `telex_passthrough` replacement never removed the machine-learned rule.

## Symptoms

- Typing `anh did ` in `vi-lotus` mode rewrote `đi` into `did ` because `"did"` sat in `telex_passthrough` and `should_passthrough_telex` matched it unconditionally before checking context.
- Typing `"like ắ"` or `"aws"` triggered `ắ` (`vietnamese.cm.dict` includes 74 single-char symbols), unless `"aws"` was manually or passively added to `telex_passthrough`.
- Backspacing a composed Vietnamese word and retyping its Telex keys (`thì` → BS → `thif`) incremented `staged_telex_passthrough` for `thif`, `cos`, `minhf`, `cais`, `chuwa`, `ddor`, `choongf`, and `mayas` because `maybe_record_vi_passthrough_repair` fell through to `else if new_buf.is_ascii()` even when `buf_sim.is_composed_vietnamese` was `true`.
- Reverting a `telex_passthrough` rewrite via `2x BackSpace + Space` added a `blocklist` pair (`{ typed = "đi", rejected = "did" }`) without removing `"did"` from `telex_passthrough` or `staged_telex_passthrough`.

## Solution

1. **Tier 1 — Auto-restore impossible standalone Vietnamese outputs (`is_impossible_standalone_vi_syllable`):**
   - In [vi_telex.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/vi_telex.rs), treat words ending in toned open `ă`/`â` vowels (`ằ, ắ, ẳ, ẵ, ặ, ầ, ấ, ẩ, ẫ, ậ`) as phonologically impossible standalone Vietnamese syllables. Keep toneless `ă`, `â`, and `đ` excluded so `"chữ ă"`, `"chữ â"`, `"chữ đ"`, and `"aw"` stay intact.
   - In `SmartKeyEngine::should_passthrough_telex` ([session.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/session.rs)), when `is_impossible_standalone_vi_syllable(composed_lower)` holds and `is_known_english_or_personal_for_passthrough(&lower)` is true (valid English lexicon or global `personal_dict`, excluding `[lang.vi].personal_dict` and Vietnamese slang), restore raw ASCII (`"ắ"` → `"aws"`) in all contexts while still honoring `is_pair_blocked`.
2. **Tier 2 — Stage passive double-key escapes for TUI review without silent auto-graduation:**
   - Added `stage_telex_passthrough_candidate` in [learner.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/learner.rs) and routed passive double-key escapes (`Didd `, `themee `) and repair detections through `SmartKeySession::maybe_stage_vi_passthrough_candidate` instead of `record_telex_passthrough_repair`.
   - Real bilingual collisions (`did`/`đi`, `is`/`í`, `this`/`thí`, `now`/`nơ`, `theme`/`thêm`) now preserve Vietnamese at sentence start and after Vietnamese words, rely on the English-Sentence Momentum Shield mid-English (`what did`, `is this`, `right now`), and require explicit TUI/CLI approval before activating unconditionally.
3. **Tier 3 — Block composed-Vietnamese & non-English Telex poisoning + add undo demotion:**
   - Fixed `maybe_record_vi_passthrough_repair` to allow `new_buf.is_ascii()` fallback only when `!self.lotus_composing`, and required `is_known_english_or_personal_for_passthrough` in `maybe_stage_vi_passthrough_candidate` so pure Vietnamese Telex sequences (`thif`, `thiff`, `cos`, `minhf`) never stage.
   - Added `RuleStore::remove_telex_passthrough_if_machine_learned` in [store.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/store.rs) and invoked it from `record_undo_keep_verbatim` when reverting to a Vietnamese-rune token, deleting `ManualFix2x` `telex_passthrough` rules and clearing `staged_telex_passthrough` while leaving user-authored `RuleOrigin::Manual` entries intact (still shadowed by the permanent `blocklist` pair).
   - Added a CLI collision notice on `smartkey rules add passthrough`, filtered already-active entries in `smartkey rules staged`, and surfaced the colliding Vietnamese syllable inside `smartkey tui` Review.

## Why This Works

- Keeping `dicts/vi/vietnamese.cm.dict` aligned 1:1 with upstream `fcitx5-lotus` preserves exact preedit/break simulation while SmartKey's English lexicon cleanly distinguishes `"aws"` → `"ắ"` (`valid_word`) from `"awf"` → `"ằ"` (`bypass`).
- Staging passive escapes instead of silently mutating `rules.toml` eliminates invisible hijacking of high-frequency Vietnamese words while keeping `on_lotus_passthrough_repair` intact for callers that explicitly request graduation.

## Prevention

- Never auto-graduate passive ASCII escapes into unconditional `telex_passthrough`; keep `telex_passthrough` gated by explicit approval or unambiguous Tier 1 phonological impossibility.
- Guard every `vi-lotus` passthrough learning path with `!lotus_composing` for raw-ASCII fallback and `is_known_english_or_personal_for_passthrough`, covered by the regression suite in [vi_lotus_eval.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/tests/vi_lotus_eval.rs).
- Related: [Safe `vi-lotus` Sentence-Start English Passthrough Boundaries](./vi-lotus-sentence-start-english-passthrough.md).

## References

0
