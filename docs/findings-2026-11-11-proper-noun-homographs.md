# Findings: Proper-Noun Homograph Bug — `polish → Polish` (2026-11-11)

> **Handoff doc.** Root-cause investigation of a false-positive proper-noun
> autocapitalization, plus a measured, layered prevention strategy designed to hold up
> as new language packs are added.
> Status: **root cause verified against live logs; dictionary fix + guard design agreed; nothing changed yet.**

## Symptom

Typing the ordinary English word `polish` (verb/noun) in any active app autocorrects it to
`Polish`. Observed live on 2026-11-11 in `zcode`: 4 firings, user backspace-undid twice
(`self_confirm_cancelled`, then `edited_to_third` → final word `polis`) before the learner
permanently blocked the pair.

Log evidence (`~/.local/state/smartkey/logs/events-2026-11-11.jsonl`):

```json
{"app":"zcode","applied":"Polish","event":"autocorrect_applied","kind":"proper_noun","lang":"en","total_count":1,"typed":"polish",...}
{"app":"zcode","event":"backspace_undo","rejected":"Polish","resolution":"self_confirm_cancelled","typed":"polish",...}
{"app":"zcode","event":"backspace_undo","resolution":{"edited_to_third":{"final_word":"polis"}},"typed":"polish",...}
{"app":"zcode","event":"manual_backspace_fix","fixed":"polis","orig":"polish","outcome":"skipped:valid_word_no_context",...}
```

## Root cause (dictionary)

- `smartkey-dicts/dicts/en/proper_nouns.tsv:19` contains `polish → Polish`
  (mirrored verbatim into `smartkey/data/proper_nouns.tsv`; both files are identical, confirmed by diff).
- The TSV's own header states the rule that nothing enforces:
  *"Only unambiguous proper nouns belong here. Non-proper words and homographs (e.g. 'may', 'march', 'will') are excluded."*
- `polish` is exactly such a homograph. The entry was added by the language/nationality sweep
  (lines 4–34 are language names) without a homograph check.

## Root cause (code — why the entry is unconditional)

`crates/smartkey-core/src/scorer.rs:762-788` — the proper-noun pass runs **inside** the
valid-word fast path (`contains_valid("polish")` is true at line 739) and fires with **no
scoring**: hardcoded `score = 4.5, margin = 4.5`, no threshold, no homograph/context guard.
Only `is_pair_blocked` can stop it.

Key subtlety: `english` and `monday` are *also* valid dictionary words, so a runtime
"is the lowercase form a valid word?" guard cannot discriminate — the runtime genuinely
cannot tell `english` (style-only capitalization) from `polish` (different word sense).
The discriminating signal must be computed at **dictionary build time** and shipped in the pack.

The user's learned `{ typed = "polish", rejected = "polish" }` block pair (written by
`add_pair_block`, `store.rs:1290` via the 1x EditedToThird rule) is why
`smartkey explain polish` now reports `UNCHANGED (valid_word)` while
`explain english` / `explain monday` still fire — a per-user workaround, not a fix.
Every fresh user will hit the same 4-firing / 2-undo battle.

## Measured evidence — corpus-based homograph detection works

Case-sensitive lowercase-vs-capitalized shares from the in-repo corpus
`data/sources/count_2w.txt` (Google Books bigram counts, already used by the build):

| word | lowercase count | capitalized count | lowercase share | verdict |
|---|---:|---:|---:|---|
| latin | 2,293,412 | 655,514 | **78%** | ❌ ambiguous ("latin alphabet") — **still in the TSV, unfixed** |
| polish | 760,419 | 285,123 | **73%** | ❌ ambiguous (the verb) |
| april | 643,028 | 2,745,591 | 19% | ✅ same sense, uncapped style |
| august | 390,302 | 1,915,789 | 17% | ✅ |
| english | 3,509,622 | 23,843,031 | 13% | ✅ |
| january | 369,864 | 3,470,011 | 10% | ✅ |
| monday | 310,930 | 10,933,483 | 3% | ✅ |
| greek | 159,185 | 1,534,515 | 9% | ✅ |
| danish | 0 | 119,213 | 0% | ✅ (pastry sense negligible in corpus — previously suspected, cleared) |
| dutch | 0 | 313,749 | 0% | ✅ |
| turkish | 0 | 137,442 | 0% | ✅ |
| vietnamese | 0 | 0 | — | (no bigram data; wordlist-based fallback needed) |

**The distribution is bimodal with a wide gap: lowercase *style* tops out ~19–20%;
lowercase *different word sense* starts at ~73%.** A single threshold (~40%) separates
"uncapitalized style" from "genuinely ambiguous homograph" cleanly. This is
language-independent: every future pack runs the same check against its own corpus.

## Agreed fix strategy (layered)

### F1 — Immediate dictionary fix (both repos)
Remove `polish → Polish` and `latin → Latin` from:
- `smartkey-dicts/dicts/en/proper_nouns.tsv` (source of truth)
- `smartkey/data/proper_nouns.tsv` (build-time mirror)

Then rebuild the FST (`smartkey-dev build-lexicon` → `crates/smartkey-core/assets/en_lexicon.skdict`),
rebuild/install the addon + CLI, and reinstall the pack (`smartkey dict install`).
Do **not** remove `danish`/`dutch`/`turkish` — corpus says they're clean.

Post-fix behavior is correct without any code change: `polish` stays lowercase (valid-word
fast path, `Unchanged`), deliberate `Polish` is untouched (TitleCase pass, `scorer.rs:362-374`).
Note: users who already accumulated the `(polish, polish)` block pair can keep it; it's harmless.

### F2 — Build-time homograph lint (the future-languages answer)
In `smartkey-dev build-lexicon` and the `smartkey-dicts` CI (`build-packs.yml`):
for every proper-noun entry, compute the case-sensitive lowercase share from the pack's
corpus (EN: `count_2w.txt`; other languages: their own corpus/bigram source, with a
wordlist-only fallback when bigram data is missing). **Reject entries ≥ ~40% lowercase share**
(fail CI, list offenders). Encodes the TSV header's stated rule mechanically — no curation
discipline required for future language packs that lack perfect dictionaries.

### F3 — Pack-format safety bit (runtime belt-and-suspenders)
Bump `.skdict` format to v3 with one `ambiguous` bool per proper-noun entry (the parser at
`lexicon.rs:300-334` already version-gates: proper nouns only load at `version >= 2`).
Scorer skips flagged entries at `scorer.rs:763`. Near-zero cost; protects against packs
whose corpora are weak. The v2 fallback keeps old packs working.

### F4 — (Deferred, optional) Context-gated capitalization for flagged words
The codebase already has the right primitive (`[context_corrections]` bigrams,
`"sent form" → "sent from"`). Flagged entries could capitalize only under language-y context
("speak polish" → "speak Polish"; "polish the code" untouched). Phone keyboards do this.
Defer until a real pack needs it — F1–F3 cover the known cases.

### F5 — Behavioral detection (complements F2, on-device)
`smartkey log stats` (see companion doc
`findings-2026-11-11-event-logging-audit.md`, item U1) surfaces any future bad dict entry
from user behavior: high undo-rate per `MatchKind` = dictionary bug signature.

## Design principle this restores

Valid words are already shielded from typo-correction unless context supports the fix
(`RealWordDeclined`, `[context_corrections]`). The proper-noun pass is the one intervention
exempt from that shield. F2/F3 restore the shield for ambiguous words while keeping the
feature for unambiguous ones (`english`, `monday`, `january`, …).

## Open items

- [ ] F1 dictionary removal + lexicon rebuild (10-minute change; needs a release/pack bump)
- [ ] F2 lint in `smartkey-dev` + dicts CI
- [ ] F3 `.skdict` v3 flag + scorer guard
- [ ] Decide wordlist-only fallback rule for languages without bigram corpora (F2)
- [ ] Audit `proper_nouns.tsv` for non-language homographs as packs grow (lint makes this automatic)
