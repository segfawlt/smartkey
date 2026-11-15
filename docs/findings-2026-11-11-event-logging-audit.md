# Findings: Diagnostic Event Logging Audit + Log Viewing UX (2026-11-11)

> **Handoff doc.** Produced by a code audit + live-log investigation of SmartKey's
> diagnostic event pipeline. All empirical numbers come from the author's real logs
> (`~/.local/state/smartkey/logs/events-2026-11-*.jsonl` and
> `~/.config/smartkey/learner_state.json` as of 2026-11-11).
> Status: **issues verified, no code changed yet.**

## Scope

- `crates/smartkey-core/src/event_log.rs` — JSONL writer/reader, level filters
- `crates/smartkey-core/src/store.rs` — `DiagnosticEvent` enum, `record_event_only`, `log_*` methods, ring buffer (`recent_events`, `MAX_RECENT_EVENTS = 256`)
- `crates/smartkey-cli/src/main.rs` — `smartkey log` (ring-buffer path vs JSONL path), `LogFilter`, `format_diagnostic_event_line`
- `crates/smartkey-core/src/session.rs` — emission sites for `UnfixedUnknown` / `RealWordDeclined` / `WordUnchanged`

## Event-type → sink matrix (the core structural finding)

| Event type | JSONL (durable) | Ring buffer (state) | Notes |
|---|---|---|---|
| `AutocorrectApplied` | ✅ | ✅ | `store.rs:1014` |
| `BackspaceUndo` | ✅ (only after resolution) | ✅ (incl. `Triggered`) | `store.rs:1033`, `resolve_pending_undo_log` at 1057 |
| `ManualBackspaceFix` | ✅ | ✅ | `store.rs:1098` |
| `UnfixedUnknown` | ⚠️ near-miss only | ✅ all | filter at `event_log.rs:222-235` |
| `RealWordDeclined` | ✅ | ❌ never pushed | `store.rs:1183`, `session.rs:2042` |
| `AppBypass` | ✅ | ❌ never pushed | `store.rs:1201`, FFI `lib.rs:356` |
| `RuleLearned` | ✅ | ❌ never pushed | `store.rs:1218` — **only fired for `personal_graduated`** |
| `WordUnchanged` | ✅ (only at `log_level = "all"`) | ❌ never pushed | `session.rs:2046-2051` |

Push sites: only `log_autocorrect_applied`, `log_backspace_undo`, `log_manual_backspace_fix`,
`log_unfixed_unknown` call `state.push_event`. The other four `log_*` methods write JSONL only.

## Verified issues

### L1 — "Can't determine at all" unknowns are dropped from the durable log (highest priority)

At default `[diagnostics].log_level = "interesting"`, `EventLog::should_record`
(`event_log.rs:222-235`) requires `best_candidate.is_some() && best_score >= near_miss_band * threshold`
(default band 0.5) for `UnfixedUnknown`. Words where the scorer found **no candidate at all**
(the class the system completely failed to determine — the most valuable signal for lexicon tuning)
are silently excluded from JSONL.

Empirical (2026-11-11):
- Ring buffer: 56 `unfixed_unknown` events, **37 with no candidate**.
- JSONL (all 3 day files): 25 `unfixed_unknown` events, **0 with no candidate**.
- `untouched_unknowns` counter map holds 813 unique words (learning channel — separate concern).

They remain visible only via bare `smartkey log` (ring buffer) and the top-N counter list —
never in the time-ordered durable log, and lost forever when the ring evicts (256 cap).

### L2 — Two divergent data sources behind one CLI surface

Bare `smartkey log` reads the ring buffer from `learner_state.json`; any of
`--today/--yesterday/--since/--grep/--json/--log-dir` switches to JSONL
(`main.rs:1550-1555`, `query_jsonl` condition). Same filter names
(`fixed|manual|unfixed`), different event universes:

- Bare `smartkey log` can **never** show `RealWordDeclined`, `AppBypass`, `RuleLearned`, `WordUnchanged` (never pushed to ring).
- `smartkey log unfixed` (ring path) shows all unknowns incl. no-candidate; `smartkey log unfixed --today` (JSONL path) shows near-miss only (L1).
- `--json` fallback path (`main.rs:1589-1612`) prints ring events **without the envelope** (`v`, `ts_ms`, `app`, `lang`), so output shape differs from the JSONL path.

### L3 — `RuleLearned` covers only one learning stage; permanent blocks are silent

`log_rule_learned` is emitted only via `add_personal_word` (stage `personal_graduated`,
`store.rs:1337`). The 1x **permanent block pair** written by `add_pair_block`
(`store.rs:1290`) — the "Undo + Edit blocks (typed, rejected) permanently" rule —
produces **no log event and no provenance comment** in `rules.toml`.

Real consequence (2026-11-11, `polish` case, see companion doc
`findings-2026-11-11-proper-noun-homographs.md`): the system permanently changed its
behavior (`{ typed = "polish", rejected = "polish" }` appended to `[blocklist]`) and the
event log shows nothing — the only traces are two `backspace_undo` events and a diff of rules.toml.

Graduated `[corrections]` / `[context_corrections]` / `telex_passthrough` promotions are
similarly unlogged (verify each write path in `learner.rs` when implementing).

### L4 — `Triggered` undos are not durable until resolved

`log_backspace_undo` (`store.rs:1048`) skips JSONL when `resolution == Triggered`;
`resolve_pending_undo_log` (`store.rs:1057`) writes the final resolution later.
If the addon process exits/reloads (fcitx5 restart, config reload) or the pending entry is
evicted from the 256-slot ring before the user finishes the undo, the event never reaches JSONL.

### L5 — Retracted unknowns leave false records in JSONL

`cancel_pending_unfixed_unknown` (`store.rs:1157`) decrements the counter and removes the
ring entry when the user immediately backspaces into a just-committed unknown word, but the
already-appended JSONL record stays (append-only; no compensating event). Log replay
overstates unknown-word volume.

### L6 — Minor

- `AppBypass` logs per window-focus/refocus (`reason: "capability:terminal"` × 11 for ghostty across 3 days). Candidate for per-(app,reason) dedupe within a time window.
- `EventLogLevel::parse` (`event_log.rs:34-41`) silently maps any unknown string to `Interesting` — a config typo like `"al"` degrades verbosity with no warning.
- `prune_old_files` runs once-per-date per writer process; multiple concurrent writers (addon + `smartkey repl`) race benignly but each re-prunes. Low priority.
- `read_records_in_range` silently skips records whose `event` tag fails to deserialize (forward-incompatible: new event types are invisible to older CLIs — acceptable, but the `v` envelope field is parsed and unused; consider logging skipped-version counts at least in debug).

## UX improvements (proposed, not implemented)

Priority order:

### U1 — `smartkey log stats [--since <spec>] [--app <id>]`
Aggregate JSONL into a false-positive table: per `MatchKind` → applied count, undone count,
undo rate, median time-to-undo. This single view makes dictionary bugs self-evident:
`proper_noun 4 applied / 2 undone within 4 minutes` (the real `polish` case).
Join `BackspaceUndo` (non-`Triggered`) with the preceding `AutocorrectApplied` on the
`(typed, applied)` pair within `undo_window_ms`.

### U2 — `smartkey explain --why`
Enumerate every gate for a token: valid-word shield, personal-dict canonical, undo-shield,
session shield, blocklist pair (with learned date if derivable), proper-noun candidate
suppressed, near-miss candidates below threshold, context rules considered.
Motivating case: `smartkey explain polish` currently prints `UNCHANGED (valid_word)`
when the *actual* reason is the learned `(polish, polish)` block pair — actively misleading.
(`explain` reads the blocklist, so it reflects suppression, but never names it.)

### U3 — `smartkey log --follow`
Tail today's JSONL file (poll or inotify), rendered with `format_diagnostic_event_line`.
For live debugging: type in one window, watch events stream.

### U4 — Filters for the JSONL path
`--kind <match_kind>`, `--app <id>` (exact/substring), maybe `--event <type>`.
Currently only the three positional filters + `--grep` on raw line text exist.

### U5 — Unify the two views
Either (a) push all 8 event types to the ring and treat bare `smartkey log` as "recent,
from state", with `--json`/time flags as "durable, from JSONL" (documented in `--help`),
or (b) make bare `smartkey log` read JSONL too when the dir exists. At minimum, document
the divergence in `smartkey log --help`.

### U6 — Emit `RuleLearned` for every durable rule mutation
Stages to add: `pair_blocked_1x` (from `add_pair_block`), correction graduation,
context-correction graduation, telex-passthrough promotion. Pairs naturally with
`diagnostics.annotate_rules` provenance comments (extend the comment format to include
date + app already collected in `format_provenance_comment`, `store.rs:1227`).

## Suggested implementation order for the handoff agent

1. U6 + L3 (small, high signal — makes learning events durable)
2. L1 (flip the no-candidate unknowns to always-record at `Interesting`, or add a third level; decide whether 813-entry volume needs sampling)
3. U1 (stats) — then U2 (`explain --why`)
4. U3 + U4 (tail + filters, mechanical)
5. L2/L4/L5/L6 (consistency fixes, any order)
6. Update `tests/learning_eval.rs:724-808` expectations when event emission changes

## Test notes

- `crates/smartkey-core/tests/learning_eval.rs` asserts ring-buffer contains only the 4 core
  variants (not bloated by `WordUnchanged`/`RealWordDeclined`) — this encodes L2's divergence
  as intentional; revisit if U5 is implemented.
- `event_log.rs` unit tests cover the near-miss band behavior — extend for no-candidate policy change.
