---
title: "Stepwise BackSpace Undo Across fcitx5-lotus PreInputMethod and PostAutocorrectOrigin"
category: integration-issues
module: crates/smartkey-core/src/session.rs
tags: [fcitx5, vi-lotus, undo, pending-dot, state-machine]
problem_type: integration-issue
date: 2026-11-18
---

# Stepwise BackSpace Undo Across `fcitx5-lotus` `PreInputMethod` and `PostAutocorrectOrigin`

## Problem

Changing post-autocorrect `BackSpace` undo to `settings.undo_mode = "step"` (1st `BackSpace` deletes only the trailing boundary `' '` via `PassThrough`, leaving `JustAutocorrected` with `trailing_boundaries == ""` so the user can append `.`, `,`, `s`, or `.json`; 2nd `BackSpace` snaps back to `raw_typed`) introduces two state-machine hazards across `SmartKeySession` (`crates/smartkey-core/src/session.rs`) and `SmartKeyAddon` (`adapters/fcitx5-smartkey/src/smartkey-addon.cpp`):

1. **Fcitx5 `fcitx5-lotus` BackSpace ordering:** After 1st `BackSpace` deletes `' '` in Vietnamese mode (`"tiếng "` -> `"tiếng"`), the cursor rests directly against `"tiếng"` while `mode == "JustAutocorrected"` and `trailing_count == 0`. Deferring the 2nd `BackSpace` to `onPostKeyEvent` (`PostInputMethod`) as with normal `BackSpace` lets `LotusState::keyEvent` intercept the key and backspace into `"tiếng"` first.
2. **Post-autocorrect `.` / `:` / `;` lookahead re-scoring & spurious learning:** Once `trailing_boundaries` is empty, typing `.` or `;` must support both prose punctuation (`"resource. "`, `"resource; "`) and domain/extension continuation (`"resource.json "` -> `Composing`), without routing back through `finish_token_at_boundary_inner` (which would re-score an already-corrected token or run `simulate_vi_lotus` on non-ASCII `"tiếng"` inside `on_lotus_boundary_commit`), and without recording a fake `ManualBackspaceFix` or `untouched_unknown` if `<ENTER>` / `<BS>` interrupts before the lookahead resolves.

## Symptoms

- Deferring `2nd BackSpace` under `fcitx5-lotus` when `trailing_count == 0` causes Lotus to consume/modify the corrected Vietnamese word before SmartKey's `Replace { delete_chars, insert: raw_typed }` executes.
- Routing `PendingTrailingDot` from `JustAutocorrected` through the regular `Composing` boundary path re-evaluates `"tiếng"` / `"resource"` as a newly typed word or mis-commits context state.

## What Didn't Work

1. **Adding new FFI functions to query stepwise undo readiness:** Unnecessary ABI expansion — `SmartKeySessionSnapshot` (`crates/smartkey-ffi/include/smartkey.h`) already exposes `mode_utf8` and `trailing_count` (`trailing_boundaries.chars().count()`).
2. **Entering `Mode::Composing` directly when backspacing the trailing space of an autocorrection:** Breaks 2nd-`BackSpace` revert (`EditingRestored`), loses `JustAutocorrected` surrounding-text reconciliation, and treats `"resource."` as a brand-new composing token rather than preserving `raw_typed` and `applied`.

## Solution

1. **Attach `post_autocorrect: Option<PostAutocorrectOrigin>` to `Mode::PendingTrailingDot` and `Mode::PendingSemicolon`** (`crates/smartkey-core/src/session.rs`):
   - When `ch` is `.` / `:` / `;` inside `Mode::JustAutocorrected` with `trailing_boundaries.is_empty()`, transition to `PendingTrailingDot` / `PendingSemicolon` carrying `PostAutocorrectOrigin { raw_typed, prev_word_before, is_snippet }`.
   - On immediate boundary (`"resource. "` or via `on_lotus_boundary_commit`), `finish_post_autocorrect_pending_punct` calls `update_context_after_commit` (setting `at_sentence_start = true` for `.`) and returns directly to `Mode::JustAutocorrected` with `trailing_boundaries = ". "` without calling `finish_token_at_boundary_inner` or `simulate_vi_lotus`.
   - On non-boundary continuation (`"resource.json"`), `finish_post_autocorrect_pending_punct` merges into `Mode::Composing`; on `BackSpace`, it returns to `JustAutocorrected { trailing_boundaries: "" }`; on `<ENTER>`/`<NAV>` (`commit_active_without_autocorrect`), it updates context without recording manual edits or `untouched_unknowns`.
2. **Handle `SMARTKEY_KEY_BACKSPACE` in `PreInputMethod` (`SmartKeyAddon::onKeyEvent`) when `lotusActive` and `trailing_count == 0`:**
   - Guard on `!boundaryAbortedNav && !state->hasPendingBoundary() && !state->hasLotusActivityInWord()`, query `smartkey_session_debug_snapshot(state->session(), &snap)`, and when `mode_utf8 == "JustAutocorrected" && snap.trailing_count == 0`, set `setSuppressPostKey(true)` and execute `smartkey_session_process_key(..., SMARTKEY_KEY_BACKSPACE, ...)` before Lotus sees the key.

## Why This Works

- Keeping `1st BackSpace` in `PostInputMethod` preserves normal client/Lotus handling when deleting trailing whitespace (`trailing_count > 0`), while `PreInputMethod` interception when `trailing_count == 0` ensures SmartKey deletes the exact `applied.chars().count()` scalars atomically before Lotus can nibble the restored word.
- `PostAutocorrectOrigin` keeps sentence-start capitalization, multi-step backspace (`"resource. "` -> `<BS><BS><BS>` -> `"reosurce"`), and `.json` merges consistent without double-scoring or corrupting non-ASCII Vietnamese state.
- Related: [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./surrounding-text-undo-guard-async-echo-windows.md), [Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle](../architecture-patterns/fcitx5-flight-recorder-ffi-and-fd-contract.md).

## Prevention

- Keep `undo_mode_step_supports_post_autocorrect_punctuation_and_suffix_edits` and `undo_mode_immediate_and_off_behave_per_setting` (`crates/smartkey-core/tests/learning_eval.rs`), `step_undo_punctuation_and_suffix_edits` (`tests/corpus/learning_traces.toml`), and the `.` punctuation step test in `test_vi_lotus_backspace_undo_and_modular_plugin_toggle` (`crates/smartkey-core/tests/vi_lotus_eval.rs`).
- Never match `Mode::PendingTrailingDot` / `PendingSemicolon` without explicitly checking `post_autocorrect` inside `handle_char`, `handle_backspace`, `commit_active_without_autocorrect`, and `on_lotus_boundary_commit`.

## References

0
