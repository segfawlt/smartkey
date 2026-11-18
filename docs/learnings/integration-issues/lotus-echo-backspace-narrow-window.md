---
title: Lotus Echo-BackSpace Classification Needs a Narrow Window Separate From the Burst Guard
category: integration-issues
module: adapters/fcitx5-smartkey/src/smartkey-addon.h
tags: [fcitx5, vi-lotus, backspace, shadow-buffer, trace, wayland]
problem_type: integration-issue
date: 2026-11-18
---

# Lotus Echo-BackSpace Classification Needs a Narrow Window Separate From the Burst Guard

## Problem

In `vi-lotus` mode, `PreInputMethod` dropped any `BackSpace`/`Left`/`Delete` arriving while
`isInLotusBurst()` was true, assuming it was a synthetic `fcitx5-lotus` uinput echo. That broad
window (`kBurstInitialWindowMs = 350`, `kBurstEchoGraceMs = 300`) opens on nearly every Telex
letter (`a e o d w s f r x j z`, or any letter once `hasLotusActivityInWord()`), and every
swallowed BackSpace re-armed it for another 300 ms.

## Symptoms

- Human BackSpaces (120–300 ms after a letter, 40 ms autorepeat) never reached `SmartKeySession`;
  the shadow buffer kept deleted letters (`claudezill`, `thooi`, `klkieuer` in traces).
- Usually silent, but destructive when the stale buffer looked rescuable:
  `kiểu s⌫cos␣` → buffer `scos` → `vietnamese_telex_rescue` `sóc`, deleting 4 chars → `kiểsóc `.
- Older events with the same shape: `nsinh -> nính`, `zczcode -> zcode`, `anawn -> năn`.

## What Didn't Work

Several sessions tuned other layers (surrounding-text guards, delete ordering, boundary commits)
because the 350/300 ms window from `ffe6f67` was treated as settled — it had fixed the *opposite*
symptom (echoes leaking into the buffer). Without per-key timing there was no way to see that
user BackSpaces were labelled echoes.

## Solution

1. Flight-recorder evidence (`smartkey trace`, `pre_key` `lotus_echo_bs` rows): 180 genuine bursts
   had trigger → first echo ≤ 19 ms, echo → echo/`Pause` gaps ≤ 9 ms, always closed by `Pause`;
   misclassified user BackSpaces were 40–282 ms and never followed by `Pause`.
2. `LotusBridgeTracker` gained a separate `echoBsDeadlineMs`: `armBurst` sets `now + 45 ms`
   (`kEchoBsFirstWindowMs`), `rearmBurstEcho` (echo, `Pause`, VK rune, intermediate commit) sets
   `now + 30 ms` (`kEchoBsChainGapMs`). The echo filter uses `expectsLotusEchoBackspace()`; the
   broad `isInLotusBurst()` window is kept only for surrounding-text/boundary guards.
3. Diagnostic: `lotus_pause+late_echo_suspect` is traced when `Pause` arrives ≤ 120 ms after a
   BackSpace classified as the user's (would reveal a too-narrow window under load).

## Why This Works

`fcitx5-lotus` itself never guesses: it treats BackSpace as its own only while `g_isDeleting` is
set by an actual `performReplacement`. SmartKey cannot read that flag, but the injection latency
is tiny and bounded (preDelay ≤ `DefaultCommitInterval`, per-echo `DefaultBackspaceInterval`),
while human reaction is ≥ 40 ms. Chained bursts from `replayBufferedKeys()` land within the 30 ms
post-`Pause` allowance (observed +10 ms). Replaying all traced BackSpaces: old 0 echo leaks / many
user swallowed; new 0 echo leaks / 0 user swallowed.

## Prevention

- Never reuse a broad "client may still be mutating" guard window as a key classifier.
- Keep the `addon_load_smoke` echo-classification block (`scos`, `kl`, `Pause`+autorepeat cases).
- If `lotus.conf` `Default*Interval`/`PostDelay` are raised far above defaults, raise
  `kEchoBsFirstWindowMs`/`kEchoBsChainGapMs`; watch for `late_echo_suspect` in traces.
- Related: [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./surrounding-text-undo-guard-async-echo-windows.md).

## References

1
