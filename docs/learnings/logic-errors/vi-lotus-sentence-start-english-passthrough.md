---
title: Safe vi-lotus Sentence-Start English Passthrough Boundaries
category: logic-errors
module: crates/smartkey-core/src/session.rs
tags: [vi-lotus, telex, passthrough, vietnamese]
problem_type: design-decision
date: 2026-11-16
---

# Safe `vi-lotus` Sentence-Start English Passthrough Boundaries

## Context

`SmartKeySession::should_passthrough_telex` shields English words whose trailing/medial letters look like Telex tone or mark keys (`of -> ò`, `or -> ỏ`, `if -> íf`). Previously it required `ctx.prev_word.is_some()`, leaving sentence-start capitalized English words vulnerable to Vietnamese diacritic corruption.

## Guidance

1. Allow sentence-start restoration (`ctx.prev_word.is_none() && ctx.at_sentence_start`) **only** for TitleCase `"If"`, `"Of"`, and `"Or"`.
2. Keep `"As"`, `"Is"`, `"Was"`, `"Has"`, and `"His"` shielded when standalone/sentence-start because they collide with legitimate Vietnamese words starting sentences (`"Á"` in `"Á hậu"`, `"Í"` in `"Í thôi"`, `"Há"` in `"Há miệng"`, `"Hí"` in `"Hí hửng"`).
3. Remember `W2uMode::Default` in `LotusOptions`: leading `w` (`"Was"`) does **not** become `"Ưa"` by default anyway, so do not use `"Was "` expecting Vietnamese `"Ưa"` behavior in default-mode regression tests.

## Why This Matters

Blindly adding lowercase `"if"/"of"/"or"` or extending sentence-start passthrough to all ambiguous Telex words breaks real Vietnamese sentences (`"Ít nhất..."`, `"Á hậu..."`, `"Há miệng..."`).

## When to Apply

Any change to `should_passthrough_telex` in `crates/smartkey-core/src/session.rs` or bilingual Telex starter tests in `crates/smartkey-core/tests/vi_lotus_eval.rs`.

## Examples

- Sentence start `"If "` under `vi-lotus`: restores `"Íf"` -> `"If"` (tested via `smartkey explain --lang vi --sentence-start If`).
- Sentence start `"As haauj "` / `"Is thooi "` / `"Has mieengj "`: remains Vietnamese (`"Á hậu "`, `"Í thôi "`, `"Há miệng "`).

## References

0
