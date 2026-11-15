---
title: Scorer Step 5a Curated Typo Metadata Filtering and Test Isolation
category: logic-errors
module: crates/smartkey-core/src/scorer.rs
tags: [scorer, dialect, false-positive, testing]
problem_type: logic-error
date: 2026-11-16
---

# Scorer Step 5a Curated Typo Metadata Filtering and Test Isolation

## Problem

In `EnglishScorer`, Step 5a (curated typo dictionary lookups) skipped lexicon metadata validation and fell back to `meta.freq.unwrap_or(145)`. All later candidate sources (fuzzy FST Steps 5b/5d and smash rescue Step 5e) enforced `!meta.is_offensive()`, `meta.is_valid_for_dialect(config.dialect)`, and presence in the lexicon (`freq > 0`). As a result, a curated typo mapping pointing to a wrong-dialect word (e.g., UK-only word under `EnUs`), an offensive word, or a word absent from the lexicon could still autocorrect.

## Symptoms

- Curated typo pairs bypassed dialect enforcement (`EnUs`, `EnGb`) and the offensive-word filter.
- Missing lexicon targets silently received a hardcoded fallback frequency of `145` instead of being rejected.

## What Didn't Work

Testing Step 5a filtering in isolation with a synthetic distance-2 typo on a 7+ letter target (e.g. a UK-only word `britword` with a d=2 typo) failed to isolate Step 5a because Steps 5b (`lev2`) / 5e (`smash_entries`) legitimately picked up valid targets at low edit distance whenever Step 5a filtered the typo entry out — masking both false positives and filter correctness.

## Solution

1. In `crates/smartkey-core/src/scorer.rs` Step 5a, require `self.lexicon.lookup_word(mapped)` to return `Some(meta)` with `meta.freq > 0`, `!meta.is_offensive()`, and `meta.is_valid_for_dialect(config.dialect)` before emitting the curated candidate (removing `.unwrap_or(145)`).
2. In `crates/smartkey-core/tests/corpus_eval.rs` (`step5a_curated_typos_enforce_metadata_and_dialects_stay_green`), isolate Step 5a with a synthetic `.skdict` typo unreachable by Steps 5b–5e (`birktu -> brituk`: length 6, Damerau–Levenshtein distance 3), verifying it fixes under `Mixed` / `EnGb` and stays `Unchanged` under `EnUs`.

## Why This Works

Every candidate source in Layer 1 now shares the exact same lexicon metadata gate (`freq > 0`, `!is_offensive()`, `is_valid_for_dialect()`), closing the single remaining Layer 1 false-positive / dialect-leak vector. Using a DL=3 length-6 token in tests guarantees only Step 5a can propose the target.

## Prevention

- Any future candidate step added to `EnglishScorer` must apply the same `lookup_word` metadata + dialect + non-offensive gate (never fabricate a default `freq`).
- When unit-testing a single candidate step in `corpus_eval.rs`, ensure the test token cannot match `lev1`/`lev2`/`chatter`/`smash` fallbacks.

## References

0
