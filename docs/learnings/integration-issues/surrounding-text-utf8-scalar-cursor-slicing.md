---
title: "SurroundingText Unicode-Scalar Cursor Indexing vs UTF-8 Slicing"
category: integration-issues
module: adapters/fcitx5-smartkey/src/smartkey-addon.h
tags: [fcitx5, surrounding-text, utf8, vietnamese, wayland]
problem_type: integration-issue
date: 2026-11-18
---

# SurroundingText Unicode-Scalar Cursor Indexing vs UTF-8 Slicing

## Context

While building [`extractSurroundingSlice`](../../adapters/fcitx5-smartkey/src/smartkey-addon.h) to capture live cursor windows (`scr`) around Vietnamese text such as `"cần |sinh"`, Fcitx5's `InputContext::surroundingText()` contract creates a recurring offset mismatch trap.

## Guidance

- `fcitx::SurroundingText::cursor()` and `anchor()` (and `InputContext::deleteSurroundingText()`) measure **Unicode scalar (codepoint) counts**, whereas `SurroundingText::text()` is a **UTF-8 `std::string`**.
- **Never** index or `.substr()` `st.text()` directly using `cursor()` / `anchor()` byte offsets: multi-byte Vietnamese diacritics (`ầ` = 3 UTF-8 bytes, `ư` = 2 bytes, `đ` = 2 bytes) shift every subsequent codepoint offset and risk splitting inside a UTF-8 sequence.
- Walk UTF-8 codepoint lead bytes sequentially up to `cursorChars`, retain only a small ring buffer of the last `maxBefore + 1` byte offsets to extract the window before the cursor, validate bounds (`"<invalid-cursor>"`), and short-circuit oversized editor documents (`cursorChars > 16384`) to guarantee O(1) event-loop latency.

## Why This Matters

Splitting a multi-byte UTF-8 rune produces invalid JSON/UTF-8 strings and points the debugger at the wrong character boundary when analyzing off-by-one deletes (`delete_chars`) or phantom leading-letter concatenation (`"cần sinh"` -> `"nsinh"`).

## When to Apply

Any C++ or Rust code inspecting `SurroundingText::cursor()`, `anchor()`, or mapping `delete_chars` back to `SurroundingText::text()`. See also [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./surrounding-text-undo-guard-async-echo-windows.md).

## Examples

In `text = "cần sinh"` (`10` UTF-8 bytes, `8` Unicode scalars), `cursor = 4` (`"cần "`) falls at byte offset `6`, producing `"cần |sinh"` rather than slicing mid-codepoint. Verified in `verifyTraceRecorderHelpers()` (`adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp`).

## References

0
