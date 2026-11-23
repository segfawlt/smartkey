---
title: "DBus KeyEventOrderFix Capability Check and Dropped-Delete Circuit Breaker"
category: integration-issues
module: adapters/fcitx5-smartkey/src/smartkey-addon.h
tags: [fcitx5, dbus, surrounding-text, gtk4, undo, wayland]
problem_type: integration-issue
date: 2026-11-18
---

# DBus `KeyEventOrderFix` Capability Check and Dropped-Delete Circuit Breaker

## Problem

On `frontend:dbus` clients (GTK4 `libim-fcitx5.so`, SDL) and web/canvas editors (Google Docs, Lark Sheets, Lexical), two distinct text-deletion failures cause `orig + replacement` word duplication (`dontdon't `) and subsequent `rules.toml` blocklist poisoning when the user erases the duplicated word with `BackSpace`:

1. **Named DBus clients without `ForwardKey` support:** The previous guard in `SmartKeyAddon::classifyProfile` only checked `rawProg.empty() && ic->frontendName() == "dbus"` before bypassing missing/invalid `SurroundingText`. Any *named* GTK4 or SDL DBus client—or any GTK4 widget advertising `SurroundingText` capability while reporting `cursor() == 0 < delete_chars`—could fall through to `forwardKey(BackSpace)` (which is `endbr64; ret` in GTK4 `libim-fcitx5.so`) followed by `commitString`.
2. **Silent `deleteSurroundingText` drops & unanswered deletes:** Clients that accept `deleteSurroundingText` without deleting text (or ignore consecutive `deleteSurroundingText` calls while `SurroundingText` stays valid) leave `"dontdon't "` on screen. Moreover, `SmartKeySession::reconcile_surrounding` treated `"dontdon't "` as satisfying `ends_with("don't ")` in `Mode::JustAutocorrected`, leaving `1x BackSpace` undo armed to poison `rules.toml`.

## Symptoms

- Autocorrecting `dont` -> `don't` produces `dontdon't ` on GTK4/SDL DBus apps or canvas editors that ignore `deleteSurroundingText`.
- Pressing `BackSpace` to clean up `dontdon't ` fires `1x BackSpace` undo and eventually blocklists `("dont", "don't")`.

## What Didn't Work

Porting `ngosen`'s 5-strategy replacement ladder (`overtype xtest` -> `overtype` -> `del` -> `surrounding` -> `forwarded backspaces`) directly violates SmartKey's invariants: `del` targets VTE terminals (which SmartKey strictly `BYPASS`es), `overtype xtest` targets Chromium URL bars (also `BYPASS` and X11-only), and `overtype` only applies to Facebook Messenger composers in `ngosen`.

## Solution

1. **Disassembly-verified `CapabilityFlag::KeyEventOrderFix` (`1ULL << 37`) check (`canForwardBackspace` in `adapters/fcitx5-smartkey/src/smartkey-addon.h`):**
   - Binary inspection of Arch IM modules shows GTK3 (`im-fcitx5.so`) and Qt6 (`libfcitx5platforminputcontextplugin.so`) set `KeyEventOrderFix`, while GTK4 (`libim-fcitx5.so`) and SDL omit it.
   - `canForwardBackspace(frontend, flags)` returns `flags.test(CapabilityFlag::KeyEventOrderFix)` when `frontend == "dbus"`, and `true` otherwise.
   - `SmartKeyAddon::classifyProfile` bypasses (`"dbus-no-surrounding-text"`) whenever `!canForwardBackspace(...) && !canSurround` (regardless of whether `ic->program()` is empty), and `applyReplaceAction` / `applyDeleteAndCommit` abort without filtering the key or calling `commitString` when neither `canUseDeleteSurrounding` nor `canForwardBackspace` is usable.
2. **Post-`deleteSurroundingText` verification & unanswered-delete circuit breaker (`SurroundingSyncTracker`):**
   - Before calling `deleteSurroundingText`, snapshot up to `16` Unicode scalars before the deleted range (`prefixTailUtf8`), the deleted slice, and the inserted string using UTF-8 codepoint stepping (`stepUtf8Codepoint`, keeping `smartkey-addon.h` FFI-free).
   - In `onSurroundingTextUpdated` (before `isSelfCommitting()` and the `80 ms` echo guard), classify updates as `Confirmed`, `StalePreCommit` (including 2-step DBus delete-then-commit snapshots), or `DroppedDeletion`.
   - On `DroppedDeletion`—or when `unansweredDeletes >= 3` (`kMaxUnansweredDeletes`) while `canForwardBackspace` is true—latch `surroundingUntrusted = true` for that `InputContext`, disarm `JustAutocorrected` via `SMARTKEY_KEY_NAV_OR_CLICK` (guarded strictly to `mode == "JustAutocorrected"` so active `EditingRestored` is never resolved as `KeptVerbatim`), and re-run `classifyProfile`.
   - Preserve `surroundingUntrusted` and `unansweredDeletes` across `reset()` (`onInputContextReset`), clearing them only via `resetTrust()` on `focusOutContext`, `onSwitchInputMethod`, `reloadConfig`, and `InputContextDestroyed`.
3. **Core defense-in-depth (`crates/smartkey-core/src/session.rs`):**
   - In `reconcile_surrounding` (`Mode::JustAutocorrected`), also reset via `NavOrClick` when `!raw_typed.is_empty() && before.ends_with(&format!("{raw_typed}{applied}{trailing_boundaries}"))`.

## Why This Works

- Prevents broken `commitString` emissions whenever no working delete primitive exists for the current `InputContext`, avoiding `orig + replacement` duplication without adding X11/XTest or terminal-mutating paths.
- Detects dropped deletions immediately upon the next fresh `SurroundingTextUpdated`, switches future replacements to `forwardKey(BackSpace)` (or `BYPASS` on GTK4/SDL DBus), and prevents user cleanup backspaces from triggering undo/blocklist poisoning.
- Related: [Unified App Profile Classification and `unknown_app_policy` Ordering](../architecture-patterns/unified-app-profile-classification-and-unknown-policy.md), [SurroundingText Unicode-Scalar Cursor Indexing vs UTF-8 Slicing](./surrounding-text-utf8-scalar-cursor-slicing.md), [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./surrounding-text-undo-guard-async-echo-windows.md), and [Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle](../architecture-patterns/fcitx5-flight-recorder-ffi-and-fd-contract.md).

## Prevention

- Keep `verifyBackspaceCapabilityAndDroppedDeleteGuard()` in `adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp` (`ctest`) and `reconcile_surrounding_disarms_undo_on_dropped_delete_duplication` in `crates/smartkey-core/tests/learning_eval.rs`.
- Never place `applyReplaceAction`'s abort guard after `keyEvent.filterAndAccept()`, and never clear `surroundingUntrusted` inside `onInputContextReset` (`reset()`).

## References

1
