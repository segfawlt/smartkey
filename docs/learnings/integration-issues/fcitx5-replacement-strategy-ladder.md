# Fcitx5 Correction Replacement: Strategy Ladder over Linear Fallback

**Tags:** `#fcitx5 #replacement #delete-surrounding-text #surrounding-text #wayland #app-quirks`

## Context

`adapters/fcitx5-smartkey/src/smartkey-addon.cpp` replaces an already-committed word when the
scorer decides a correction is needed. The replacement is a linear if/else chain over four
outcomes (`delete_surround` → `forward_bs_delay15ms` → `forward_bs_sync` → `insert_only`,
`:746-822`). Any surface that supports neither `deleteSurroundingText` nor forwarded
`BackSpace` leaves the caller with no valid move.

## Lesson

Model replacement as a **strategy table with per-strategy usability probes**, not a linear
fallback chain:

```cpp
struct Method {
    const char* name;
    bool (TypingState::*usable)(const ReplaceRequest&);
    void (TypingState::*run)(const ReplaceRequest&);
};
static constexpr Method methods[] = {
    {"overtype xtest", ...},   // selection + type over, X11 only
    {"overtype",       ...},   // Facebook-class composers
    {"del",            ...},   // erasesWithCommittedDel()
    {"surrounding",    ...},   // deleteSurroundingText()
    {"forwarded backspaces",...},
};
// fallthrough: "system backspaces"
```

Reference implementation: `ngosen/ngosen` `src/core/ngosen-state-replace.cpp:554-579`
(`TypingState::deleteAndCommit`). Each `*Usable` probes what the field **reports**
(`selectsOverAutocompletion`, `forwardsBackspaces`, `erasesWithCommittedDel`,
`deletesThroughSurrounding`) rather than matching an application name.

## Why the linear chain is not enough

Four distinct surfaces defeat a linear chain and are all handled by ngosen's ladder:

| Surface | Linear chain result | Ladder strategy |
| --- | --- | --- |
| Firefox/Chromium address bar | no-op, correction dropped | `overtype` (Shift+Left select, type over) |
| X11 SDL / Wine / XIM clients | forwarded keys dropped or reordered | `overtype xtest` (real XTEST events) |
| VTE terminals (Ptyxis, xterm) | surrounding-text unavailable | `del` (committed DEL) |
| GTK4 / LibreOffice | drops forwarded Backspace | `surrounding` |

The duplicate-text hazard is documented in the caller's own comment
(`smartkey-addon.cpp:573-574`): without a valid delete path the code appends the replacement
after the original (`orig + replacement`).

## Required companion: verify and retry

A replacement is not done when the bytes are sent — only when the field confirms them.
ngosen's `src/core/ngosen-state-check.cpp` shows the pattern:

- snapshot surrounding text at send time (`recordSendSnapshot`,
  `ngosen-state-replace.cpp:581`) and mark the snapshot fresh vs. stale, so a lagging app's
  stale report cannot satisfy the check;
- `fixDroppedDeletion` (`check.cpp:200-225`) retries the ladder with the recomputed residue,
  but only when the report is byte-exact, no intervening key/commit occurred, and a prefix
  exists — otherwise it gives up rather than thrashing;
- count consecutive unanswered deletions (`surrUnanswered_`); past
  `UnansweredLimit`, mark the field untrusted and stop attempting replacements there.

Without this, a dropped deletion is indistinguishable from a successful one and the user sees
either duplicated or missing text.

## How to apply here

1. Extract the inline block at `smartkey-addon.cpp:746-822` into a strategy table.
2. Add an `overtype`/`xtest` path — currently missing entirely.
3. Add `verifySurroundingAfterDelete` with a bounded retry and a per-`InputContext`
  "unanswered deletions" counter.
4. Drive each strategy from a fake host that reports configurable capability flags, so every
  branch is testable without a Wayland seat.

Full comparison and probe output: `docs/findings-2026-11-18-ngosen-comparison.md`.