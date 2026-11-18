---
title: Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle
category: architecture-patterns
module: adapters/fcitx5-smartkey/src/smartkey-addon.h
tags: [fcitx5, ffi, trace, file-descriptor, privacy]
problem_type: design-decision
date: 2026-11-18
---

# Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle

## Context

Adding a zero-overhead live Fcitx5 flight recorder (`TraceLogger`, `SmartKeySessionSnapshot`, and `smartkey trace`) spanning `smartkey-core`, `smartkey-ffi`, and `adapters/fcitx5-smartkey` introduced four non-obvious cross-layer constraints across C++, C ABI, and the long-running `fcitx5` daemon process.

## Guidance

1. **Keep `smartkey-addon.h` header-only helpers FFI-free (`CMakeLists.txt` boundary):**
   - `addon_load_smoke` compiles against `adapters/fcitx5-smartkey/src/smartkey-addon.h` **without** linking `libsmartkey_ffi.a` so the test can validate real `dlopen` symbol isolation of `libsmartkey.so` under `-Wl,--exclude-libs,ALL`.
   - Any header-only helper instantiated by unit tests (`appendJsonEscaped`, `extractSurroundingSlice`, `formatSessionCompact`, `TraceLogger`) must only consume passive C structs (`SmartKeySessionSnapshot`), while FFI calls (`smartkey_engine_is_trace_enabled`, `smartkey_session_debug_snapshot`) must stay in `smartkey-addon.cpp`.
2. **Materialize borrowed C ABI slice fields before mutating the session:**
   - `SmartKeySessionSnapshot` exposes `buf_utf8`, `aux_utf8`, and `prev_utf8` as borrowed pointer + `*_byte_len` pairs without NUL termination to keep snapshots zero-allocation.
   - Because they point into `SmartKeySession`'s internal buffers, convert them immediately (`formatSessionCompact` -> owned `std::string`) *before* any mutating `smartkey_session_*` call (`process_key`, `on_lotus_commit`, `reconcile_surrounding`).
3. **Close persistent trace file descriptors unconditionally in `reloadConfig()` and refresh config in `reload_dicts()`:**
   - Holding one persistent `0600` `O_APPEND` fd per UTC day avoids per-key `open`/`close` overhead, **but** calling `smartkey trace clear` unlinks `trace-YYYY-MM-DD.jsonl*` while Fcitx5 still holds the old inode open, and `smartkey trace off` requires closing immediately.
   - `SmartKeyAddon::reloadConfig()` must call `trace_.closeIfOpen()` unconditionally, and `SmartKeyEngine::reload_dicts()` must call `self.store.reload_if_modified()` so DBus `ReloadConfig` picks up `diagnostics.trace_log` toggles even when no `InputContext` exists.
4. **Enforce `classifyProfile != SMARTKEY_PROFILE_BYPASS` at every trace site:**
   - Hooks that normally skip profile classification when tracing is off (`onSurroundingTextUpdated`, `isSelfCommitting` / `!hasPendingBoundary()` early returns in `onCommitString`, and `focusOutContext` / `onInputContextReset` / `onSwitchInputMethod`) must explicitly check `classifyProfile(ic, appName, nullptr) != SMARTKEY_PROFILE_BYPASS` *after* `isTraceActive()` and *before* reading `SurroundingText` or emitting any trace line.

## Why This Matters

- Calling FFI symbols from inline helpers in `smartkey-addon.h` breaks `addon_load_smoke` linking even when `libsmartkey.so` succeeds.
- Skipping `trace_.closeIfOpen()` on `reloadConfig()` leaves the daemon appending silently to an unlinked inode after `smartkey trace clear`.
- Omitting the bypass gate in `surround`/`commit`/`focus_out` hooks leaks terminal/password text into `trace-*.jsonl`.

## When to Apply

- Whenever adding C++-testable structs or inline utilities to `adapters/fcitx5-smartkey/src/smartkey-addon.h`.
- Whenever returning borrowed string slices over `smartkey-ffi` or caching open file descriptors inside Fcitx5 addons.
- Whenever adding new `on*` event hooks in `SmartKeyAddon` that touch `InputContext::surroundingText()` or `CommitStringEvent`.

## References

1
