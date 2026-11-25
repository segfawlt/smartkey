---
title: Guard untouched_3x Against Built-In Contractions and 2-Char Noise Without Breaking TitleCase Tech Words
category: logic-errors
module: crates/smartkey-core/src/learner.rs
tags: [learner, personal-dict, contractions, casing, untouched-3x]
problem_type: logic-error
date: 2026-11-25
---

# Guard `untouched_3x` Against Built-In Contractions and 2-Char Noise Without Breaking TitleCase Tech Words

## Problem

`record_untouched_unknown()` in [crates/smartkey-core/src/learner.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/learner.rs#L1120-L1290) only checked `lexicon.contains_any_variant_or_elided()` and `lookup_known_typos()` before staging unknown tokens for `3x -> personal_dict` graduation. Missing-apostrophe contractions, built-in shorthand triggers, and 2-character typing fragments passed through and graduated into `personal_dict`.

## Symptoms

- Missing-apostrophe contractions such as `didnt` graduated via `untouched_3x` when typed in bypassed or non-autocorrected contexts, and once in `personal_dict` (`Step 3` runs before `Step 3b` contractions in `scorer.rs`) permanently suppressed `didnt -> didn't`.
- Short 2-character slip fragments (`od`, `sp`, `aj`, `ot`, `si`, `se`, `ke`) accumulated into `personal_dict` without ever being confirmed by an undo-keep action.

## What Didn't Work

Restricting `is_cased_tech_word` (lines 1147–1151) from `has_upper` to `has_internal_upper` in order to keep plain TitleCase `dev_terms` entries (`Discord`) out of `personal_dict` breaks `learning_eval.rs:1139-1201`, which intentionally requires TitleCase `dev_terms` (`Hyprland`) to graduate canonical casing on `3x` untouched use. Keep `is_cased_tech_word` intact.

## Solution

Added two `!has_prior_undo` guards inside `record_untouched_unknown()` in [crates/smartkey-core/src/learner.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/src/learner.rs#L1217-L1233):

```rust
if char_count < 3 && !has_prior_undo {
    return LearnEvent::None;
}
if !has_prior_undo
    && lexicon.is_some_and(|l| {
        l.lookup_known_typos(&lower).is_some()
            || l.rules().replacements.iter().any(|r| r.typed == lower)
            || l.lookup_shorthand(&lower).is_some()
    })
{
    return LearnEvent::None;
}
```

## Why This Works

Gating both checks on `!has_prior_undo` stops passive `untouched_3x` pollution from contractions, shorthands, and 2-character noise while preserving `undo_keep_2x` escape hatches if the user explicitly undoes an expansion and keeps the raw token (e.g. `ng -> người` undone in `vi`). Keeping `is_cased_tech_word` untouched preserves canonical-casing graduation for single-cap `dev_terms` like `Hyprland`.

## Prevention

Keep contraction/shorthand and minimum-length (`< 3`) checks aligned with `has_prior_undo` in `record_untouched_unknown()`, and verify both blocked tokens (`didnt`, `od`, `sp`, `aj`) and 3-character OOV retention (`dux`) in `vi_syllable_validation_untouched_guards_trailing_quote_and_ai_shift_slip` in [crates/smartkey-core/tests/learning_eval.rs](file:///home/domixi/Projects/smartkey/crates/smartkey-core/tests/learning_eval.rs#L2736-L2762).

## References

0
