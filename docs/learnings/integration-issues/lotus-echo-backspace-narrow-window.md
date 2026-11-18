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
   `now + 30 ms` (`kEchoBsChainGapMs`). `armBurst` fires on a Telex trigger letter, on any letter
   once `hasLotusActivityInWord()`, on every boundary char (`Space`, punctuation), and on queued
   post-boundary letters — so the 45 ms window also follows `Space`. The echo filter
   (`BackSpace`/`Left`/`Delete`) uses `expectsLotusEchoBackspace()`; the broad `isInLotusBurst()`
   window is no longer a key classifier — it remains for surrounding-text/self-commit guards,
   post-boundary queuing, the VK-rune re-arm guard, and trace fields.
3. Diagnostic: `lotus_pause+late_echo_suspect` is traced when `Pause` arrives ≤ 120 ms after a
   BackSpace classified as the user's (would reveal a too-narrow window under load).

## Why This Works

`fcitx5-lotus` itself never guesses: it treats BackSpace as its own only while `g_isDeleting` is
set by an actual `performReplacement`. SmartKey cannot read that flag, but the injection latency
is tiny and bounded (preDelay ≤ `DefaultCommitInterval`, per-echo `DefaultBackspaceInterval`),
while human BackSpaces are slow. The classifier does not measure human timing; the separation is
empirical (2026-11-17/18 traces, 281 BackSpaces following a non-BackSpace key): 239 at 0–20 ms
(all echoes), none at 21–100 ms, the fastest human one at 144 ms; BackSpace autorepeat is 40 ms,
above the 30 ms chain gap. Chained bursts from `replayBufferedKeys()` land within the 30 ms
post-`Pause` allowance (observed +10 ms). Replaying all traced BackSpaces: old 0 echo leaks / many
user swallowed; new 0 echo leaks / 0 user swallowed.

### Residual risks (accepted)

- A user BackSpace ≤ 45 ms after an arming key (or ≤ 30 ms after an echo/`Pause`) is still
  swallowed. Not observed in traces (see gap above).
- An ordinary non-arming letter does **not** clear `echoBsDeadlineMs`. Do not "fix" this: 43 of
  1412 traced user-key gaps were < 20 ms (key rollover), and the first echo can arrive up to ~19 ms
  after the trigger, so clearing on the next letter would leak echoes into the session.
- A swallowed BackSpace also extends the broad window by 300 ms (`rearmBurstEcho`).
- All traced echoes were `BackSpace` (421/421); `Left`/`Delete` stay in the filter for parity with
  Lotus's replacement paths but have no trace evidence either way.
- Echoes later than the window leak; `late_echo_suspect` only catches them when a `Pause` follows
  within 120 ms.

## Prevention

- Never reuse a broad "client may still be mutating" guard window as a key classifier.
- Keep the `addon_load_smoke` echo-classification block (`scos`, `kl`, `Pause`+autorepeat cases).
- If `lotus.conf` `Default*Interval`/`PostDelay` are raised far above defaults, raise
  `kEchoBsFirstWindowMs`/`kEchoBsChainGapMs`; watch for `late_echo_suspect` in traces.
- Related: [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./surrounding-text-undo-guard-async-echo-windows.md).

## References

1
