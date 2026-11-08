# Handoff: Diagnostics Event Log & Learning Improvements

**Date:** 2026-11-08
**Status:** **Implemented & verified** (`cargo test --workspace` + 0% FP corpus eval gate passing).
**Prepared by:** ZCode agent session + Antigravity review & implementation with owner.
**Intended reader:** a reviewer picking up implementation.

---

## 0. Reviewer orientation & repo state warning

Before touching anything, know this:

> **The working tree contains a large uncommitted feature that is NOT part of this handoff.**
> `git status` shows 9 modified files, +421/-21 lines: an in-flight **`vi_lotus` plugin-toggle
> feature** (new `crates/smartkey-core/src/vi_telex.rs` module, `[plugins]` section in
> `rules.toml`, `smartkey_engine_is_vi_lotus_enabled` FFI, addon + CLI plumbing).
> All line anchors in this document were verified against that working tree
> (post-feature state). If that feature lands first or gets rebased, re-verify anchors.
>
> Overlap points to coordinate: our `[diagnostics]` section is parsed in the **same**
> `extract_compiled` function that just gained `[plugins]` handling
> (`store.rs:770`), and we add FFI functions to the same file that just gained
> `smartkey_engine_is_vi_lotus_enabled` (`crates/smartkey-ffi/src/lib.rs:299`).

Existing docs for context: `docs/architecture-and-plan.md`, `docs/competitor-analysis.md`.

---

## 1. Background & motivation

The owner uses SmartKey daily and periodically observes: *"sometimes the thing just doesn't
work like intended and I want to investigate back."* The investigation found that current
logging covers some decisions but is blind to exactly the failure classes users report.

Owner's explicit asks that shaped this work (chronological):

1. "Does smartkey log what has been detected and autocorrected into and what has been skipped?"
2. "Can we log forever? Is that too space consuming? … at least we can try to log something
   like not high percent confident level" → became the *near-miss band* concept.
3. Retention preference: users will want to "surface the previous day or even previous hours
   about what happened while they're typing" → day-partitioned files.
4. Location: "check other programs and see where they log when they're not really a system
   app" → research-driven, see §11.
5. CLI access to logs is **future work**, not this iteration (owner's explicit call).
6. Vocabulary promotion: `protmpcord → promptcord` should work after a manual fix —
   "We can promote this directly" (owner approved 1x direct promotion).
7. Per-app learning scoping → future work (owner confirmed logging it).
8. Context investment: investigated but **decided**: Tier 2 trigram rerank is next after
   logging; neural tier and Harper are explicitly deferred (see §10, §12).

---

## 2. Audit: what SmartKey logs today

All diagnostics live in `~/.config/smartkey/learner_state.json` (`LearnerState`,
`store.rs:145`), written through on **every event** via `flush_state()`
(full JSON rewrite → tmp file → atomic rename). The fcitx5 addon contributes **zero** log
output (includes `fcitx-utils/log.h` but never emits a line).

| Signal | Where recorded | Anchor (working tree) |
|---|---|---|
| `AutocorrectApplied {typed, applied, kind, total_count}` | `applied_counts` map + 256-slot `recent_events` ring | `store.rs:114`, `store.rs:504` |
| `BackspaceUndo {typed, rejected, resolution, undo_count}` | `undo_counts` + ring | `store.rs:121` |
| `ManualBackspaceFix {orig, fixed, outcome, attempt_count}` | `manual_fix_counts` + ring | `store.rs:127` |
| `UnfixedUnknown {word, best_candidate, best_score, threshold, total_count}` | `untouched_unknowns` + ring | `store.rs:592`, called from `session.rs:1387` |
| Learning skips (`record_skipped`, 128-entry dedup'd vec) | `skipped_edits` + reasons (`delta_span_rewrite`, `mind_change_distance`, `target_not_valid_word`, `valid_word_no_context`, …) | `store.rs:173`, call sites `learner.rs:324-606` |
| Staging counters (`staged_corrections`, `staged_context`, `staged_undo_keep`, `untouched_unknowns`, `trouble_words`, `staged_telex_passthrough`) | `LearnerState` | `store.rs:145-161` |

Ring buffer cap: `MAX_RECENT_EVENTS = 256` (`store.rs:163`). Counters are unbounded.
View: `smartkey log [fixed|manual|unfixed] [--limit N] [--clear]` (`crates/smartkey-cli/src/main.rs`, `cmd_log`).

**Good news for investigations of the "score was too low" class:** `UnfixedUnknown` already
records the best candidate, its score, and the aggressiveness threshold
(`session.rs:1369-1387`: thresholds 0.24 conservative / 0.14 balanced / 0.06 aggressive),
and `smartkey explain <word>` can reproduce scoring offline.

---

## 3. Gap analysis — where misbehavior leaves no trace

1. **No timestamps, no app identity.** Events are ordered only; 256-slot ring overflows
   within hours of typing. "What happened yesterday in Firefox?" is unanswerable.
2. **Addon-level bypass is invisible.** `classifyProfile`
   (`adapters/fcitx5-smartkey/src/smartkey-addon.cpp:160`): capability flags
   (PasswordOrSensitive/Terminal/Url/Email/Digit/…), the GTK4 empty-program-name heuristic
   (dbus frontend without SurroundingText), and `classifyProgramName` lists — all return
   before the engine sees a keystroke. Nothing is recorded. **"SmartKey did nothing in app
   X" cannot be investigated.**
3. **Real-word typos declined are invisible.** The valid-word fast path
   (`scorer.rs:550`, Step 4b) returns `Unchanged{ValidWord, do_not_learn=true}` *without
   consulting context*. `"sent form"` leaves no trace unless an explicit
   `[context_corrections]` rule already exists (those are checked earlier, Step 1,
   `scorer.rs` ~302).
4. **Bypassed tokens invisible** (code tokens, URLs, camelCase, stretched words like
   `soooo`, single chars — all `Unchanged{Bypass/StretchedWord/ValidWord, do_not_learn=true}`).

---

## 4. Decisions (agreed with owner)

| # | Decision | Rationale |
|---|---|---|
| D1 | **Durable append-only JSONL event log**, one file per day, records `{ts_ms, app, lang, v:1, …event}` | Space is a non-issue (~150 B/event; interesting-only ≈ tens of KB/day); timestamps+app make bugs investigable; per-record `"v"` lets format evolve |
| D2 | **Default level `interesting`**; `all` available in config | Correct words are noise; interesting-only catches virtually every "looks like a bug" case |
| D3 | **`keep_days = 14` default, `0` = keep forever** (day-based pruning, no rotation logic) | Owner wants "previous day / previous hours" surfacing; pruning = deleting whole old files, crash-safe |
| D4 | **Location `~/.local/state/smartkey/logs/`** (XDG_STATE_HOME aware) | `rules.toml` is explicitly designed for Git sync — a growing keystroke-derived log must not pollute `~/.config/smartkey/`; XDG spec names logs as state-dir content; espanso precedent (see §11) |
| D5 | **Near-miss band:** log declined unknowns whose best score ≥ `near_miss_band` (default 0.5) × threshold | Owner's "log low-confidence decisions" idea; catches threshold misses without logging every novel word |
| D6 | **CLI query UI deferred to future work** (owner's call) | This iteration = core logging only; existing `smartkey log` keeps reading `learner_state.json` |
| D7 | **Unknown-target manual fixes stage at 1x and promote to `personal_dict` + `[corrections]` at 2x**, with **automatic 2x un-dictionary demotion** if a `personal_dict` word is itself manually fixed away 2x + **personal words become Step 5 correction candidates** | Refined during implementation review: 1x direct promotion risks shielding a mistyped target; 2x promotion + 2x auto-demotion self-heals mistakes while making `protmpcord→promptcord` and future adjacent typos (`promptxord→promptcord`) autocorrectable |
| D8 | **Rule supersession gets provenance**: supersede events in the log + one-line provenance comments stamped into `rules.toml` on machine-learned entries (`annotate_rules`, default on) | `add_correction` silently overwrites same-typed rules (`store.rs:693`) — old rule's existence vanished without trace |
| D9 | **Harper / neural tiers deferred**; Tier 2 trigram rerank is the next quality investment after this iteration | See §10; owner confirmed ordering |

---

## 5. Agreed design spec

### 5.1 Config — new `[diagnostics]` section in `rules.toml`

```toml
[diagnostics]
event_log = true                 # durable decision log on/off
log_level = "interesting"        # "interesting" | "all" ("all" = every committed word)
keep_days = 14                   # prune logs older than N days (0 = keep forever)
near_miss_band = 0.5             # log declined unknowns scoring >= band * threshold
annotate_rules = true            # provenance comments on machine-learned rules.toml entries
```

- Parsing lands in `RuleStore::extract_compiled` (`store.rs:770`) **after** the `[settings]`
  block, mirroring the hand-rolled `doc.get(...).and_then(Item::as_bool/as_str/as_integer/as_float)`
  idiom used by `[settings]`/`[plugins]` (unknown keys are already tolerated and preserved
  verbatim by `toml_edit` round-trips — forward/backward compatible for free).
- New `DiagnosticsConfig` struct + `EventLogLevel` enum (serde, `snake_case`); field
  `diagnostics: DiagnosticsConfig` on `CompiledRules` (`store.rs:206`).
- `Preset::initial_toml()` template (`store.rs:32`) gains the section (append after the
  `[plugins]` table, before `[corrections]` — TOML table ordering constraint).
- Since `CompiledRules` derives `Default`, add `impl Default for DiagnosticsConfig`
  with the values above so old configs without the section behave as spec'd.

### 5.2 Storage layout

```
$XDG_STATE_HOME/smartkey/logs/          # default ~/.local/state/smartkey/logs/
  events-2026-11-07.jsonl
  events-2026-11-08.jsonl               # one file per UTC?? day — see §7 note
```

Line format (one JSON object per line; `DiagnosticEvent` is already internally tagged
`#[serde(tag="event", rename_all="snake_case")]`, so merging host fields into the payload
object gives a flat record):

```json
{"event":"autocorrect_applied","typed":"teh","applied":"the","kind":"known_typo_dict",
 "total_count":42,"v":1,"ts_ms":1699430000000,"app":"firefox","lang":"en"}
```

### 5.3 New `DiagnosticEvent` variants (additive)

| Variant | Purpose | Level |
|---|---|---|
| `RealWordDeclined {word, prev, candidate, candidate_score, typed_score}` | Valid word committed while context preferred another candidate | interesting |
| `AppBypass {app, profile, reason}` | Addon profile transitions (`capability:terminal`, `program-list`, `dbus-no-surrounding-text`, …) | interesting |
| `RuleLearned {rule, stage, previous: Option<String>}` | Provenance on `rules.toml` mutation: `graduated_2x`, `personal_promoted_2x`, `personal_graduated`, `personal_demoted_2x`, `superseded` (with previous target) | interesting |
| `WordUnchanged {word, reason}` | Every unchanged token (bypass/valid/stretched) | **all only** |

`UnfixedUnknown` keeps its shape; JSONL writes apply the near-miss band filter
(D5) while the in-memory ring keeps recording everything as today.

---

## 6. Implementation plan (component by component)

### 6.1 `crates/smartkey-core/src/event_log.rs` (new module)

- `EventLogLevel` + `DiagnosticsConfig` (or keep config in `store.rs`; either is fine —
  owner's docs live in `store.rs` today).
- `EventLog { dir: Option<PathBuf>, app: Option<String> }` — **pure std**, paths injected
  by host per the store's stated convention ("Never reads `$HOME` implicitly; paths are
  always injected by the host", `store.rs:~221` doc comment).
- `record(&self, diag: &DiagnosticsConfig, ev: &DiagnosticEvent)`: level/band filtering →
  `OpenOptions::append(true).create(true).open(events-YYYY-MM-DD.jsonl)` per write
  (word-commit frequency; no cached handle needed) → serialize payload to
  `serde_json::Value`, merge `{v, ts_ms, app, lang}`, write line. **All I/O errors
  swallowed — the log must never break typing.**
- Pruning: on first write of a new day, `read_dir` + parse `events-YYYY-MM-DD.jsonl`
  names, `remove_file` older than `keep_days` (skip entirely when `keep_days == 0`).
- Date formatting: std has no civil-date formatter — implement days-from-civil →
  `YYYY-MM-DD` (Howard Hinnant's algorithm, ~20 lines) or reuse epoch-day arithmetic.
  **Timestamps must come from `SystemTime::now()` inside core** — see §7 warning about
  the host-injected `now_ms` being monotonic.
- Expose `date_string(epoch_ms)` for provenance comments (§6.5).

### 6.2 Store wiring (`store.rs`)

- `RuleStore` gains `event_log: EventLog` + `pub fn set_log_dir(&mut self, Option<PathBuf>)`
  (default `None` = logging off — in-memory tests and privacy unaffected until host opts in).
- Every `log_*` method (`store.rs:504, 531, ~566, 592`) additionally calls
  `if self.compiled.diagnostics.event_log { self.event_log.record(&self.compiled.diagnostics, &ev) }`.
- App identity: `RuleStore::set_current_app(Option<&str>)`; `SmartKeySession::set_app`
  forwards to it (single-engine multi-session addon already calls `set_profile` per key
  event the same way — same race profile, acceptable).
- Lang: stamp `store.active_lang`.

### 6.3 Real-word declined evaluator (`session.rs` commit path)

- Location: the `EvalOutcome::Unchanged` commit arm (`session.rs:1369`). Today
  `do_not_learn=true` (valid words) short-circuits all logging.
- Addition: when reason == `MatchKind::ValidWord`, lang == en, prev word present, and
  event_log enabled → run `scorer.explain(&word, &ctx, &cfg, &store)` (same call the
  unknown path already makes at `session.rs:1383`) → take top candidate ≠ word → compute
  `lexicon.bigram_score(prev, cand)` vs `bigram_score(prev, word)` → emit
  `RealWordDeclined` when candidate wins by a margin (start: ≥ 0.15 raw bigram delta;
  tune from logs later).
- **Observability only** — engine behavior must not change in this iteration.
- Cost: `explain()` ≈ FST automaton scan (~20 µs/class per README claims); per valid
  word with context only; negligible at typing rates. Gate behind `event_log` so
  `event_log=false` users pay nothing.

### 6.4 Supersede events + provenance comments (D8)

- `add_correction` (`store.rs:693`) and `add_context_correction` (`store.rs:~774`):
  before insert, read existing target; if different → `RuleLearned{stage:"superseded",
  previous: Some(old)}` event, then insert.
- If `annotate_rules`: after `tbl[typed] = toml_edit::value(fixed)`, set a decor suffix
  on the value item: ` # learned 2x 2026-11-08 (discord)` / ` # … — was: "term"`.
  `toml_edit` supports value decor; comments survive round-trips. Only ever applied to
  machine-learned writes, never to user-edited keys.

### 6.5 Vocabulary promotion & 2x auto-demotion (D7) — `learner.rs`

- Insertion point: the `SkipReason::TargetNotValidWord` branch
  (`learner.rs`, inside `record_manual_edit_inner`).
- Change: when the *target* is unknown to the lexicon (and not personal, not blocked),
  reject known typos (`lex.lookup_known_typos`), expressive stretches, `<4` char tokens,
  and key-bounce extensions (`orig -> orig + last_char`). Otherwise allow the edit into
  staging (`1/2`) and increment `untouched_unknowns` (`1/3`). On the **2nd** manual fix
  (`2/2`), promote `fixed_lower` to `personal_dict` (`RuleLearned{stage:"personal_promoted_2x"}`)
  and graduate `orig -> fixed` to `[corrections]` (`RuleLearned{stage:"graduated_2x"}`).
- Automatic un-dictionary (`personal_demoted_2x`): only core English lexicon words get the
  permanent Guard 2 valid-word shield. If `orig_lower` is only in `personal_dict` and the
  user manually fixes `orig -> fixed` 2x (or undoes + edits an autocorrect to a 3rd word 2x),
  `RuleStore::remove_personal_word` removes `orig_lower` from `personal_dict` and logs
  `RuleLearned{stage:"personal_demoted_2x"}`.

### 6.6 Personal words as Step 5 candidates — `scorer.rs`

- In Step 5 (`scorer.rs:649`), after curated smashes / FST scan: linear scan of
  `personal_dict` (typically dozens of words; µs-cheap), gate by char-length delta ≤ 2
  and `weighted_keyboard_cost ≤ ~3.0`, then `push_candidate(...)` with a modest bonus
  (start 0.10) and a new `MatchKind::PersonalDict` variant (`types.rs:110`).
- `push_candidate` already handles `is_pair_blocked` exclusion and bigram context term
  (`bg_term = 0.95 * bg`, `scorer.rs:693`).
- **Caution:** `MatchKind` is exhaustively matched in places (CLI `match_kind_slug`,
  `undo_resolution_label`, serde `rename_all=snake_case`) — add arms/wildcards everywhere.
  Run `cargo test --workspace` to catch.
- `UserRuleView` trait (`scorer.rs:13-41`) needs a personal-words accessor; `EmptyUserRules`
  returns empty iterator (default method keeps bench paths zero-cost).

### 6.7 FFI + addon

- FFI (copy the `smartkey_session_set_lang` pattern: `catch_unwind` + null guards):
  - `smartkey_engine_set_log_dir(engine, const c_char*) -> bool`
  - `smartkey_session_set_app(session, const c_char*)`
  - `smartkey_engine_log_app_event(engine, app_cstr, profile_u8, reason_cstr)` →
    `RuleStore::log_app_bypass` → `AppBypass` event
- Addon (`smartkey-addon.cpp`): `classifyProfile` is called from **two** key paths
  (`:297` and `:423` — plus diff shows recent GTK4 guard work). Track per-IC
  `lastLoggedProfile` in the IC property struct (`smartkey-addon.h`) and log **on
  transition only** (not per keystroke). App name = `ic->program()`.

### 6.8 Tests

- Unit: `DiagnosticsConfig` parse/defaults; day-file naming + pruning (fake clock or
  injectable date fn); near-miss band filter; supersede event; promotion path
  (`record_manual_edit` unknown target → personal + staged); personal candidate
  generation incl. blocklist respect; serde backward-compat (old `LearnerState` JSON with
  4-variant ring loads cleanly with new variants/fields — see §7).
- Corpus: `cargo test --workspace` + `cargo run --release -p smartkey-cli --bin
  smartkey-dev -- eval` must stay green (the 0% false-positive corpus gate — §6.3/§6.6
  are the only behavior-adjacent changes and must be no-ops at eval level).

---

## 7. Technical cautions for the reviewer

1. **`now_ms` is NOT wall time.** The addon injects `monotonicNowMs()` (steady_clock,
   `smartkey-addon.cpp:24`) — uptime-based. The event log's `ts_ms` **must** be taken
   from `SystemTime::now()` inside core (store.rs already uses `SystemTime` for mtimes,
   so this is in-convention). Using the injected clock would produce garbage dates.
2. **Serde backward compat of the ring buffer.** `LearnerState` has `#[serde(default)]`
   (struct-level), but a **new field on an existing `DiagnosticEvent` variant** (e.g.
   adding `prev` to `UnfixedUnknown`) breaks deserialization of existing state files
   unless that variant field carries `#[serde(default)]` itself. New *variants* are
   additive-safe. Existing live state files exist (~62 KB, 247 ring events) — don't break
   them.
3. **Threading:** all writes happen on the fcitx5 event-loop thread via `process_key`;
   `EventLog` needs no locking, but CLI+daemon can write concurrently — append-only
   O_APPEND writes of single <4 KB lines are effectively atomic on Linux; the existing
   store solves state-file races via mtime `sync_external_*_before_write` — the JSONL has
   no in-memory state to merge, so append-only suffices.
4. **Write-through cost:** `flush_state()` already rewrites the whole state JSON on
   *every* event today; a per-event JSONL append is strictly cheaper. No batching needed.
5. **Privacy:** at `all` level the log is effectively a plaintext record of typed words.
   Local-only, user-clearable (`smartkey log --clear` should delete JSONL files too when
   CLI lands), documented in README at ship time. Default `interesting` keeps volume low.
6. **Day boundary:** decide UTC vs local for file naming; local matches "yesterday while
   typing" intuition — recommend local date via the civil-date fn fed by
   `SystemTime::now()` (owner's surfacing use case is local-time based).
7. **Do not log content at `interesting` level beyond what's needed:** correction pairs
   and near-miss words are already in state files; the log adds timestamps/app to the
   same class of data, not new sensitivity classes — but `WordUnchanged` (all-level)
   records everything by design.

---

## 8. Learning-semantics audit (verified, for the reviewer's context)

Owner asked whether "relearning" works: user gets autocorrected A→B, undoes it, later
makes the same typo and fixes it to C. Verified behavior:

- **Pair-scoped blocking:** `add_permanent_block` stores `(typed, rejected)` pairs
  (`store.rs:631`; write path `learner.rs:245`). Blocking `(teh, the)` does **not**
  prevent learning `(teh, term)`. ✓
- **Two learning lanes** (`record_manual_edit_inner`, `learner.rs:288+`):
  - Non-word orig (`fomr`): Guard 2 (`learner.rs:426`) → staged per-pair at 2x →
    `[corrections]` global unigram. Keyboard smashes are context-free, so global is by-design.
  - Valid-word orig (`form`): Guard 2 blocks global learning; Guard 3
    (`learner.rs:480`) requires prev + bigram asymmetry → `[context_corrections]`
    (prev-keyed). This is exactly the "same typo means different things in different
    spots" answer: real words can only be learned context-scoped.
- **Precedence:** Step 1 checks `[context_corrections]` (score 9.0) before unigram
  `[corrections]` (8.5). ✓
- **Sharp edge (D8's motivation):** `add_correction` is a `BTreeMap::insert` — a later
  graduation **silently overwrites** an earlier rule for the same typed word
  (`store.rs:693`). No record of the old rule remained → provenance gap. Solution:
  supersede events + `annotate_rules` comments (§6.4).
- Per-app "don't learn here" beyond pair-blocking = per-app scoping → future work (§12).

---

## 9. Vocabulary promotion — the verified `protmpcord` chain

Owner asked: "when we type `protmpcord` it will be redirected back to `promptcord`, correct?
It did not do it in first place because it's not a real word. We can promote this directly."

Verified today's behavior — worse than the owner assumed:

1. `protmpcord` → no fix (not in lexicon). ✓ (expected)
2. Manual fix `protmpcord→promptcord` → **`skipped:target_not_valid_word`**
   (`learner.rs:410-424`) — the *target* is unknown, so it never even stages. (The skip
   IS visible in `manual_fix_counts` outcomes today.)
3. The 3x-untyped-vocab promotion requires typing `promptcord` correctly 3× → lands in
   `personal_dict`…
4. …**but `personal_dict` words are never correction candidates.** Verified: Step 5's
   candidate pool is FST-lexicon + curated smashes only; `is_personal_word` appears in
   the scorer solely as a shield (`scorer.rs` ~241, ~426). Promotion today = protection,
   not correction. The only current escape hatch is `smartkey dict install` (adds to the
   FST).
5. Also verified: `trouble_words` (boosted in `push_candidate` via
   `rules.trouble_word_boost`) only boosts candidates that already come from the lexicon —
   irrelevant for unknown vocab.

Fix = D7 = §6.5 + §6.6. Owner explicitly approved **1x direct promotion** (stronger
evidence than 3 untyped occurrences; Delta-Span/MindChange guards still apply). Easy
undo path: edit `rules.toml` (the word is a visible `personal_dict` entry).

---

## 10. Context research — tiered architecture & real numbers

Owner: "some users having powerful PCs (is it that demanding though?) can have more
context… some projects already have typing context… or we just implement a short
implementation that checks the previous word."

**Key finding: the "short implementation that checks the previous word" already exists
end-to-end.** Verified:

- Session keeps a 2-word window (`word` + `prev_word_before`).
- `[context_corrections]` are `(prev, typed)` lookups, checked first at score 9.0.
- Every Step 5 candidate gets a bigram term: `bg_term = 0.95 * bigram_score(prev, cand)`
  (`scorer.rs:693`), backed by bundled `data/sources/count_2w.txt` (Google Books bigrams).
- Learning Guard 3 uses bigram asymmetry (`learner.rs:480+`).
- The **gap** is only that valid words skip context entirely at eval time (Step 4b
  shield) — which is what this iteration's `RealWordDeclined` evaluator (§6.3) addresses,
  and the future "act on high-confidence real-word fixes" builds on.

| Tier | What | Latency | Footprint | Status |
|---|---|---|---|---|
| 0 | FST + 95,924 curated smashes + bigram term | <20 µs | MBs | shipped |
| 1 | Real-word context evaluator + context rules + dict import | µs | 0 | **this iteration** (evaluator) |
| 2 | **Trigram rerank** of top-k candidates by compressed n-gram model | KenLM measures **~1–2 µs/query** (probing); k≈10 → still µs | curated model **30–100 MB**, mmap, **zero new deps** (encodable in the `fst` crate; `smartkey-dev build-lexicon` pipeline exists) | **agreed next quality investment** (D9) — cheap enough to be default-on |
| 3 | Neural rerank (distilled T5/BERT via ONNX `ort` or pure-Rust `candle`) | full models **120–180 ms CPU/pass**; distilled+quantized ≈ 10–50 ms; real-time bar ≈ 50 tok/s | 60–250 MB model, 300 MB–1 GB RAM, C++ runtime dep | deferred; must be async sidecar; fcitx5 lacks clean "rewrite later" UX |
| — | LanguageTool integration | 10–100 ms/req, never in typing path | ~1 GB RAM base; **n-gram data 8–9 GB download / ~15 GB disk** for their/there-class confusion | optional adapter only ("if user already runs LT"); harvest LT-verified fixes into `[context_corrections]` |
| — | Harper integration | ms-class, async | "<1/50th of LanguageTool's memory" (README) | deferred (D9) — see §12 |

Answer to "is it that demanding?": **Tier 2 is not demanding at all** (µs queries, mmap'd
file, no deps). **Tier 3 is** (10–50 ms, hundreds of MB, async UX problem in fcitx5).

Sources:
- KenLM paper/benchmarks — https://github.com/kpu/kenlm (~1–2 µs trigram queries; quantized trie 2.55 GB → 1.19 GB)
- LanguageTool n-gram data — https://dev.languagetool.org/finding-errors-using-n-gram-data.html
- LanguageTool self-host sizing — https://serverroom.net/use-cases/languagetool-server
- On-device transformer latency primer — https://aman.ai/primers/ai/on-device-transformers
- Transformer spell-correction latency measurements (BERT ~120 ms / T5 ~180 ms CPU) —
  https://www.researchsquare.com/article/rs-9167443/latest.pdf
- Harper — https://github.com/automattic/harper

---

## 11. Ecosystem research (owner-requested)

**"Where do non-system programs log?"**
- **Espanso** (closest analog: user-space Rust text-expansion daemon): config in
  `~/.config/espanso`, **logs in `~/.local/share/espanso/logs/espanso.log`**; ships
  `espanso log` + `espanso path` commands — https://espanso.org/docs/command-line/cli-list/
- **XDG basedir spec**: `~/.local/state` is the defined home for "state data that should
  persist between restarts but is not precious: logs, history, recently used" —
  https://specifications.freedesktop.org/basedir-spec/latest/
- VS Code puts logs in `~/.config/Code/logs/` (widely considered messy).
- **Decision D4** = `~/.local/state/smartkey/logs/` (spec-correct; keeps the log out of
  the Git-synced config folder). CLI access deferred (D6) — when it lands, mirror espanso:
  a `smartkey log` that reads JSONL directly so users never need to know the path.

**"The red underline every system comes with" — Linux reality:**
There is **no system spellcheck service** on Linux (unlike macOS/Windows). Stack per
https://wiki.archlinux.org/title/Language_checking :
Firefox/Thunderbird → Hunspell directly; GTK apps → Gspell; KDE → Sonnet; most →
**Enchant** wrapper → Hunspell/Nuspell. Practical SmartKey integrations (all adapter-side
or file-parsing, keeping the core dep-free):
1. **Personal-dictionary import**: read what users already taught other apps — Firefox
   `persdict.dat`, LibreOffice `standard.dic`, Chromium custom dict → merge into
   `personal_dict` (opt-in). Cheap, immediate value. **Future work.**
2. **System Hunspell dicts as valid-word authority** (`/usr/share/hunspell`) — via the
   fcitx5 addon (fcitx5 already ships a spell addon backed by hunspell/enchant; exact
   addon-to-addon API needs a follow-up look) or an FFI "external dictionary provider"
   callback.
3. **Enchant as optional adapter dep** (C library) — never in `smartkey-core`.

**Harper** (https://github.com/automattic/harper): offline grammar checker, **pure Rust**,
parser/POS-based (not ML), Apache-2.0, integrates VS Code/Neovim/Helix/Zed/Obsidian/Chrome
via LSP/WASM/CLI; README claims ms-class linting and <1/50th of LanguageTool's memory;
English-only. SmartKey and Harper fix disjoint error classes: SmartKey = spelling-level
(edit-distance visible), Harper = grammar-level (all words valid: "a apple", "they goes",
"the the"). Integration, when it happens (§12), is async + opt-in, never in the keystroke
path.

---

## 12. Future-work backlog (explicitly deferred, owner-approved deferral)

Ordered roughly by expected value; each entry carries its spec so it can be picked up
without re-deriving:

1. **Tier 2 trigram rerank** (D9, next after this iteration) — spec in §10; build model
   via `smartkey-dev` data pipeline (fetch_sources.sh already pulls count_2w; add a
   curated count_3w/Wikipedia subset); encode in `fst`; rerank top-k in Step 5.
2. **CLI: `smartkey log` reads the JSONL** — `--today` / `--yesterday` / `--since 2h` /
   `--grep <word>` / `--json`; `--clear` deletes JSONL too; document the state-dir path
   in help output (owner: CLI designed later, not this iteration).
3. **`smartkey explain` with rule provenance** — cross-reference `RuleLearned` events:
   "matches `[corrections] fomr→from` — learned 2x 2026-11-07 (discord)".
4. **Counterfactual replay / aggressiveness tuning** — every declined record carries
   score + threshold → offline "what would aggressive mode have done with my last 30
   days"; also personal regression corpus for engine upgrades (requires self-contained
   records — why `v`/`app`/`lang` are baked in now).
5. **Per-app learning scoping** (owner: "log this in future work") — every record carries
   `app` from day one; later: learn only in prose apps, per-app aggressiveness.
6. **Personal-dictionary import** from Firefox/LibreOffice/Chromium (§11).
7. **Harper adapter** ("SmartKey Grammar" phase) — async, opt-in, feature-gated or
   sidecar; Harper-flagged fixes feed `[context_corrections]` with 1x graduation.
8. **LanguageTool adapter** — optional, only if user already runs the server; harvest
   verified real-word fixes.
9. **Tier 3 neural sidecar** — §10 numbers; big design question (async propose/revert UX
   in fcitx5); only for users who opt in.
10. **Bigram normalization tuning** — current `bg_term = 0.95 * bg` uses raw scores;
    conditional-probability/PPMI normalization is a cheap quality pass.
11. **Act on real-word context fixes** (strict margins) once §6.3's logs show the false-
    positive profile — turns `RealWordDeclined` from observability into behavior.

---

## 13. Verification plan (for the implementer)

1. `cargo test --workspace` (includes corpus false-positive gate).
2. `cargo run --release -p smartkey-cli --bin smartkey-dev -- eval` — golden corpus +
   latency must stay green (README claims <20 µs; §6.3/§6.6 are the only eval-adjacent
   changes and must be no-ops when event_log is off).
3. Manual: `smartkeyctl repl` (types + backspace learning sandbox) — verify
   `events-<today>.jsonl` lines appear with `v/ts_ms/app/lang`; verify supersede comment
   appears in `rules.toml` after double-fixing a word; verify `protmpcord` scenario (§9)
   end-to-end: fix once → personal_dict entry → second `protmpcord` autocorrects.
4. Addon: build via `adapters/fcitx5-smartkey/scripts` (CMake), switch focus between a
   terminal and Firefox, confirm `AppBypass` transition lines only (no per-keystroke spam).
5. Pruning: set `keep_days = 1`, drop an old dated file in the logs dir, type one word,
   confirm old file removed.
6. Old-state compat: point a build at a pre-existing `learner_state.json` (247 ring
   events) — loads cleanly, ring intact.

---

## 14. Open questions for the reviewer

1. Day boundary for file names: **local** (recommended, matches "yesterday while typing")
   vs UTC.
2. `RealWordDeclined` margin (start 0.15 raw bigram delta) — tune from real logs after a
   week; consider making it a config value only if users ever ask.
3. Whether `WordUnchanged` (all-level) should also record matched-but-unchanged valid
   words' top candidate (cheap via the same `explain()` call §6.3 already makes —
   recommend yes, `all` is explicitly the debugging level).
4. Promotion (§6.5) for **Vietnamese targets**: the existing lang-routing
   (`en → vi` for non-ASCII targets) applies before staging — confirm
   `add_personal_word` routes to `[lang.vi].personal_dict` accordingly (it takes `lang`,
   `store.rs:650` — expected fine).
5. FFI ABI versioning: additive functions only; confirm the project's stance on bumping
   `smartkey.h` for new exports (the in-flight vi_lotus feature just did the same).

---

## Appendix A: chronological decision log

1. Owner asks whether corrections/skips are logged → audit performed (§2, §3).
2. Owner asks about unlimited logging / space / low-confidence logging → space analysis;
   near-miss band designed (D5).
3. Design questions asked (level/retention/location) → answered: interesting (D2),
   day-based retention with "surface previous day/hours" rationale (D3), location
   delegated to research (D4).
4. Owner: CLI access deferred (D6); asks for more use cases for log+learning → use-case
   brainstorm (§12 items 3, 4, 5, 6 originate here); format must be self-contained from
   day one (v/app/lang baked in).
5. Owner asks about relearning + context-dependence → §8 audit; overwrite sharp edge
   identified → D8.
6. Owner asks about `smartkey explain` + provenance → agreed explain upgrade is future
   work (§12.3) once log exists.
7. Owner asks about `protmpcord` promotion → §9 audit reveals target-validation skip +
   personal-words-not-candidates; owner approves 1x direct promotion (D7).
8. Owner asks to invest in context / external engines → §10 research; Tier 2 agreed as
   next; Tier 3/Harper/LT deferred (D9).
9. Owner asks where logs should live + day-based retention confirmation → §11 research;
   D3/D4 finalized.
10. Owner requests this handoff → this document. Implementation not started.

## Appendix B: verified code anchor index (working tree, 2026-11-08)

| Anchor | Location |
|---|---|
| `DiagnosticEvent` enum (4 variants, serde-tagged) | `crates/smartkey-core/src/store.rs:114` |
| `LearnerState` + 256-ring | `store.rs:145`, `store.rs:163` |
| `CompiledRules` | `store.rs:206` |
| `Preset::initial_toml` template | `store.rs:32` |
| `flush_state` (whole-file rewrite per event) | `store.rs` (~490, post-diff) |
| `log_autocorrect_applied` / `log_unfixed_unknown` | `store.rs:504` / `store.rs:592` |
| `add_permanent_block` (pair-scoped) | `store.rs:631` |
| `add_personal_word` | `store.rs:650` |
| `add_correction` (silent overwrite) | `store.rs:693` |
| `extract_compiled` (hand-rolled TOML parse; `[plugins]` just added here) | `store.rs:770` |
| `record_manual_edit_inner` (guards; TargetNotValidWord) | `learner.rs:288`, `learner.rs:410-424` |
| Guard 2 valid-word shield / Guard 3 bigram asymmetry | `learner.rs:426` / `learner.rs:480` |
| Permanent-block write path | `learner.rs:245` |
| `UserRuleView` trait | `scorer.rs:13-41` |
| Step 4b valid-word fast path (context-blind) | `scorer.rs:550` |
| Step 5 non-word decoder + `push_candidate` bigram term | `scorer.rs:649` / `scorer.rs:693` |
| `evaluate_token` / Unchanged commit arm / unknown-log call | `session.rs:92` / `session.rs:1369` / `session.rs:1387` |
| `monotonicNowMs` (uptime clock — not wall time) | `smartkey-addon.cpp:24` |
| `classifyProfile` (+ GTK4 heuristic) | `smartkey-addon.cpp:160` |
| Profile set per key event (two call paths) | `smartkey-addon.cpp:297`, `:423` |
| Core deps (std-only OS access) | `crates/smartkey-core/Cargo.toml`: fst, levenshtein_automata, serde, serde_json, toml_edit |
