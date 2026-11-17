---
title: Surrounding-Text Undo Guard and Async Lotus Echo Windows
category: integration-issues
module: adapters/fcitx5-smartkey/src/smartkey-addon.cpp
tags: [fcitx5, vi-lotus, undo, surrounding-text]
problem_type: integration-issue
date: 2026-11-16
---

# Surrounding-Text Undo Guard and Async Lotus Echo Windows

## Problem

After an autocorrection, moving the text cursor away with the mouse (`SurroundingTextUpdated` without a synthetic keyboard `Left`/`Click` event) did not always invalidate undo state before the user pressed `BackSpace`:

1. `onSurroundingTextUpdated` ignored Vietnamese `fcitx5-lotus` contexts (`isLotusActive`) entirely.
2. `SmartKeySession::reconcile_surrounding` left displaced cursors armed in some states (`Mode::JustAutocorrected`, `Mode::AfterWord`, and `Mode::Composing` / `EditingRestored` / `EditingPrevious`) and only tuned an `80 ms` echo guard for English typing.
3. Conversely, unconditionally calling `reconcile_surrounding` during `vi-lotus` uinput bursts or SmartKey's own delete+insert commits would misread lagging asynchronous compositor snapshots as cursor displacements.

## Symptoms

- Clicking elsewhere within `undo_window_ms` and pressing `BackSpace` could fire `Replace` (deleting characters and re-inserting the raw typo at the new cursor position).
- Under `fcitx5-lotus`, asynchronous `Pause`/Backspace bursts take longer to settle than synchronous English key commits (~`400 ms` vs `80 ms`).

## What Didn't Work

Simply forwarding every `onSurroundingTextUpdated` event to `reconcile_surrounding` with a fixed `80 ms` cutoff breaks both sides: Wayland/XIM `SurroundingTextUpdated` echoes during `fcitx5-lotus` bursts look like cursor jumps once past `80 ms`, wiping active state mid-word.

## Solution

1. In `crates/smartkey-core/src/session.rs` (`reconcile_surrounding`): use language-aware echo windows (`80 ms` for `en`, `400 ms` for `vi`) and reset via `NavOrClick` on cursor displacement across `Mode::JustAutocorrected`, `Mode::AfterWord` (when not `vi` + `lotus_composing`), and `Mode::Composing | EditingRestored | EditingPrevious`.
2. In `adapters/fcitx5-smartkey/src/smartkey-addon.cpp` (`onSurroundingTextUpdated`): run for both `isEnglishKeyboardActive(ic)` and `isLotusActive(ic)`, but return early whenever `state->isSelfCommitting()` or `(lotusActive && (state->isInLotusBurst(nowMs) || state->hasPendingBoundary()))`.

## Why This Works

Self-commits, Lotus bursts, and pending boundary commits are filtered before reaching `reconcile_surrounding`, while true user mouse clicks / external cursor moves after the echo guard reset the undo/shadow buffer cleanly in apps that support `CapabilityFlag::SurroundingText`. Note that apps without `SurroundingText` (e.g. legacy X11 XIM clients) still cannot detect mouse-only cursor moves between keystrokes.

## Prevention

- Keep `reconcile_surrounding_guards_displaced_cursor_undo` (`learning_eval.rs`) and `test_vi_lotus_sentence_start_english_passthrough_and_surrounding_undo_guard` (`vi_lotus_eval.rs`) in CI.
- Never remove the `isSelfCommitting()` / `isInLotusBurst()` / `hasPendingBoundary()` guards from `onSurroundingTextUpdated`.
- Related: [SurroundingText Unicode-Scalar Cursor Indexing vs UTF-8 Slicing](./surrounding-text-utf8-scalar-cursor-slicing.md).

## References

0
