---
title: "Vietnamese Telex Rescue Strategy Ordering and Simulation Gates"
category: logic-errors
module: crates/smartkey-core/src/vi_telex.rs
tags: [vi-telex, lotus, rescue, false-positive, testing]
problem_type: logic-error
date: 2026-11-24
---

# Vietnamese Telex Rescue Strategy Ordering and Simulation Gates

## Problem

Adding omitted-vowel-mark (`tienf` → `tiền`, `buocs` → `bước`, `dduocj` → `được`) and dropped-`-ng` (`nhuwg` → `nhưng`, `duowg` → `dương`, `hoafg` → `hoàng`) rescue rules to `rescue_telex_typo_with_dict` in `crates/smartkey-core/src/vi_telex.rs` exposed four subtle interactions with `LotusSimEngine`, existing Strategy 5 (`TELEX_FUNCTIONAL_KEYS`), and upstream session shields.

## Symptoms

- `tienf` was **not** handled natively by Lotus: it composed `tièn` (missing from `vietnamese.cm.dict`), triggered restore fallback, and existing Strategy 5 rewrote its final tone `f -> r` into the dialectal outlier `tiẻn`.
- Unrestricted mark insertion on bare toneless ASCII corrupted Vietnamese names and unaccented words (`Nguyen` → `Nguyên`, `Duong` → `Dương`, `Tien` → `Tiên`, `Tuan` → `Tuân`) because those unmarked spellings are absent from `vietnamese.cm.dict`.
- Strategy 5 also turned `nguoif` → `nguoiw` → `ngươi` via final `f -> w` substitution, contradicting an initial assumption that `nguoif` would yield two conflicting mark candidates and return `None`.
- For incomplete diphthong stems like `"duow"`, `simulate_lotus_word("duow")` composed `"dươ"` mid-word yet flattened `committed_at_break` back to `"duow"`, reporting `is_composed_vietnamese == false` and `needs_uinput_restore_on_break == true`.
- Asserting raw `rescue_telex_typo(word) == None` over `tests/corpus/must_not_touch.txt` panicked on `"OSes"` because pre-existing Strategy 2 matches `ose -> oé` when called without `SmartKeyEngine::evaluate_vi_lotus_token`'s upstream English lexicon shield.

## What Didn't Work

- Placing single-mark insertion blindly before Strategy 5 would change `duongx -> duongw -> dương` into `dưỡng`; restricting Strategy 7 to `prefix_sim.is_composed_vietnamese` silently dropped `duowg -> dương`.
- Probing `moi`, `nina`, or `cola` as negative unit tests for new rules is vacuous (`moi` is already in `vietnamese.cm.dict`; `nina` and `cola` are shielded upstream by the English lexicon).

## Solution

1. **Gate Strategy 5a and Strategy 6 on mid-word Vietnamese composition (`has_midword_vi_rune`):**
   - Compute `let has_midword_vi_rune = orig_sim.needs_uinput_restore_on_break || orig_sim.is_composed_vietnamese;`. This admits both restore-class tokens (`tienf`, `buocs`) and `ddFreeStyle` `đ`-words (`ddiems`, `dduocj`, where `must_fallback_to_english()` returns `false`), while rejecting bare toneless ASCII (`Nguyen`, `Duong`, `Tien`).
2. **Split ordering around Strategy 5 (`try_ie_ye_circumflex_rescue` before, `try_unique_single_mark_rescue` after):**
   - Run Strategy 5a (`ie`/`ye` + coda `c|m|n|ng|p|t|u` + trailing strict tone key `s|f|r|x|j`) **before** Strategy 5 so `tienf` rescues to `tiền` instead of Strategy 5's `tiẻn`.
   - Keep Strategy 5 (`duongx -> dương`, `duongj -> dương`) unchanged, and run Strategy 6 (unique 1-mark completion, returning `Some` only when `distinct_matches.len() == 1`) **after** Strategy 5 so checked-coda syllables (`-c`, `-p`, `-t`, `-ch`, which never take level tone) rescue when unambiguous (`buocs -> bước`, `dduocj -> được`) and stay untouched when ambiguous (`nuocs`, `thuocs`, `duocj`, `luots`, `truotj`).
3. **Check `is_composed_vietnamese || needs_uinput_restore_on_break` on Strategy 7 prefixes and repair raw Telex:**
   - When `n >= 3 && bytes[n - 1] == b'g' && bytes[n - 2] != b'n'`, simulate `prefix = &lower[..n - 1]` and require `prefix_sim.is_composed_vietnamese || prefix_sim.needs_uinput_restore_on_break`, then run `try_candidate_repair(&format!("{prefix}ng"), ...)` so `duowg -> dương`, `nhuwg -> nhưng`, and `hoafg -> hoàng` (preserving tone relocation) work while English `-g` words (`bag`, `thing`, `config`, `frog`) never match.
4. **Verify `must_not_touch.txt` through `SmartKeyEngine::evaluate_token`:**
   - Keep corpus zero-false-positive assertions in `crates/smartkey-core/tests/vi_lotus_eval.rs` on `engine.evaluate_token(word, ...)` (so upstream English/casing shields run alongside `rescue_vi_telex`).

## Why This Works

- Requiring mid-word Vietnamese composition (`has_midword_vi_rune`) guarantees that plain ASCII names and unaccented Vietnamese text never get forcibly accented.
- Ordering 5a → 5 → 6 preserves existing `w`-substitution behavior while eliminating the `tienf -> tiẻn` hijack and preventing 2-way checked-coda guesses.
- Re-simulating raw `{prefix}ng` lets `LotusSimEngine::refresh_last_tone_target` compute exact tone placement instead of naively appending `"ng"` after already-composed text.
- Related: [Safe `vi-lotus` Sentence-Start English Passthrough Boundaries](./vi-lotus-sentence-start-english-passthrough.md).

## Prevention

- Never run any `vi_telex.rs` mark-insertion rule on bare ASCII without `needs_uinput_restore_on_break || is_composed_vietnamese`.
- Keep `test_rescue_omitted_ie_ye_circumflex_and_unique_mark_without_bare_ascii_fp`, `test_rescue_dropped_n_before_trailing_g`, and `test_vi_lotus_omitted_mark_and_dropped_ng_with_undo_and_corpus_shield` in `cargo test` (`vi_telex.rs` and `vi_lotus_eval.rs`).
- When planning future Strategy 8 (compound splitting), validate candidate splits against `must_not_touch.txt` (`macOS`, `APIs`, `soooo`) and guard mixed-case, 3x stretch tokens, and short open-syllable collisions.

## References

0
