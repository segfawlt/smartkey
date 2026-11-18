# Findings: ngosen (`github.com/ngosen/ngosen`) vs SmartKey — Feature & Implementation Comparison (2026-11-18)

> **Handoff doc.** Comparative investigation of ngosen/ngosen against this repository, run to
> determine (a) which project is technically stronger and (b) which features SmartKey should adopt.
> Status: **INVESTIGATION ONLY — no code changed.** All claims below are backed by a probe run or a
> cited file/line; `[INFERENCE]` marks anything not directly executed.
> Probes used: `/tmp/bamptest` (bamboo-core 0.5.5, Telex), `/tmp/skprobe` (this crate's
> `LatinScorer`). Clone at `/tmp/ngclone` @ `7357c85e` (`1.12.1`).

## TL;DR

1. **ngosen wins as a shipped system; SmartKey wins on the algorithm.** Not the same product —
   ngosen is a Vietnamese *composer*, SmartKey is an English *corrector*.
2. **ngosen has no typo-repair feature.** Its claimed "Sửa lỗi gõ vội" is roadmap item #4 and is
   unimplemented. Measured: `tiếgn` → `tiếgn`, `recieve` → `recieve`.
3. **There is nothing to take on typo logic.** SmartKey is strictly ahead on that exact feature.
4. **What to take instead: ngosen's replacement-delivery machinery** — the 5-strategy delete ladder
   and the verify-and-retry loop. SmartKey's inline 4-path replacement can duplicate or drop text.

## Method

| Step | Command / artifact |
| --- | --- |
| Clone ngosen | `git clone --depth 1 https://github.com/ngosen/ngosen` → `/tmp/ngclone` (`7357c85e`, 1.12.1, 27 MB) |
| Run ngosen's real core | `/tmp/bamptest` → `bamboo-core = "0.5.5"`, `Engine::new(InputMethod::telex())`, feed keys, `commit()`, `get_processed_str(FULL_TEXT)` |
| Run SmartKey's real scorer | `/tmp/skprobe` → path-dep on `crates/smartkey-core`, `LatinScorer::new_bundled()`, `LanguageScorer::evaluate` |
| SmartKey eval gates | `cargo run --release -p smartkey-cli --bin smartkey-dev -- eval [--lang es\|fr]` |

## 1. Measured: ngosen does not repair typos

Probe `/tmp/bamptest` feeds real Telex keystrokes into bamboo-core 0.5.5 (the exact crate
ngosen vendors at `bamboo-rs/vendor/bamboo-core`, version confirmed `Cargo.toml` → `0.5.5`;
ngosen pins `0.5.2` vendored but the source read here is 0.5.5).

| Typed | ngosen output | Verdict |
| --- | --- | --- |
| `tiếgn` | `tiếgn` | ❌ not repaired — this is literally roadmap item #4 |
| `nhuwg` | `nhuwg` | ❌ not repaired — README claims "nhuwg + Space → nhưng" |
| `coongty` | `coongty` | ❌ not repaired — README claims "coongty → công ty" |
| `tihng` | `tihng` | ❌ not repaired |
| `recieve` | `recieve` | ❌ not repaired |
| `teh` | `teh` | ❌ not repaired |
| `wrods` | `wrods` | ❌ not repaired |
| `tieengs vieetj` | `tiếng việt` | ✅ works — but only as **unspaced** Telex, not as a typo fix |
| `hoangf` | `hoàng` | ✅ tone-mark placement (`free_tone_marking`) |
| `nest` | **`nét`** | ⚠️ **false positive** — English word destroyed |
| `too` | `tô` | ⚠️ false positive (their roadmap item #1) |
| `home` | `home` | ✅ |
| `part` | `part` | ✅ |

### Root cause (code)

`bamboo-rs/vendor/bamboo-core/src/engine/mod.rs:962` — `apply_auto_correct`:

```rust
if has_transforms && !self.is_valid_internal(work.as_slice(), false) {
    let raw_comp = crate::syllable::break_composition_slice(work.as_slice());
    ...
    work.clear();
    work.extend_from_slice(&raw_comp[..raw_len]);
    self.english_bypass = true;
}
```

This **reverts the composition to raw keys** when the result is not a valid Vietnamese prefix.
It is an undo, not a repair — it never searches a dictionary for a correction. Gated by
`Config::auto_correct` (`config.rs:83`, default `true`) and surfaced in the UI as the
`SpellCheck` toggle (`src/ngosen-config.h:307`, `src/ngosen-engine.cpp:391`).

Consequence: `tiếgn` is invalid → reverted to raw `tiếgn`. The user sees their typo verbatim.
`nest` *is* a valid syllable → composed to `nét`. Their own changelog documents the bug class
at `CHANGELOG.md:375` (*"gõ issue, sorry, coffee không còn mất chữ"*) — i.e. they fixed the
**worst** cases by list, not by algorithm.

### README claims are stale

`README.md:72` (Cứu chữ) and `:90` (doubled keys) describe behavior the current core does not
implement for space-separated input. `ROADMAP.md` item #4, *"Sửa lỗi gõ vội, như `tiếgn` thành
`tiếng`"*, is the honest statement of current state: **not built**.

## 2. Measured: SmartKey on the same inputs

Probe `/tmp/skprobe` → `LatinScorer::new_bundled()`, `ScorerConfig{dialect: Mixed,
aggressiveness: Balanced}`, empty `EvalContext`.

| Token | SmartKey | ngosen | Delta |
| --- | --- | --- | --- |
| `tihng` | → `thing` (KnownTypoDict, 0.96) | unchanged | SmartKey fixes |
| `recieve` | → `receive` (KnownTypoDict, 1.04) | unchanged | SmartKey fixes |
| `teh` | → `the` (KnownTypoDict, 1.33) | unchanged | SmartKey fixes |
| `adn` | → `and` (KnownTypoDict, 1.28) | unchanged | SmartKey fixes |
| `taht` | → `that` (KnownTypoDict, 1.22) | unchanged | SmartKey fixes |
| `wrods` | → `words` (FuzzyLexicon, 0.51) | unchanged | SmartKey fixes |
| `nest` | **SHIELD** (`ValidWord`) | **`nét`** | **SmartKey correct** |
| `too` | SHIELD (`ValidWord`) | `tô` | SmartKey correct |
| `home` | SHIELD (`ValidWord`) | `home` | tie |
| `part` | SHIELD (`ValidWord`) | `part` | tie |
| `issue` | SHIELD (`ValidWord`) | — | — |
| `sorry` | SHIELD (`ValidWord`) | — | — |
| `coffee` | SHIELD (`ValidWord`) | — | — |
| `tiếgn` | keep (`Bypass`) | keep | tie — neither repairs |
| `vieetj` | keep (`Bypass`) | keep | tie |

**SmartKey is strictly ahead on the feature in question.** It repairs 6/6 English typos measured
and shields 7/7 valid words; ngosen repairs 0/6 and corrupts 2/7 valid words.

### Eval gate numbers (this machine, 2026-11-18)

| Language | Must-fix recall | Must-not-touch | Latency |
| --- | --- | --- | --- |
| `en` | 95/95 (100.0%) | 141/141 (0 FP) | p50 0.8 µs / p95 69.0 µs / p99 550.8 µs |
| `es` | 43/43 (100.0%) | 56/56 (0 FP) | p50 0.9 µs / p95 68.0 µs / p99 88.0 µs |
| `fr` | 49/49 (100.0%) | 62/62 (0 FP) | p50 1.0 µs / p95 80.2 µs / p99 92.8 µs |

Caveat: these corpora are 97/99/111 hand-captured cases. ngosen's published 92.9 % / 96.9 %
covers 9,884 English words. **Not the same scale** — ngosen's number is over a wider net and is
not reproducible from its tree (no committed harness; `misc/gen-english-words.py` only regenerates
the word list). Do not compare the two directly.

## 3. What ngosen genuinely does better

### 3a. Replacement-delivery ladder (the real finding)

`src/core/ngosen-state-replace.cpp` is 78 KB and is **not** typo logic — it is the machinery for
*getting a correction into the app*. `TypingState::deleteAndCommit` (`:554-579`):

```cpp
struct Method {
    const char* name;
    bool (TypingState::*usable)(const ReplaceRequest&);
    void (TypingState::*run)(const ReplaceRequest&);
};
static constexpr Method methods[] = {
    {"overtype xtest",       &TypingState::overtypeXTestUsable,       &TypingState::runOvertypeXTest},
    {"overtype",             &TypingState::overtypeUsable,             &TypingState::runOvertype},
    {"del",                  &TypingState::delUsable,                  &TypingState::runDel},
    {"surrounding",          &TypingState::surroundingUsable,          &TypingState::runSurrounding},
    {"forwarded backspaces", &TypingState::forwardedBackspacesUsable,  &TypingState::runForwardedBackspaces},
};
// "system backspaces" is the fallthrough
```

Each strategy has an independent usability probe keyed off what the field *reports*
(`selectsOverAutocompletion`, `forwardsBackspaces`, `erasesWithCommittedDel`,
`deletesThroughSurrounding`) — behavior, not app name (`AGENTS.md`: *"Handle apps by how they
behave, not by name"*).

### 3b. Verify-and-retry after deletion

`src/core/ngosen-state-check.cpp` snapshots surrounding text, compares to what was sent
(`recordSendSnapshot`, `replace.cpp:581`), and when the app dropped the deletion:

- `fixDroppedDeletion` (`check.cpp:200-225`) recomputes residue (`deletionLeftOver`) and retries
  via the same ladder, but **only** when the report is byte-exact and no intervening key/commit
  occurred (`exact && untouched && !prefix.empty()`), otherwise it gives up;
- `surrUnanswered_` counter → after `UnansweredLimit` consecutive non-answers, `markSurroundingUntrusted`
  and stop trusting the field (`check.cpp:195`);
- stale vs. fresh snapshots distinguished via `surr_wait_sent_snapshot_fresh_` so a lagging app's
  stale report cannot satisfy the check.

### 3c. SmartKey's current state is weaker here

`adapters/fcitx5-smartkey/src/smartkey-addon.cpp:746-822` is a single inline sequence with four
outcomes: `delete_surround` → `forward_bs_delay15ms` → `forward_bs_sync` → `insert_only`.
No XTEST overtype, no committed-DEL strategy, no post-delete verification, no retry, no
"unanswered" accounting.

The file itself documents the consequence (`:573-574`):

> *"If `SurroundingText` is also invalid (e.g. a GTK4 terminal or raw surface) cannot delete the
> original word and would duplicate text (`orig + replacement`)"*

and `:750-752`:

> *"On Wayland, only call `deleteSurroundingText` when `surroundingSynced` is true; otherwise fall
> back to `forwardKey(BackSpace)` … `commitString` by 15 ms"*

So SmartKey's known failure mode is **text duplication or a silently dropped correction** on
surfaces where neither surrounding-text nor forwarded-backspace deletion works. ngosen's ladder
exists precisely because they hit every one of those surfaces.

### 3d. Other confirmed gaps (ranked)

| # | Gap | Evidence | Effort |
| --- | --- | --- | --- |
| 1 | **Zero CI** — `.github/workflows/` is empty | `find .github -type f` → nothing | 1 file |
| 2 | No headless fake-Host harness for `SmartKeySession` | `crates/smartkey-core/benches`, `fuzz/` do not exist; adapter has only `tests/addon_load_smoke.cpp` | medium |
| 3 | No published recognition-accuracy number | `smartkey-dev eval` prints precision/recall but no committed artifact | small |
| 4 | Replacement inline instead of strategy table | `smartkey-addon.cpp:746-822` | medium |
| 5 | No `app_quirks` split | capability flags read inline (`:547-567`) | small |
| 6 | No `CHANGELOG.md` for users | only `docs/` handoffs | small |
| 7 | No `Kiểm tra máy`-style self-check | `smartkey status` is a text dump | medium |
| 8 | Lotus `vietnamese.cm.dict` resolved at runtime only | `vi_telex.rs` `resolve_lotus_dict_path()` | small |

## 4. What NOT to copy

- **Typo logic** — there is none to copy; ngosen is behind.
- **The Python settings GUI** (qtpy/PyQt *and* PyGObject; documented as heavy and
  version-sensitive in their `DESIGN.md`). SmartKey is CLI-first; keep it that way.
- **Vendored Rust inside CMake with `--locked --offline` drift** — ngosen vendors bamboo-core
  `0.5.2` while crates.io is at `0.5.5`; every bump is a manual vendor + checksum shuffle
  (`test/vendored-crates-switch.sh`). SmartKey should use `--locked` + Dependabot, not vendoring.
- **Vietnamese-only scope** — ngosen is single-language; SmartKey's multi-pack design (en/es/fr/vi)
  is the differentiator.
- **`install.sh \| bash` without checksums** — ngosen does include SHA-256; SmartKey already has
  `.sha256` files, keep that habit.

## 5. Strategic note

ngosen's `ROADMAP.md` item #4 is typo repair — SmartKey's entire reason to exist. Item #1 is
*"stop converting `nest` → `nét`"*, which is the zero-false-positive shield problem SmartKey's
`ValidWord` path already solves generally. ngosen is one release away from needing a scorer like
SmartKey's.

Therefore: **do not converge.** ngosen adding typo repair does not justify SmartKey adding Telex
composition, macros, or a settings GUI. SmartKey's defensible position is the scorer + pack
compiler + learner **as a library**, consumed by ngosen-class frontends. The FFI already makes
that possible; nothing else in this space does at this quality.

One concrete overlap to watch: ngosen's `is_valid_word` / `spell_check` gate vs SmartKey's
`do_not_learn` `ValidWord` shield solve the same problem two ways. ngosen's CVC check
(`bamboo-core/src/orthography/spelling.rs`) is cheaper for the Vietnamese-specific case.

## 6. Recommended next actions

1. **Extract the replacement ladder** into a `Method{name, usable, run}` table in
   `adapters/fcitx5-smartkey/src/`, mirroring `ngosen-state-replace.cpp:554-579`. Add a
   `verifySurroundingAfterDelete` check with a retry cap, and an XTEST-overtype path for
   Firefox/Chromium address bars (currently a no-op in SmartKey).
2. **Add CI** — five jobs: build+test+eval, ASan/UBSan, MSRV, `--core-only`-style adapter build
   with no fcitx5, packaging. ngosen's `.github/workflows/build.yml` is the model.
3. **Port the fake-Host rig** — `test/core/fake-app.cpp` + `fake-host.cpp` model late/stale/
   never-reporting surrounding text with artificial latency; add seeded fuzz
   (`NGOSEN_FUZZ_RUNS`/`NGOSEN_FUZZ_SEED`) and golden replay (`replace-record/*.txt` byte-exact
   delete/insert sequences). SmartKey's `SmartKeySession` has the same exposure and no harness.
4. **Publish accuracy numbers** — check `smartkey-dev eval` output into CI as an artifact; state
   the method and corpus size in the README. Do not repeat ngosen's mistake of publishing numbers
   with no committed harness.

## Open items / not verified

- ngosen pins vendored bamboo-core `0.5.2`; this probe ran crates.io `0.5.5`. A 0.5.2-specific
  behavior difference is `[INFERENCE]` unlikely (the `apply_auto_correct` revert is long-standing)
  but was not tested against 0.5.2 directly.
- ngosen's 92.9 % / 96.9 % English numbers: **not reproducible** — no harness in tree.
- SmartKey recall/precision above is from a fresh local `smartkey-dev eval` run, not from docs.
- `/tmp/ngclone` and the two probe crates are throwaway artifacts outside the repo; nothing was
  added to or modified in this repository.