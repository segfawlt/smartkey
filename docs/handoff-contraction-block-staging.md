# Handoff: 2x evidence for blocking curated contractions

Status: **COMPLETE — implemented, tested (`contraction_*` tests in `learning_eval.rs`), full workspace `cargo test` + clippy green. Only open decision §5.1 (evidence decay) remains.** Branch `main` at `e988b2a`, up to date with `origin/main`.
Uncommitted diff: `crates/smartkey-core/src/{learner.rs,scorer.rs,store.rs}` (+89/−12).
Also uncommitted but unrelated and pre-existing: local deletion of `.github/workflows/ci.yml`
(decide separately; do not silently commit it with this work).

## 1. Goal

Curated unconditional contractions (`dont -> don't`, `isnt -> isn't`, … — see
`UNCONDITIONAL_CONTRACTIONS`, scorer.rs:91) are near-100% corrections: the typed form is never
a valid English word. Today a **single** BackSpace after such a correction, followed by
re-committing the raw form, permanently blocks the pair on disk — and blocks it in a way
typing can never undo. Invariant being established:

> One accidental keypress must never permanently silence a curated rule, and repeated
> deliberate intent must always win over a block.

Concretely: rejection of a curated contraction pair requires **2x evidence across sessions**
before the permanent blocklist write (mirroring the 2x graduation the manual-fix learner
already requires), and graduating a learned correction must clear any stale block of the same
pair. Non-contraction pairs (ambiguous words, compounds like `alot -> a lot`, personal-dict
demotions) keep the current 1x-permanent behavior. Absolute un-blockability is NOT the goal —
`dont` is a real French word and raw forms are legitimate in code/quotes — so 2x evidence,
not impossibility.

## 2. Bug mechanics (pre-existing, for context)

Blocklist writers (only these three):

- W1 `record_undo_keep_verbatim` — Row 2 "Undo + Keep Verbatim" (`dont -> don't -> BS -> dont`).
  Was: 1x writes permanent block immediately (learner.rs, old line 170). BS may be a pure
  accident; the revert restores raw text and re-committing it counted as full rejection.
- W2 `resolve_restored_edit` Row 1 "EditedToThird" (`dont -> don't -> BS -> donut`).
  Was: 1x permanent block of `(raw, rejected)` once delta-span/mind-change guards pass —
  pure collateral damage for contractions; the user never re-typed the raw word.
- W3 manual `[blocklist]` entries in `rules.toml` / CLI blocklist commands — deliberate, fine.

Suppression chain in `scorer.rs` (unchanged by this work):

- Step 2 (rs:~427): learned `[corrections]` lookup — gated by `is_pair_blocked`, so a block
  *shadows* a graduated rule.
- Step 3 (rs:~527): personal dict / undo shield -> `Unchanged` before contractions are even
  consulted. `is_undo_shielded` also reads persisted `staged_undo_keep` (store.rs:~2735), so
  shields can outlive the session; `record_manual_edit` clears the undo shield when the word
  is not a lexicon word (pre-existing behavior).
- Step 3b (rs:~622): contraction table, each candidate gated by `is_pair_blocked`.

The deadlock: `add_permanent_block` does not merely shadow a matching learned correction — it
**erases** it (store.rs:1245-1260). So: graduate -> later 1x BS -> correction deleted + pair
blocked -> fixing manually twice re-graduates -> block still shadows it -> any future block
write deletes it again. Only escape: CLI blocklist-remove (main.rs:1002) or hand-editing
`rules.toml`. Field evidence: telemetry on one machine showed `dont->don't` applied 57x,
blocked by a single undo-keep on another machine.

## 3. Implemented so far (all in `crates/smartkey-core`, compiles clean via `cargo check`)

1. **scorer.rs:143** — `pub fn is_unconditional_contraction_pair(typed_lower, expanded_lower)
   -> bool`. Exact-match predicate over `UNCONDITIONAL_CONTRACTIONS` (lowercases the expanded
   side, so `("im", "I'm")` matches). Module `scorer` is pub; no re-export needed.

2. **store.rs:186** — new `LearnerState` field
   `pub staged_contraction_rejects: BTreeMap<String, u32>`. Key format matches the other
   staging maps: `"en:dont->don't"`. Struct has `#[serde(default)]`, so existing
   `learner_state.json` files load unchanged.

3. **learner.rs:158** — `fn reject_pair_permanently(store, lang, typed_lower, rejected_lower)
   -> bool`. For curated contraction pairs: increments `staged_contraction_rejects` (with
   `sync_state_before_write()` + `flush_state()`), returns `false` below 2 and only calls
   `store.add_permanent_block` at >= 2. For every other pair: immediate
   `add_permanent_block`, returns `true`. Returns `true` = pair is (now) permanently blocked.

4. **learner.rs:216** — `record_undo_keep_verbatim` now: session shield + session pair block
   always (in-session behavior unchanged), then early-returns
   `LearnEvent::ShieldedWordSession` when `reject_pair_permanently` is `false` (1x contraction
   rejection = nothing permanent on disk). The rest (staged_undo_keep counter, 2x personal-dict
   promotion, `GraduatedUndoKeep`) runs only when the pair is blocked.

5. **learner.rs:305** — Row 1 in `resolve_restored_edit` uses `reject_pair_permanently` and
   emits `LearnEvent::BlockedPairPermanent` **only when the pair actually got blocked**
   (learner.rs ~318-330: event vector now conditional). Editing to a third word still routes
   `(raw, fixed)` through the guarded manual-edit learner exactly as before.

6. **learner.rs:190** — `fn clear_pair_rejection(store, lang, typed_lower, fixed)`: removes the
   permanent blocklist pair via `store.remove_blocklist_pair` and resets
   `staged_contraction_rejects`. Called at **both** graduation sites:
   - plain correction graduation (learner.rs:612, inside the `count >= 2` branch of
     `record_manual_edit_inner`, right after `add_correction`),
   - context correction graduation (learner.rs:672, after `add_context_correction`).

   Rationale: graduation (2x deliberate fixes) is strictly stronger evidence than any earlier
   rejection; without the clear, a stale block shadows the new rule in the scorer AND the next
   `add_permanent_block` would erase the correction again.

Event-surface note: 1x non-contraction undo-keep and all 2x paths emit exactly the same
`LearnEvent` variants as before; `session.rs:2168` (mobile HeliBoard hook) routes through the
same learner functions and inherits the fix. FFI/adapter code needs no changes.

## 4. Remaining implementation work

### 4a. Regression tests (none written yet)

Conventions: `crates/smartkey-core/tests/learning_eval.rs` — use `RuleStore::load_or_init` on
temp-dir paths for disk-persistence tests, `RuleStore::in_memory` for unit tests, and the
`feed_keys` helper with key-spec strings (`"dont <BS> "` etc.; `<BS>` = BackSpace, apostrophe
is a plain char). Existing coverage to keep green: `six_row_matrix_and_guards_behave_exact_to_spec`
uses `alot` (unmerged compound — must still block at 1x) and `chagnes` (Row 3); those must not
change behavior.

Scenarios to add:

- T1 1x contraction undo-keep stages only: `"dont <BS> "` -> buf `"dont "`, event
  `ShieldedWordSession`, `staged_contraction_rejects["en:dont->don't"] == 1`,
  `!is_pair_blocked("dont", "don't")`. Reload store in a fresh session; `"dont "` still
  corrects to `"don't "`.
- T2 2x across sessions blocks: repeat the undo-keep in session 2 -> counter 2, block written
  (`is_pair_blocked` true); session 3: `"dont "` stays `"dont "`.
- T3 Row 1 collateral no longer blocks at 1x: `dont -> don't -> BS -> donut ` -> NO
  `BlockedPairPermanent` event, no block, counter 1; second occurrence -> block +
  `BlockedPairPermanent`.
- T4 Deadlock recovery (the important one): seed `add_permanent_block("dont", "don't")`, then
  perform the manual fix `dont -> don't` twice (either session-driven: type `dont ` (stays),
  BS into it, retype `don't ` — or directly via `record_manual_edit` twice). Assert graduation
  event, `!is_pair_blocked`, and that `engine.explain_token`/eval now autocorrects `dont`.
- T5 Non-contraction regression: `alot <BS> ` still blocks at 1x (already covered by
  six_row_matrix; add an explicit assert if cheap).
- T6 Predicate unit test: true for `("dont","don't")` and `("im","I'm")`; false for
  `("alot","a lot")` and `("dont","doesn't")`.

### 4b. Verification

- `cargo test -p smartkey-core` then workspace `cargo test` (also `corpus_eval`,
  `vi_lotus_eval`, FFI `c_abi_smoke`). NOTE: in this environment cargo is at
  `~/.cargo/bin/cargo` (not on PATH).
- `cargo fmt`, `cargo clippy --workspace`.
- No FFI/adapter changes required (pure core logic, event surface preserved) — a rebuild of
  the fcitx5 adapter is only needed to ship, not to validate logic.

### 4c. Docs (optional but recommended)

Several `docs/*.md` audits describe the old 1x-blocks-immediately behavior
(`architecture-and-plan.md`, `diagnostics-event-log-handoff.md`, `cli-audit.md`). One-line
updates for truthfulness.

## 5. Open decisions for the user (do NOT decide unilaterally)

1. **Evidence decay**: `staged_contraction_rejects` never resets on its own. Reject once,
   accept the correction 50x, reject once more -> permanent block from two rejections months
   apart. Candidate: reset the counter when the pair's correction is successfully applied
   (the `applied_counts` logging path). User has not decided.
2. **`ci.yml` deletion** in the working tree — unrelated to this change; keep, restore, or
   commit separately.

## 6. Repo / env notes

- Working tree: 3 modified files (above) + the deleted ci.yml. Nothing staged.
- `smartkey-dicts/` is a sibling checkout (dictionary sources), untouched.
- Blocked-path rules from user's global AGENTS.md apply (no `.env*`, no env dumping, etc.).

## 7. Known subtlety introduced — verify or adjust

For curated contraction pairs, the early return in `record_undo_keep_verbatim`
(learner.rs:216-219) skips the `staged_undo_keep` increment on the 1st rejection. Consequence
on the 2nd rejection: `reject_pair_permanently` writes the block (counter = 2) and returns
true, but `staged_undo_keep` only reaches 1, so the event is `ShieldedWordSession` — NOT
`GraduatedUndoKeep` — and the personal-dict promotion for that word effectively moves from
2x to 3x undo-keeps (moot once the pair is blocked, since the block already prevents the
correction). Options: (a) accept as-is and only assert the block in T2, or (b) restructure so
`staged_undo_keep` increments at 1x too and `GraduatedUndoKeep` fires when the reject counter
reaches 2. Existing tests do not cover this path for contractions, so nothing breaks today —
but the next agent should confirm with the user or fix before shipping.


## 8. Resolution of §7 (done)

Option (a) is **required**, not cosmetic: `RuleStore::is_undo_shielded` treats any persisted
`staged_undo_keep` key for the word as a cross-session shield (checked before the contraction
table). Incrementing it at 1x would silence `dont -> don't` in the very next session, defeating
the 2x requirement. The early return stays (now commented in code). Side effect: contraction
rejections never promote the raw form into `personal_dict`. `clear_pair_rejection` additionally
removes the pair's `staged_undo_keep` key on graduation so the leftover 2x key cannot shield it.
`ci.yml` no longer shows as deleted in `git status` (§5.2 moot).
