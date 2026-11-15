# Audit: UX Flow & Decision Choices

**Date:** 2026-11-16
**Scope:** UX-flow and decision-logic audit of the current tree (`47542e7`). Takes a different lens from the three 2026-11-10 audits (`cli-audit.md`, `system-integration-audit.md`, `lotus-bridge-audit.md`) — those covered CLI surface, packaging/integration, and the lotus host contract. This audit asks: *what does the user experience, and are the design decisions right?* Where a prior audit already flagged something, it is cross-referenced instead of repeated.
**Companion docs:** `docs/cli-audit.md`, `docs/system-integration-audit.md`, `docs/lotus-bridge-audit.md`, `docs/architecture-and-plan.md` §3 (learning matrix spec), `docs/competitor-analysis.md` (adopted/rejected UX patterns), `docs/ekakey-core-learnings.md` (F1–F4 feature specs).
**Status:** **Remediation pass landed (`9ab2ef5`, `c168d47`, plus docs/build follow-up).**
- **Fixed:** Step 5a dialect & offensive metadata filtering (`scorer.rs`, verified on `Mixed`/`EnUs`/`EnGb`), true zero-copy `EnglishLexicon::from_static_bytes` (`lexicon.rs`), displaced-cursor surrounding-text undo guard across `en` and `vi` (`session.rs` + `smartkey-addon.cpp`), `vi-lotus` sentence-start TitleCase `If`/`Of`/`Or` passthrough (`session.rs`), unified Rust+FFI+Fcitx5 app classifier (`zcode` built-in bypass, fixed `unknown_app_policy` for named unlisted apps + `"default"` parsing, `smartkey_engine_classify_app`, logging all profile transitions, `smartkey app` CLI), stale checkout cleanup & `smartkey status` health check in `install-local.sh`, Waybar indicator config (`contrib/waybar/smartkey.jsonc`), README caveats & latency/undo clarity, and companion doc restoration (`docs/rfc-multilang-dict-packs.md`, `docs/findings-2026-11-11-*.md`).
- **Consciously kept as-is:** `Suspect`/`Provisional` confidence tiers fire immediately without gating (T5.2); `Ctrl+Z` stays a `NavOrClick` reset without feeding the learner (T3.3); runtime stays silent without OSD popups (Waybar consumes `smartkey status --json` for status visibility).
- **Code-verified clarifications correcting T5:** (1) `record_undo_keep_verbatim` already calls `reject_pair_permanently` at `1×` for non-curated pairs (`learner.rs` — stale doc-comment on `resolve_restored_edit` updated); (2) absent typo bigrams return `0.0 < 0.25`, so `2×` contextual graduation works (`learning_traces.toml`), though Guard 3 keeps un-learned real-word contextual rescue conservative by design.

Severity legend: **P1** = breaks the interaction contract in realistic scenarios · **P2** = wrong/invisible behavior worth a conscious decision · **P3** = polish / documentation alignment.

Sources: direct review of `crates/smartkey-core/src/{learner.rs, session.rs, store.rs (settings), types.rs}` and `crates/smartkey-cli/src/main.rs`, all `docs/`, plus deep-dive reviews of the fcitx5 adapter (`adapters/fcitx5-smartkey/src/smartkey-addon.{cpp,h}`) and Layer 1 (`scorer.rs`, `casing.rs`, `vi_telex.rs`, `lexicon.rs`).

---

## 0. Prior-audit follow-through — verified fixed in the current tree

The 2026-11-10 fix-plan batches landed. Verified directly:

- **P0 store integrity:** unique tmp (`.{base}.tmp.{pid}.{nonce}`), `sync_all` before rename, parent-dir sync (`store.rs:520-556`).
- **P1 char-vs-byte `deleteSurroundingText`:** now Unicode-char counts with an explanatory comment (`smartkey-addon.cpp:431-438`).
- **P1 CLI B1–B8:** unknown-flag errors, missing-value errors, per-command `--help`, exit-code policy (0/1/2) — all fixed with a regression test (`main.rs:3233-3288`).
- **Inert dependency floor:** `[Addon/Dependencies]` in both `smartkey.conf.in` and the embedded portable conf.
- **New since the 2026-11-10 audits:** rules CRUD (add/remove for fix/context/snippet/word/block/pin/passthrough), `rules staged`, `rules prune --dry-run`, confidence tiers, `config get/set` (18 validated keys), JSONL `log` queries, IM-environment checks in `status`, read-only commands no longer create config dirs, `log --clear` deletes JSONL files.

The CLI went from "unusually complete for a beta" to genuinely good. The remaining gaps below are the ones that matter for the *typing experience*, which no CLI fix can reach.

---

## 1. The interaction model as designed (the decision map)

The entire UX is one contract: **silent autocorrect at the boundary key + one-BackSpace revert + deterministic learning from what the user does next.** There is no suggestion UI, no highlight, no popup — by explicit design (`competitor-analysis.md` §4 rejects SmartType/ibus-typing-booster popups). Interception is minimal: only boundary-key `Replace` and the undo `BackSpace` are ever consumed; `Enter` never mutates text (`session.rs:854-866`).

Verified end-to-end flow:

1. **Type** — every keystroke pre-IM updates the shadow buffer (`Composing.buf`).
2. **Boundary** (space `, ! ? ) ] } " ”`; `.` `:` `;` get a 1-key lookahead so `teh.com` / `localhost:8080` / `don;t` prose survive) → evaluation waterfall (user rules → shields → non-word decoder → margin-gated accept).
3. **Corrected** — word + trailing boundary inserted in one commit; state `JustAutocorrected`.
4. **1× BackSpace within `undo_window_ms`** — one press snaps back to verbatim typing; session shield immediate.
5. **Then:** keep raw (→ shield; 2× → `personal_dict`), edit to a 3rd word (→ 1× permanent pair block + 2× guarded correction), retype the suggestion (→ Self-Confirm Cancel), or move on (→ untouched unknown staged, 3× → `personal_dict`).

This is a coherent and genuinely differentiated model (Pilcrow-style undo-learning on desktop, deterministic and inspectable). The findings below are about where the model's *assumptions* break and where its *numbers* encode opinions the user can't see.

---

## 2. Findings

### T1 — The model is built on an invisible 5-second window (P1, UX-contract mismatch)

`undo_window_ms` defaults to **5000 ms** (`store.rs:59`). README says "**Immediately** reverts" with no caveat. A user who pauses >5 s after a correction and backspaces gets a plain character deletion of the corrected word instead of a snap-back — with zero explanation. The window is configurable (`smartkey config set undo_window_ms`) but invisible in the moment. Trailing spaces typed after the correction *extend* the window (`session.rs:1544-1566`) — a subtle nicety nobody can discover.

Related undocumented moments:

- `Enter` deliberately never corrects (`session.rs:854-866`) — a mistyped last word ships in chat. Anti-surprise choice, correctly made, but nothing tells the user that's why the miss happened.
- ALL-CAPS input (`DONT`, `TEH`) is *never* fixed by design (`casing.rs:338-340`) — a real recall hole for Caps-Lock typists that deserves a README line.

### T2 — Everything the user needs to know lives in a terminal (P1, feedback gap)

There is **no user-visible feedback of any kind at correction time** — no OSD, no indicator, no per-app state. Concretely:

- `smartkey status --json` already emits `text: "SK"/"SK-OFF"` and a tooltip (`main.rs:289-296`) — clearly built for a panel widget — but **nothing in the repo consumes it**. No waybar/polybar glue ships.
- App misclassification is invisible: `AppBypass` diagnostics fire only on transitions *into* BYPASS (`smartkey-addon.cpp:489-492`); an unlisted terminal falling to PROSE logs nothing, and there is no command to ask "what profile is app X in?"
- Four degraded modes are silent stops: poisoned FFI handle (passthrough forever, `lib.rs:138-181`), engine-init failure, missing lexicon (snippets still work, fuzzy dead), parse-locked TOML. A user can run a dead addon for days; only `smartkey status` reveals it.

The 1× BackSpace *is* the feedback loop for corrections — that is fine. It is the *absence* of feedback for bypasses, failures, and misclassification that has no substitute.

### T3 — The shadow buffer is trusted, and apps lie (P1, corruption class)

All learning inference correctly lives in the core; but correctness rests on the buffer mirroring the app's text, and nothing verifies that between presses:

1. **Undo-after-mouse-click in no-SurroundingText apps** (med-high): correction → click elsewhere (no key event, no ST to reconcile) → BackSpace within the window → the Replace deletes N chars and re-inserts the typo **at the new cursor** (`session.rs:1044-1077`). X11/XIM apps are the classic setup; the ST reconcile (`session.rs:681-724`, 80 ms echo guard) fixes it only where ST exists.
2. **One-char-per-BackSpace assumption**: IDE unindent-on-backspace, auto-close-pair deletion, smart delete desync `EditingRestored`/`EditingPrevious` → wrong `(peak → buf)` learn pairs and wrong `delete_chars` on the *next* correction → visible text corruption.
3. **Ctrl+Z is a learning dead end** (med): any chord → `NAV_OR_CLICK` reset (`smartkey-addon.cpp:387-390`); the app undoes SmartKey's commit but no rejection is recorded. Habitual Ctrl+Z users never teach the learner — and the learner keeps "nagging" them, which reads as a bug from their seat.
4. **ForwardKey fallback can duplicate text** (med): no-ST + non-bypassed profile → synthesized BackSpaces (`smartkey-addon.cpp:439-444`); on frontends where forward is a no-op (the GTK4 case the addon itself documents at `:287-297`), delete is dropped, insert commits → `typo + replacement`.

The system's own answer to this class — deterministic shields, aggressive resets, Clause-Erase Cancel — is good, but these four paths are where determinism meets host chaos.

### T4 — App classification: default-PROSE is the wrong default for the product's core promise (P2)

- Unlisted terminal/IDE → **PROSE** → autocorrect inside code, violating "0% false positives in code" for any app not in the ~60-entry list (`smartkey-addon.h:85-133`) — e.g. `com.jetbrains.Datagrip`, `dev.zed.SomeFork` (segment match needs a hit).
- `unknown_app_policy = "bypass"` exists as the strict-allowlist escape hatch — but it **only applies when the program name is empty** (`store.rs:419-421`). A *named* unknown app never consults it. The knob users think they are setting is not the knob they are setting.
- **No CLI for app profiles**: `[app_profiles].bypass/chat/browser/prose` lists are README-documented but absent from `config get/set` (only `unknown_app_policy` is exposed, `store.rs:3319-3329`). Remediation for a misclassified IDE = hand-editing TOML.
- Substring matching quirks: `"signal"`, `"slack"`, `"telegram"`, `"chrome"` substrings can catch unrelated apps; reverse-DNS segments need ≥3 chars.

Capability-flags-first ordering is the right decision (field-level precision inside apps), and the empty-program/dbus guard shows hard-won GTK4 knowledge — the gap is specifically the *named-unknown default*.

### T5 — Learning decisions: mostly right-sized, with three opinionated edges

The 1×/2×/3× matrix is faithfully implemented (`learner.rs` vs `architecture-and-plan.md` §3) and the anti-poisoning guards are real, not decorative: Delta-Span (len diff > 2, `learner.rs:579`), MindChangeDistance (DL ≤ 1 for ≤4-char words, ≤2 for ≤7, ≤3/4 with ≥0.85 multiset overlap, `learner.rs:625-645`), valid-word shield, Guard-3 context asymmetry. Specific judgment calls worth a conscious look:

- **1× undo+edit = permanent pair block** (non-curated pairs, `learner.rs:359`) is the most aggressive choice in the system — Gboard's opaque decay this explicitly rejects. Defensible ("editing to a third word is 100% deliberate") and recoverable (graduating a manual fix clears the stale block, `learner.rs:209-226`), but note the asymmetry: **one** accidental mid-word edit permanently silences that pair, while **one** accidental undo-keep only shields the session. The contraction staging (2× + 10-accept evidence decay, `learner.rs:155-202`) shows the fix for the dangerous case was designed — it just does not cover typo→typo pairs.
- **Confidence tiers are informational only.** `Suspect`/`Provisional` tiers are computed and displayed (`rules list --suspect`) but do not gate firing: a 2×-graduated wrong rule fires immediately across all apps from the moment it lands. The tier system answers "which rule is dubious?" but not "should dubious rules wait?" — a product decision currently made implicitly (fire-now) rather than explicitly.
- **Context corrections are near-inert by construction.** Guard 3 requires `bg_orig < 0.25 ∧ bg_fixed ≥ 0.50 ∧ Δ ≥ 0.35` — and every bigram in the table is quantized to ≥ 0.314 (`lexicon.rs:976-983`), so `bg_orig` must be *absent* and `bg_fixed` top-frequency. The whole `find_real_word_context_alternative` machinery (`scorer.rs:238-297`) is diagnostics-only today. Combined with the real-word zero-tolerance policy (below), `sent form → sent from` fixes essentially only come from hand-written rules. Coherent, but it should be written down as a decision, because the corpus evidence log (`real_word_declined`) is mining fuel that nothing mines.

Strengths here deserve equal billing: Self-Confirm Cancel, Clause-Erase Cancel, the `has_prior_undo → 1×` fast-track (`learner.rs:980-986`), the personal-word demote path (edit X→Y 2× removes X, `learner.rs:360-377`), case-learning staging that refuses ordinary dictionary words (`hello→Hello` at sentence start can't hijack, `learner.rs:451-547`), pinned words, and vi-mode's refusal to learn pure-ASCII Telex key edits (`learner.rs:565-567`) — each closes a real poisoning vector the competitors shipped.

### T6 — Layer-1 numbers: shield-first is the right spine; four embedded opinions to surface

The shield→candidate→margin ordering (`scorer.rs:307-1234`) plus build-time defenses (valid words excluded from the typo dict at `lexicon.rs:903-909`, proper-noun homograph lint with a 60-freq gap at `lexicon.rs:585-640`, freq-0 slang shielded-but-never-proposed) is the strongest FP architecture in the ecosystem, backed by a hard 158-entry `must_not_touch` gate and the zero-FP assert in `corpus_eval.rs:20-47`. Embedded decisions worth surfacing:

1. **Real-word zero tolerance is absolute**: `form→from`, `its→it's`, `quiet→quite` will *never* be auto-fixed — not contextually, not in Aggressive mode (`scorer.rs:839-845`). Correct trade for silent autocorrect (a wrong valid-word fix is invisible and undo-learning can't catch what the user didn't notice), but it is the single largest recall limitation and currently documented nowhere user-facing.
2. **Step 5a filter gap** (P2, the one genuine FP vector found): the curated typo-dict source skips the `is_offensive()`/dialect-validity checks every other source applies, and falls back to freq 145 for targets missing from the lexicon (`scorer.rs:960-990`). Data-dependent, one guard away from closed.
3. **Dialect `mixed` has a US output bias**: UK input accepted untouched, but unseen typos fix to US spelling (`map_dialect`, `lexicon.rs:395-403`) — a GB user on the default gets Americanized corrections. Documented in a doc-comment (`types.rs:7-9`), invisible in config UX.
4. **The `< 20 µs` claim is unenforced**: the common path plausibly meets it (curated-hit short-circuit at `scorer.rs:993-995`), but smash-rescue is an O(N) linear scan over tens of thousands of entries (`scorer.rs:1090-1142`), stage-2 d≤2 streams the full FST, vi rescue fires 30–50 Lotus simulations per word, and `smartkey-dev eval` prints percentiles without asserting them (`dev.rs:222-229`). Also: `from_static_bytes` claims zero-copy but copies 5 MB (`lexicon.rs:343-358`).

Also worth a look: early-Shift repair requires shifted punctuation, so `whatS.` before `.`/space is unhandled while `situatioN:` is (`casing.rs:414-442`); 2-char unknown typos depend entirely on the curated dict (`scorer.rs:998-1006`); the `anagram_bonus` (+0.20) is safe only while Step 4b shields valid anagrams (`scorer.rs:908-916`, latent).

### T7 — vi-lotus: careful engineering, two boundary gaps

The Telex sim is a verified 1:1 lotus port with exact `screen_chars` accounting (correct deletes on rune words — pinned in `vi_lotus_eval.rs:111-193`), macros step aside via suffix-verification (`session.rs:1716-1727`), and Off/Default mode correctly routes raw ASCII to the English scorer (the `oof` corruption class is pinned out, `vi_lotus_eval.rs:327-339`). The English-Sentence Momentum Shield (`of→ò` protection, `session.rs:95-156`) now exists and is wired. Two boundary decisions to note:

- **Sentence start is unprotected** (med): the shield requires a prev word (`session.rs:115-117`), so `Me → Mệ`, `Be → Bệ`, `he → hệ` at sentence start are unrestorable — the one place SmartKey adds nothing over raw Lotus for its headline collision class. The ~15-word core collision list could extend to sentence-start at near-zero FP risk.
- **Burst windows are wall-clock heuristics** (350/300 ms, `smartkey-addon.h:174-177`): a stall longer than that turns lotus's synthetic echo BackSpaces into phantom *user* backspaces → mislearning. Widened correctly vs. the prior audit (≥ lotus's 200 ms ack slack), but no expiry-miss detection.

### T8 — Docs & promise alignment (P3)

- README "**Immediately** reverts" vs the 5 s window (T1); "steps aside in passwords" is true only where the frontend reports `PasswordOrSensitive` — **X11/XIM apps can't**, so SmartKey runs in password boxes there (`smartkey-addon.cpp:253-256`). The trust-relevant caveat deserves README placement.
- `ekakey-core-learnings.md` references `docs/rfc-multilang-dict-packs.md` and `docs/findings-2026-11-11-event-logging-audit.md` — **neither file exists** in `docs/` (dangling references; the multilang RFC presumably carries the Tier A/B pack design).
- `install-local.sh` has no post-restart health check (doesn't run `smartkey status`), and the moved-checkout `build/` cleanup only triggers for `/usr` prefixes (`:27-29`) — AGENTS.md documents the manual fix; the script could do it.

---

## 3. Consolidated priority view

The two highest-leverage gaps are both in T1/T2, not in the engine: (a) the interaction model's hidden parameters (5 s window, Enter-never-fixes, ALL-CAPS-never-fixes, real-word-never-fixes) are invisible exactly when a user is asking "why did it just do that?", and (b) silence at runtime means misclassification, poisoning, and dead-addon states surface only through CLI archaeology. The engine itself — shields, guards, staged learning, sim fidelity — is in strong shape and its remaining defects are narrow (5a filter, sentence-start vi shield).

Ranked decision points (findings only — no fixes applied):

| # | Finding | Theme | Sev |
|---|---|---|---|
| 1 | Undo Replace fires at a displaced cursor in no-ST apps (click mid-undo → text corruption) | T3 | P1 |
| 2 | No runtime feedback surface; `status --json` indicator payload has no consumer; misclassification/dead-addon invisible | T2 | P1 |
| 3 | 5 s undo window + Enter/ALL-CAPS/real-word exclusions undocumented user-facing | T1/T6 | P1 |
| 4 | `unknown_app_policy` only applies to empty program names; no CLI for app-profile lists; named-unknown default PROSE | T4 | P2 |
| 5 | Ctrl+Z and app auto-edits are learning dead ends / buffer desyncs | T3 | P2 |
| 6 | Step 5a skips offensive/dialect filters + freq-145 fallback (only real FP vector) | T6 | P2 |
| 7 | vi sentence-start collisions unprotected (`Me → Mệ`) | T7 | P2 |
| 8 | Confidence tiers don't gate firing; is fire-now the intended policy? | T5 | P2 (decision) |
| 9 | `<20 µs` claim unenforced; smash-rescue O(N) path; eval percentiles not asserted | T6 | P2 |
| 10 | 1× permanent-block asymmetry vs contraction staging; context-learning near-inert by bigram quantization — document as conscious choices | T5 | P3 (decision) |
| 11 | Mixed-dialect US output bias; early-Shift `.`/space asymmetry; `from_static_bytes` not zero-copy | T6 | P3 |
| 12 | README password/X11 caveat; dangling doc references; install health check | T8 | P3 |

## 4. What's working well (worth keeping exactly as-is)

- Shield-first pipeline ordering; user rules outrank everything fuzzy.
- The 1×-BackSpace-as-feedback contract and minimal key interception (only boundary `Replace` + undo `BackSpace` are consumed; `Enter` never mutates).
- Deterministic anti-poisoning guards: Self-Confirm Cancel, Clause-Erase Cancel, Delta-Span, MindChangeDistance, valid-word shield.
- Contraction 2× staging with evidence decay; the 1× fast-track after an undo-keep; the personal-word demote path.
- vi `screen_chars` fidelity and the macro suffix-verification step-aside.
- Build-time FP engineering: typo-dict valid-word exclusion, proper-noun homograph lint, freq-0 slang shield-but-never-proposed.
- Graceful no-lexicon degradation (rules + snippets + vi rescue keep working).
- The plain-text brain with hot reload — the inspectability promise (`rules.toml` + `rules staged` + `explain` + `log`) is real and competitors don't have it.
