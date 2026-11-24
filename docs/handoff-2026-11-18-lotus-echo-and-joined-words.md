# Handoff — Lotus echo-BackSpace fix follow-ups (2026-11-18)

## State at handoff
- Branch `main`, last commits: `64f8e76` (fix) and `e3e0555` (learnings). Both committed, add-on installed locally via `install-local.sh` (live `libsmartkey.so` contains the fix).
- `cargo test --workspace` green; `addon_load_smoke` green with asserts forced on.
- **Not yet verified on live typing** (needs the user, see "Pending verification").

## What was fixed (done)
Bug: `kiểu s⌫có␣` produced `kiểsóc `. Cause: `PreInputMethod` dropped any BackSpace inside the broad 350/300 ms Lotus burst window as a synthetic echo, so user BackSpaces never reached `SmartKeySession`; its shadow buffer kept `scos`, which `vietnamese_telex_rescue` turned into `sóc`, deleting 4 chars.

Fix: separate narrow echo window in `LotusBridgeTracker` (`kEchoBsFirstWindowMs=45`, `kEchoBsChainGapMs=30`) in `adapters/fcitx5-smartkey/src/smartkey-addon.h`; echo filter in `smartkey-addon.cpp` (`PreInputMethod`, Lotus branch) now uses `expectsLotusEchoBackspace()`. Broad window kept for surrounding-text/boundary guards. New trace label `lotus_pause+late_echo_suspect`. Details: `docs/learnings/integration-issues/lotus-echo-backspace-narrow-window.md` and `release-ndebug-vacuous-smoke-asserts.md`.

Evidence: replaying 473 traced BackSpaces (`scratch/replay_classifier.py` under the Antigravity brain dir for conversation 579005fc-d67c-456c-bbe9-0d6307406982) — old logic swallowed 119 user BackSpaces; new logic 0 swallowed, 0 echoes leaked.

## Pending verification (do first, needs the user's keyboard)
In Zen with Lotus on: (1) type `kiểu s⌫cos␣` → expect `kiểu có `; (2) type a word, hold BackSpace over part of it, keep typing. Then:
```bash
smartkey trace show --since 5m --grep BackSpace -n 40
```
Expect user BackSpaces as `lotus_defer_other`, not `lotus_echo_bs`; no `late_echo_suspect`. If `late_echo_suspect` appears repeatedly, 45 ms is too narrow under load: raise the constants (check `~/.config/fcitx5/conf/lotus.conf` `Default*Interval`/`PostDelay`).

## Remaining task A — investigate joined words in the learner (next, user approved)
Symptom: `~/.config/smartkey/learner_state.json` → `untouched_unknowns` (1806 entries) contains glued multi-word tokens, e.g. `chawcsthayascos` (≈ `chắc thấy có`), `neeuscos` (≈ `nếu có`), `chatgpnhuwng`, `cheetscheets`, also `vi:`-prefixed duplicates. Count 1 each, so low harm now, but repeats could get promoted into personal vocab or drive bogus corrections.

Hypothesis (unverified, from stored text only): SmartKey's session misses a word boundary (space) between Lotus-composed words, so `Composing(...)` keeps growing across a space. Possibly related to the same echo/boundary handling (`lotus_defer_boundary`, `queued_post_boundary`, `vk_boundary_*`, delayed commit).

Suggested approach (systematic-debugging, evidence first):
1. Find the originating traces: `rg -n 'neeuscos|chawcsthay|cheetscheets' ~/.local/state/smartkey/logs/` (trace files cover 2026-11-17/18 only; older entries have no trace). Look for `Composing(...)` continuing across a `space` `pre_key`.
2. State one hypothesis, reproduce with a trace-derived test, fix at the source, commit test+fix together.
3. Afterwards clean polluted entries: junk in `untouched_unknowns`, and the stray `scos->sóc` in `applied_counts` (and any related staged entries) — prefer a `smartkey` CLI path if one exists; back up the JSON first; do not hand-edit while the daemon is running without checking `smartkey status`.

## Remaining task B — pre-delete screen verification (design discussion first, do not implement blindly)
Idea: before a replacement deletes N chars, confirm the screen text ends with the word SmartKey believes is there; if not, skip the correction. Blocker: SurroundingText is intentionally ignored during Lotus bursts and was stale (`kiểu c|`) at decision time, so a naive check blocks valid corrections or misses bad ones. Needs a design (e.g. only check when surrounding text is known fresh, fail open otherwise) and the user's sign-off — this touches delete/commit ordering (see `docs/learnings/integration-issues/dbus-key-order-fix-and-dropped-delete-guard.md`).

## Repo conventions to follow
- Read `AGENTS.md` and `docs/learnings/README.md` first. Use `$HOME/.cargo/bin` on PATH.
- Verify: `cargo test --workspace`; C++ add-on test: `cmake --build adapters/fcitx5-smartkey/build --target addon_load_smoke && adapters/fcitx5-smartkey/build/addon_load_smoke adapters/fcitx5-smartkey/build/libsmartkey.so` (asserts are now forced on).
- Zero false-positive guardrails apply to `scorer.rs`/`casing.rs`/`vi_telex.rs` (`corpus_eval`, `learning_eval`, `vi_lotus_eval`).
- After a green runtime change on `main`: `./adapters/fcitx5-smartkey/scripts/install-local.sh && ~/.local/bin/smartkey status`.
- Commit only touched files, conventional messages; capture non-obvious lessons with the `compound` skill.
- Flight recorder: `smartkey trace status|show|on|off`; logs in `~/.local/state/smartkey/logs/{trace,events}-YYYY-MM-DD.jsonl` (trace keeps 14 days; privacy-bypassed apps are never traced).
