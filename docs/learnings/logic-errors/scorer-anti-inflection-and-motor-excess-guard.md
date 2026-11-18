---
title: "Scorer Relational Stem-Dominance and Motor-Excess Bigram Guard"
category: logic-errors
module: crates/smartkey-core/src/scorer.rs
tags: [scorer, inflection, smash-rescue, bigram, false-positive]
problem_type: logic-error
date: 2026-11-18
---

# Scorer Relational Stem-Dominance and Motor-Excess Bigram Guard

## Problem

In `LatinScorer::eval_internal` (`crates/smartkey-core/src/scorer.rs`), motor-faithful typos of base words were hijacked into unattested past-tense, plural, or participle inflections (or tie-blocked into `UNCHANGED`) whenever context or lexicon frequency favored the inflected form:

1. **Asymmetric `SmashRescue` `freq >= 105` cliff:** Step 5e ran on 7–9 character `dl <= 2` typos already found by `FuzzyLexicon`, adding `rescue_bonus = 0.30 * overlap` (~`+0.26`) to inflections above `smash_min_freq = 105` (`disabled` 116, `planned` 138, `compiled` 121) while `100%`-overlap stems below `105` (`disable` 92, `planner` 93, `compile` 98) got only `anagram_bonus = 0.20`.
2. **Undamped bigram overrides:** `bg_term = 0.95 * bg` applied regardless of motor distance delta, letting high-frequency passive/participle bigrams (`"is disabled"`, `"is observed"`, `"the planned"`, `"is required"`) beat or tie-block stems with less than half the keyboard edit cost.
3. **Clamped `SmashRescue` cost hides raw motor delta:** Step 5e clamped `damped_scramble_cost` to `raw_cost.min(1.15)` at high multiset overlap, erasing the motor gap between base stems and suffixed inflections.

## Symptoms

- `"is dsaible"` -> `"disabled"` (`smash_rescue`, `score=0.42` vs `"disable"` `score=0.24`, `edit=1.38` vs `0.58`)
- `"is obvserve"` -> `"observed"` (`edit=1.42` vs `"observe"` `edit=0.62`)
- `"the planenr"` -> `"planned"` (`edit=1.14` vs `"planner"` `edit=0.46`)
- `"is compiel"` -> `"compiled"` (`edit=1.26` vs `"compile"` `edit=0.46`)
- `"is reuqire"` -> `UNCHANGED` (`required=1.05` vs `require=1.02`, margin `0.03 < 0.04`)

## What Didn't Work

1. **Blind `-ed` / `-ing` / `-s` / `-ly` suffix penalties:** Penalizing all surface suffix endings regardless of candidate pool membership corrupts monomorphemic roots whose spellings happen to end in those letters (`spede -> speed`, `nothiong -> nothing`, `ttoally -> totally`).
2. **Blocking `SmashRescue` length extensions or gating `SmashRescue` on `best_so_far > 0`:** Fails on equal-length inflections (`is obvserve -> observed`, `the planenr -> planned`), misses `KnownTypoDict` hijacks (`is reuqire`), and breaks legitimate long scrambles (`everyhtihng -> everything` scores `-0.01` without `SmashRescue`).

## Solution

1. **Store true `raw_edit_cost`, `bg_term`, and `rescue_bonus` per `PooledEntry`** in `eval_internal` (using `weighted_keyboard_cost(&core_lower, cand_lower)` even when `SmashRescue` clamps `cand.edit_cost` to `1.15`), and require `raw_edit_cost <= 0.95` on `skip_heavy_dfa` so motor-distant single `KnownTypoDict` hits never skip `FuzzyLexicon` stems.
2. **Restrict Step 5e `SmashRescue` to `dl >= 3 || char_len >= 10`:** Eliminates the `freq >= 105` bonus cliff on 7–9 char `dl <= 2` typos while keeping `everyhtihng -> everything` (len 11, `dl=2`), `implementintion -> implementation` (len 15, `dl=2`), and `hstiautoin -> situation` (len 10, `dl=3`) intact.
3. **Motor-Excess Bigram Damping:** Across viable entries (`score >= t_auto`), compute `min_raw_edit`; when `excess = raw_edit_cost - min_raw_edit > 0.45`, scale `bg_term` by `(1.0 - (excess - 0.45) * 1.5).clamp(0.0, 1.0)`.
4. **English Relational Stem-Dominance Guard (`is_english_inflection_of`):** When `lexicon.header().lang == "en"` and both a viable `stem` (`score >= t_auto`) and its regular inflection (`-s/-es/-ies/-d/-ed/-ied/-ing`, consonant doubling, silent-`e` drop) coexist in `pool` with `cand.raw_edit_cost >= stem.raw_edit_cost + 0.45`, strip `cand`'s `bg_term + rescue_bonus` and cap `cand.score <= stem.score - min_margin - 0.01`.

## Why This Works

- Relational matching only suppresses an inflection relative to its own motor-cleaner stem in `pool`, so monomorphemic `-ed`/`-ing` roots (`nothing`, `speed`) and genuine inflected typos (`is dsaibled`, `is disablef`, `disabed`, `usin`) keep their full scores.
- Using un-clamped `raw_edit_cost` ensures long `SmashRescue` stems cannot be tie-blocked by their `-s`/`-ed` plurals or participles, while Motor-Excess Bigram Damping handles non-stem context hijacks (`"the planenr"` -> `"planner"` over `"planned"` without hurting `"over ther"` -> `"there"` or `"in ther"` -> `"their"` at `excess == 0.0`).
- Related: [Scorer Step 5a Curated Typo Metadata Filtering and Test Isolation](./scorer-step5a-metadata-and-dialect-filtering.md).

## Prevention

- Keep `english_inflection_helper_recognizes_regular_forms_and_rejects_unrelated_words` (`scorer.rs`) and `anti_inflection_guard_prefers_motor_clean_stems_while_preserving_true_inflections` (`crates/smartkey-core/tests/corpus_eval.rs`), plus `smartkey-dev eval` (`95/95` Must-Fix, `141/141` Must-Not-Touch).
- Never drop `dl >= 3 || char_len >= 10` back to unguarded `dl <= 2` or compare clamped `edit_cost` inside inflection/bigram dominance checks.

## References

0
