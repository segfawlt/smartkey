# Learnings Index

Indexed catalog of durable lessons captured by the `compound` skill. Check entries with relevant
tags before non-trivial work.

## architecture-patterns/

- [Fcitx5 Flight-Recorder FFI Boundary and Persistent FD Lifecycle](./architecture-patterns/fcitx5-flight-recorder-ffi-and-fd-contract.md) — Keep header-only `smartkey-addon.h` helpers FFI-free for `addon_load_smoke`, copy borrowed `SmartKeySessionSnapshot` slices before mutation, close persistent `O_APPEND` fds in `reloadConfig()`, and enforce `BYPASS` privacy before tracing. `#fcitx5 #ffi #trace #file-descriptor #privacy`
- [Unified App Profile Classification and `unknown_app_policy` Ordering](./architecture-patterns/unified-app-profile-classification-and-unknown-policy.md) — Keep capability flags and the DBus `KeyEventOrderFix`/no-`SurroundingText` guard ahead of `unknown_app_policy`, parse `"default"` as `None`, and delegate classification to `smartkey-core` via FFI. `#app-profiles #fcitx5 #ffi #security`
- [RuleStore Sidecar Lockfile and Atomic Rename Invariants](./architecture-patterns/rule-store-sidecar-lockfile-and-atomic-renames.md) — Serialize concurrent writes on sibling `.smartkey.lock` (`truncate(false)`) instead of target files replaced via `atomic_write_bytes` inode renames, use re-entrant `with_file_lock`, and keep read-only constructors lock-free. `#store #concurrency #locking #atomic-write #fcitx5`

## design-patterns/

- [Verifying ratatui screens: hidden tab bars and tmux capture](./design-patterns/ratatui-tui-verification-and-hidden-tabs.md) — `Tabs` in a titled, bordered 2-row `Block` renders no labels; assert real buffer text, and drive the release binary in tmux against copied config before calling a TUI change done (send `Escape` separately, or crossterm reads Alt+key). `#tui #ratatui #testing #tmux`

## integration-issues/

- [SurroundingText Unicode-Scalar Cursor Indexing vs UTF-8 Slicing](./integration-issues/surrounding-text-utf8-scalar-cursor-slicing.md) — Index `SurroundingText::text()` by stepping UTF-8 codepoint boundaries because `cursor()`/`anchor()` count Unicode scalars, not bytes, and bound huge editors (`>16384` chars). `#fcitx5 #surrounding-text #utf8 #vietnamese #wayland`
- [Surrounding-Text Undo Guard and Async Lotus Echo Windows](./integration-issues/surrounding-text-undo-guard-async-echo-windows.md) — Guard displaced-cursor undo across `en` (`80 ms`) and `vi` (`400 ms`) while skipping surrounding-text reconciliation during self-commits, Lotus bursts, or pending boundaries. `#fcitx5 #vi-lotus #undo #surrounding-text`
- [DBus `KeyEventOrderFix` Capability Check and Dropped-Delete Circuit Breaker](./integration-issues/dbus-key-order-fix-and-dropped-delete-guard.md) — Gate `frontend:dbus` `ForwardKey` on `CapabilityFlag::KeyEventOrderFix`, abort un-deletable replacements, and latch per-context `surroundingUntrusted` plus `JustAutocorrected` disarming when `deleteSurroundingText` is silently dropped or unanswered. `#fcitx5 #dbus #surrounding-text #gtk4 #undo #wayland`
- [Stepwise BackSpace Undo Across `fcitx5-lotus` `PreInputMethod` and `PostAutocorrectOrigin`](./integration-issues/stepwise-undo-lotus-preinput-and-pending-punct.md) — Intercept `JustAutocorrected` (`trailing_count == 0`) 2nd BackSpace in `PreInputMethod` before Lotus eats the word, and carry `PostAutocorrectOrigin` through `PendingTrailingDot`/`PendingSemicolon` instead of re-scoring already-corrected tokens. `#fcitx5 #vi-lotus #undo #pending-dot #state-machine`
- [Lotus Echo-BackSpace Classification Needs a Narrow Window](./integration-issues/lotus-echo-backspace-narrow-window.md) — Classify `BackSpace` as a Lotus echo only within 45 ms of a trigger / 30 ms of an echo or `Pause` (traced bursts ≤ 19 ms), never via the broad 350/300 ms burst guard that swallowed user BackSpaces and desynced the shadow buffer (`scos -> sóc`). `#fcitx5 #vi-lotus #backspace #shadow-buffer #trace`
- [Release `-DNDEBUG` Silently Disabled `addon_load_smoke` Assertions](./integration-issues/release-ndebug-vacuous-smoke-asserts.md) — `#undef NDEBUG` before `<cassert>` in C++ test targets built Release; confirm red tests actually fail before fixing. `#fcitx5 #cmake #testing #ndebug`
- [Fcitx5 Addon Config, Toggle Hotkey, and Optional Notifications](./integration-issues/fcitx5-addon-config-hotkey-and-optional-notifications.md) — Match `Key::checkKeyList` (`>=5.0.20`) before IM/`BYPASS` early returns, toggle via FFI + `resetContext` without calling `reloadConfig()`, use `[Addon/OptionalDependencies]` + `callWithSignature` for `Notifications::showTip`, and keep all three `Configurable=True` sites synced. `#fcitx5 #config #hotkey #notifications #packaging #ffi`

## logic-errors/

- [Scorer Step 5a Curated Typo Metadata Filtering and Test Isolation](./logic-errors/scorer-step5a-metadata-and-dialect-filtering.md) — Curated typo targets must pass lexicon dialect and offensive metadata checks without a synthetic frequency fallback, and Step 5a isolation tests require distance >= 3 tokens so `lev2` or smash rescue does not mask failures. `#scorer #dialect #false-positive #testing`
- [Scorer Relational Stem-Dominance and Motor-Excess Bigram Guard](./logic-errors/scorer-anti-inflection-and-motor-excess-guard.md) — Compare un-clamped `raw_edit_cost` with relational `is_english_inflection_of` stem dominance, damp excess-cost bigrams, and restrict 7–9 char `dl <= 2` words from `SmashRescue` so high-freq/bigram inflections never beat motor-clean stems. `#scorer #inflection #smash-rescue #bigram #false-positive`
- [Safe `vi-lotus` Sentence-Start English Passthrough Boundaries](./logic-errors/vi-lotus-sentence-start-english-passthrough.md) — Restrict sentence-start Telex restoration to TitleCase `If`/`Of`/`Or` so real Vietnamese accented starters (`Á`, `Í`, `Há`, `Hí`) remain shielded, and note `W2uMode::Default` does not transform leading `Was`. `#vi-lotus #telex #passthrough #vietnamese`
- [Vietnamese Telex Rescue Strategy Ordering and Simulation Gates](./logic-errors/vi-telex-rescue-strategy-ordering-and-gates.md) — Gate omitted-mark rescues on mid-word composition (`needs_uinput_restore_on_break || is_composed_vietnamese`), order Strategy 5a (`ie`/`ye`+coda) before Strategy 5 (`tienf -> tiền`) and Strategy 6 (unique checked-coda 1-mark) after it, accept `needs_uinput_restore_on_break` on Strategy 7 diphthong prefixes (`duowg -> dương`), and test `must_not_touch.txt` via `evaluate_token`. `#vi-telex #lotus #rescue #false-positive #testing`

## performance/

- [Zero-Copy `EnglishLexicon::from_static_bytes` with `Cow::Borrowed`](./performance/zero-copy-static-lexicon-borrow.md) — Split section parsing from byte ownership so static embedded FST and typo pools use `Cow::Borrowed` without copying 5 MB or reaching for `unsafe`. `#lexicon #fst #zero-copy #performance`

## Pending Terms

- `Clause-Erase Cancel` — Holding `BackSpace` past index 0 of a restored word aborts undo learning rather than recording a partial prefix rejection.
- `Delta-Span Check` — Learner heuristic rejecting manual edits whose length delta or edit distance indicates a change of mind rather than a typo fix.
- `Self-Confirm Cancel` — Cancelling an undo-keep signal when the user immediately retypes the exact autocorrection they just undid.
- `Lotus burst echo window` — Broad time window (`fcitx5-smartkey`, 350/300 ms) after `fcitx5-lotus` `Pause`/Telex keys during which lagging `SurroundingTextUpdated` events are skipped. Synthetic `BackSpace`/`Left`/`Delete` echoes use the separate narrow echo window (45/30 ms).

## Promote Candidates

Lessons referenced 3+ times. Promotion is a user decision.
