---
title: "Fcitx5 Addon Config, Toggle Hotkey, and Optional Notifications"
category: integration-issues
module: adapters/fcitx5-smartkey/src/smartkey-addon.cpp
tags: [fcitx5, config, hotkey, notifications, packaging, ffi]
problem_type: integration-issue
date: 2026-11-24
---

# Fcitx5 Addon Config, Toggle Hotkey, and Optional Notifications

## Problem

Exposing a configurable global toggle hotkey (`ToggleKey`) in `fcitx5-smartkey` surfaced four cross-version and lifecycle hazards across Fcitx5 5.0/5.1 headers, addon reload side effects, headless smoke testing, and portable packaging.

## Symptoms

- `KeyEvent::checkKeyList(...)` exists only in `@since 5.1.23` (`fcitx/event.h`), whereas SmartKey's minimum Fcitx5 baseline (`CMakeLists.txt`, Debian 12 portable builds) is `5.0.20`.
- Calling `SmartKeyAddon::reloadConfig()` on hotkey toggle (or inside `SmartKeyAddon::save()`) re-mmaps `.skdict` packs, closes `trace_`, and invokes `resetSurroundingTrust()` on every `InputContext`, wiping learned `surroundingUntrusted` circuit breakers.
- Listing `notifications` under `[Addon/Dependencies]` breaks headless `AddonManager` loading in `addon_load_smoke` (which has no desktop notification server), and `#include <fcitx-module/notifications/notifications_public.h>` requires `Fcitx5ModuleNotifications`.
- `Configurable=False` and Fcitx5 package dependencies are duplicated across `smartkey.conf.in`, `scripts/package-portable.sh`, and `tests/addon_load_smoke.cpp`.

## Solution

1. **Use `fcitx::Key::checkKeyList` (`fcitx-utils/key.h`) for >=5.0.20 compatibility:**
   - Check `!config_.toggleKey->empty() && keyEvent.key().checkKeyList(*config_.toggleKey)` at the top of `SmartKeyAddon::onKeyEvent`, *before* `ic == nullptr`, active-IM routing, or `SMARTKEY_PROFILE_BYPASS` early returns.
   - `Key::checkKeyList` has existed since Fcitx5 5.0, unlike `KeyEvent::checkKeyList` (`5.1.23+`).
2. **Toggle in-process without calling `reloadConfig()`:**
   - Expose `smartkey_engine_toggle_enabled(engine, &out_enabled)` in `crates/smartkey-ffi`, calling `reload_if_modified()` before flipping `enabled` so external `smartkey toggle` CLI writes never cause state oscillation.
   - On hotkey match, consume the event with `keyEvent.filterAndAccept()`, call `smartkey_engine_toggle_enabled`, reset active sessions via `resetContext(ctx)` (preserving `surroundingUntrusted`), and leave `SmartKeyAddon::save()` as an empty no-op.
3. **Wire `SmartKeyConfig` and optional notifications portably:**
   - Define `FCITX_CONFIGURATION(SmartKeyConfig, ...)` in `smartkey-addon.h` (keeping the header free of inline FFI symbols per [Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle](../architecture-patterns/fcitx5-flight-recorder-ffi-and-fd-contract.md)), call `readAsIni(config_, "conf/smartkey.conf")` in both the constructor and `reloadConfig()`, and save in `setConfig()` via `safeSaveAsIni(config_, "conf/smartkey.conf")`.
   - Put `0=notifications` under `[Addon/OptionalDependencies]` (never `[Addon/Dependencies]`) and invoke `notif->callWithSignature<...>("Notifications::showTip", ...)` when `addon("notifications", true)` is present, avoiding a compile-time `Fcitx5ModuleNotifications` dependency.
4. **Keep packaging descriptors synchronized:**
   - Link `Fcitx5::Config` in `adapters/fcitx5-smartkey/CMakeLists.txt`, add `libfcitx5config-dev` to `scripts/package-portable.sh`, and update `Configurable=True` + `[Addon/OptionalDependencies]` across `smartkey.conf.in`, `scripts/package-portable.sh`, and `addon_load_smoke.cpp`.

## Why This Works

- Matching on `keyEvent.key().checkKeyList` works inside bypassed terminals/IDEs as well as prose apps and compiles against both Arch Fcitx5 5.1.x and Debian 12 Fcitx5 5.0.x.
- Bypassing `reloadConfig()` keeps mmapped lexicons and per-context `surroundingUntrusted` state intact ([DBus KeyEventOrderFix Capability Check and Dropped-Delete Circuit Breaker](./dbus-key-order-fix-and-dropped-delete-guard.md)).
- Optional runtime dispatch via `callWithSignature` shows OSD feedback in live sessions while letting headless `addon_load_smoke` tests verify `getConfig()` / `setConfig()` / `conf/smartkey.conf` persistence with zero daemon dependencies.

## Prevention

- Keep `addon_load_smoke.cpp` testing `addon->getConfig()`, `addon->setConfig(raw)`, and `readAsIni` round-tripping under `Configurable=True`.
- Never place `notifications` under `[Addon/Dependencies]`, never call `reloadConfig()` from `save()` or key handlers, and verify `Key::checkKeyList` compatibility against Fcitx5 5.0 when touching key matching.

## References

0
