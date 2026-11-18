---
title: "Unified App Profile Classification and unknown_app_policy Ordering"
category: architecture-patterns
module: crates/smartkey-core/src/store.rs
tags: [app-profiles, fcitx5, ffi, security]
problem_type: architecture-and-bug-fix
date: 2026-11-16
last_updated: 2026-11-18
---

# Unified App Profile Classification and `unknown_app_policy` Ordering

## Problem

Application profile classification had three compounding bugs across `store.rs`, `smartkey-ffi`, `fcitx5-smartkey`, and `smartkey-cli`:

1. `AppProfileConfig::classify_override` only consulted `unknown_app_policy` when `app` was empty (`""`), so named-but-unlisted third-party apps (not in `[app_profiles]` or the built-in table) ignored `unknown_app_policy = "bypass"`.
2. `RuleStore::extract_compiled` parsed `unknown_app_policy = "default"` as `Some(AppProfile::Prose)`, contradicting `set_setting` (which treated `"default"` / `"none"` as `None`, removing the key).
3. Classification logic lived separately in C++ `classifyProgramName` vs Rust `AppProfileConfig`, `AppBypass` diagnostics only logged transitions *into* `BYPASS`, and the GTK4 DBus empty-program + missing-`SurroundingText` safety guard ran *after* `unknown_app_policy`.

## Symptoms

- Setting `unknown_app_policy = "bypass"` did not bypass unlisted named apps.
- Setting `unknown_app_policy = "prose"` could disable the GTK4 DBus no-`SurroundingText` bypass guard, exposing GTK4 DBus terminals where `ForwardKey` is a no-op (`orig + replacement` duplication).
- Unlisted terminals (`zcode`) fell through silently to `PROSE` without diagnostic visibility or CLI inspection (`smartkey app`).

## Solution

1. Mirror the built-in classifier in `smartkey-core` (`normalize_program_id`, `classify_builtin_program`, `AppProfileConfig::classify_app`) with `"zcode"` added to both Rust and C++ IDE bypass lists (`addon_load_smoke.cpp` tests `"zcode"` parity).
2. Precedence order (`SmartKeyAddon::classifyProfile` + `AppProfileConfig::classify_app`):
   1. Fcitx5 `CapabilityFlags` (`PasswordOrSensitive`, `Terminal`, `Url`, `Email`, numeric/disabled) -> always `BYPASS`.
   2. Missing/dropped `ForwardKey` (`!canForwardBackspace(frontend, flags)`, which checks `CapabilityFlag::KeyEventOrderFix` on `frontend == "dbus"`) combined with unavailable/invalid/untrusted `SurroundingText` (`!canSurround`) -> `BYPASS` (`"dbus-no-surrounding-text"`) **before** `unknown_app_policy`.
   3. `smartkey_engine_classify_app(engine, app, &reason)` returning `'static` NUL-terminated reason C strings: explicit `[app_profiles]` overrides -> built-in non-`prose` matches (`program-list`/`browser`/`chat`) -> `unknown_app_policy` (for both empty and unlisted named apps) -> `Prose`.
3. Parse `"default" | "none" | "prose_default"` -> `None` in `extract_compiled`, add `RuleStore::{add,remove}_app_profile_override`, log all profile transitions via `shouldLogProfileTransition`, format `APP_BYPASS` vs `APP_PROFILE` in CLI `log` output (keeping JSONL `"app_bypass"` event tag backward-compatible), and expose `smartkey app [list|classify|add|remove]`.

## Why This Works

Security invariants (`CapabilityFlags` and GTK4 DBus missing `SurroundingText`) cannot be overridden by `unknown_app_policy = "prose"`, user overrides take precedence over built-ins, `unknown_app_policy` controls all unrecognized surfaces uniformly, and C++ + FFI + CLI share one deterministic classification source of truth.

## Prevention

- Never place user `unknown_app_policy` ahead of `CapabilityFlags` or the `dbus-no-surrounding-text` check in `adapters/fcitx5-smartkey/src/smartkey-addon.cpp`.
- Keep legacy `smartkey_engine_classify_app_override` unchanged for FFI ABI compatibility, and use `smartkey_engine_classify_app` for new callers.

## References

1
