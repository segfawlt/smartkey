---
title: ImeBridge Packs Must Validate Learner Words Against syllable_dict and slang_set
category: logic-errors
module: crates/smartkey-core/src/learner.rs
tags: [learner, vi-lotus, ime-bridge, lexicon, validation]
problem_type: logic-error
date: 2026-11-25
---

# ImeBridge Packs Must Validate Learner Words Against `syllable_dict` and `slang_set`

## Problem

`PackLexicon::contains_any_variant_or_elided()` only checks `words_fst` and `clitics.prefixes`. For `PackTier::ImeBridge` packs such as Vietnamese (`vi`), `words_fst` is empty (`words = 0`) and valid vocabulary lives in `syllable_dict()` (`7,886` syllables) plus `slang_set()`. Calling `contains_any_variant_or_elided()` directly in `crates/smartkey-core/src/learner.rs` treated every Vietnamese syllable as an out-of-vocabulary word.

## Symptoms

- Manual Vietnamese repairs whose target had `< 4` characters (`"đ" -> "đi"`, `"các" -> "cá"`, `"cá" -> "các"`, `"trừu" -> "trừ"`) failed target validation and logged `skipped:target_not_valid_word`.
- Standard Vietnamese syllables and `slang.txt` entries passed through `record_untouched_unknown()` and polluted `[lang.vi] personal_dict` (`72` redundant entries accumulated in local config).
- Guard 2 (`ValidWordNoContext`) failed to recognize valid Vietnamese `orig` words when evaluating manual edits.

## What Didn't Work

Expanding `dicts/vi/slang.txt` alone only adds entries to `slang_set()` for `ImeBridge` packs (`pack_builder.rs` leaves `words_fst` empty when `syllable_dict = true`), so `contains_any_variant_or_elided()` still returned `false` for both `vietnamese.cm.dict` syllables and `vi/slang.txt` tokens.

## Solution

Added a private helper `is_lexicon_known_word(lex: &PackLexicon, lower: &str) -> bool` in [crates/smartkey-core/src/learner.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/learner.rs#L11-L16):

```rust
#[inline]
fn is_lexicon_known_word(lex: &PackLexicon, lower: &str) -> bool {
    lex.contains_any_variant_or_elided(lower)
        || (!lex.syllable_dict().is_empty()
            && crate::vi_telex::is_dict_word_in(lex.syllable_dict(), lower))
        || lex.is_slang_word(lower)
}
```

Replaced direct `contains_any_variant_or_elided()` checks across `resolve_restored_edit()`, `record_manual_edit_inner()`, `record_untouched_unknown()`, and `is_valid_prose_word_for_space()`.

## Why This Works

Data-tier packs (`en`, `es`, `fr`) populate `words_fst`, while `ImeBridge` packs (`vi`) populate `syllable_dict()` and `slang_set()`. Checking all three stores in one helper aligns Layer 2 learning validation (Guard 2 `orig` shield, `< 4`-char target validation, expressive-stretch stemming, untouched-unknown filtering, and space-split word checks) with how Layer 1 actually stores each pack's vocabulary.

## Prevention

Never call `PackLexicon::contains_any_variant_or_elided()` directly in learner or validation code when multi-language packs can be passed; use `is_lexicon_known_word()` and cover `ImeBridge` (`bundled_vi_lexicon()`) in `vi_syllable_validation_untouched_guards_trailing_quote_and_ai_shift_slip` inside [crates/smartkey-core/tests/learning_eval.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/tests/learning_eval.rs#L2690-L2734).

## References

0
