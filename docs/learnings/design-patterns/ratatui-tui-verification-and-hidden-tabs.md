---
title: "Verifying ratatui screens: hidden tab bars and tmux capture"
category: design-patterns
module: crates/smartkey-cli/src/tui.rs
tags: [tui, ratatui, testing, tmux, ux]
problem_type: design-decision
date: 2026-11-24
---

# Verifying ratatui screens: hidden tab bars and tmux capture

## Context

The first `smartkey tui` passed every `TestBackend` smoke test, yet its tab bar never appeared on screen. The header used `Tabs` inside a `Block` with a title and `Borders::BOTTOM` in a 2-row area, so the title and border used both rows and left zero rows for the tab labels.

## Guidance

- Do not put `Tabs` inside a bordered, titled `Block` unless the area is at least 3 rows. Prefer one `Paragraph` line of spans for the header, with a separate 1-row rule beneath it.
- "Renders without panic" tests prove nothing about content. Assert the text you expect, for example that the first buffer line contains every screen label (`tab_bar_is_visible_and_every_screen_renders_at_any_size`).
- Before calling a TUI change done, drive the release binary in tmux against a copy of real config and read the screens:

  ```bash
  tmux new-session -d -s sk -x 110 -y 32 "target/release/smartkey tui --dir <copy>"
  tmux send-keys -t sk 2; sleep 0.5; tmux capture-pane -p -t sk
  ```

- Send `Escape` and the next key in separate `send-keys` calls with a short sleep in between. Otherwise crossterm reads them as Alt+key, and the modal looks stuck.

## Why This Matters

Real data exposed problems the synthetic fixtures could not: 1,600+ staged entries, truncated log lines, and duplicated controls. The hidden tab bar went unnoticed until someone looked at a real capture.

## When to Apply

Apply this to any change to `crates/smartkey-cli/src/tui.rs` layout, header, or modal code, and to any new ratatui screen.

## References

0
