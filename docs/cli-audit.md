# Audit: `smartkey` / `smartkey-dev` CLI

**Date:** 2026-11-10
**Scope:** `crates/smartkey-cli` (`src/main.rs` 1309 lines, `src/dev.rs` 255 lines), the `smartkey-core` public APIs it could expose, and overlap with the diagnostics handoff backlog (`docs/diagnostics-event-log-handoff.md` §12).
**Repo state at audit:** `v0.1.1-5-gbe2c346-dirty`
**Status:** **Findings only — no fixes applied, no design decisions made.** This document is an inventory to plan from.

---

## 0. Summary

The CLI is functional and unusually complete for a beta (status / toggle / log / rules / explain / plugin / dict / init / repl), but it suffers from three categories of issues:

1. **Silent-misbehavior bugs** in the hand-rolled argv parsing (unknown flags swallowed, missing values ignored, inconsistent exit codes).
2. **Presentation debt** — zero styling, no per-command `--help`, two binary names in one CLI, drifting hand-aligned columns.
3. **Unexposed engine capability** — the entire manual rule-CRUD layer of `smartkey-core` is never called by the CLI, the durable JSONL event log is write-only, and all `[settings]` mutation requires hand-editing `rules.toml`.

All line references are to `crates/smartkey-cli/src/main.rs` unless noted.

---

## 1. Bugs — silent misbehavior

| # | Finding | Location |
|---|---------|----------|
| **B1** | `explain` treats **unknown flags as the token**: `smartkey explain --jso` evaluates `--jso` as a word to autocorrect, with no error. | main.rs:750–752 |
| **B2** | `explain --prev` / `--punct` **silently swallow missing values** — `--prev` with no argument is simply ignored. | main.rs:716–723 |
| **B3** | `explain --dialect` accepts anything and **silently coerces to Mixed** (`--dialect usx` → `Mixed`, no warning). | main.rs:724–730 |
| **B4** | `reload` **ignores all argv silently** — `smartkey reload --foo` behaves identically (dispatch calls `cmd_reload()` bare). | main.rs:1283 |
| **B5** | `plugin` success message hardcodes `'vi-lotus'` even when a different name was passed; unknown positional args silently accumulate. | main.rs:1035, 1082–1090 |
| **B6** | `dict update` prints "Could not download…" and **exits 0** — scripts cannot detect the failure. | main.rs:979–983 |
| **B7** | With the default `bundled-en` feature, a **broken/invalid installed dict is silently skipped** and execution falls back to the bundled lexicon. The user is never told their installed pack was ignored. | main.rs:34–50 |
| **B8** | `explain` **hardcodes aggressiveness threshold constants locally** (Conservative/Balanced/Aggressive `(min_score, margin)` pairs) instead of reading the engine's `ScorerConfig` — drift risk if core tuning changes. | main.rs:821–826 |

## 2. Consistency & correctness smells

- **Incoherent exit-code policy.** Nominal scheme is `0` success / `1` `Err(String)` / `2` top-level usage failure — but reload-unreachable, `dict update` download failure, `dict remove` no-op, and `init` already-initialized all exit 0. The only enforced rule is "any `Err` → 1" (main.rs:1304–1307). Scripts cannot rely on any code beyond "not 2".
- **Two binary names in one CLI.** Usage strings say `smartkeyctl` (main.rs:147, 252, 313, 349, 1124) while the help banner says `smartkey`. `smartkeyctl` is a filesystem symlink created at package time (`scripts/package-portable.sh:36,63`, `packaging/arch/PKGBUILD:44`), **not** an argv[0]-based alias — a redesign must not assume argv[0] aliasing, and the help text should pick one canonical name.
- **Missing `rules.toml` handled three different ways.** `explain` and `repl` silently fall back to an in-memory store (main.rs:766–770); `status`/`rules`/`log` error out. Same condition, three behaviors.
- **`log` rows misaligned.** `REAL_WORD_DECLINED` rows lack the `[Nx]` counter every other event row carries (main.rs:617–698).
- **Dead code.** `let _ = &mut store;` (main.rs:1145).
- **`smartkey-dev` drift.** No `--version`; its usage error lacks the `error:` prefix the main binary uses and exits 2 from a different convention (dev.rs:245–248).

## 3. Presentation gaps

- **Zero styling anywhere.** No colors in any output; the only ANSI escape in the whole crate is the REPL redraw line (`\r\x1b[2K> …`, main.rs:1162). Cargo deps are exactly `smartkey-core`, `serde`, `serde_json`, `libc` (Cargo.toml:21–25).
- **No per-command `--help`.** Usage text exists only inline inside unknown-flag error strings. The top-level help (main.rs:1251–1274) also omits: `explain`'s `--prev/--punct/--dialect/--sentence-start`, `init --dir`, `dict install/remove` arguments, and all aliases (`on/off`, `plugins`, `dicts`).
- **Sparse `--json` coverage.** Only `status` (single-line `StatusJson`, main.rs:116–193) and `explain` (raw `engine.explain_token()` serialization, main.rs:785–788) support it. `rules`, `log`, `plugin`, `dict` do not.
- **No shell completions** (bash/zsh/fish).
- **Hand-aligned columns already drifting** — see the `log` row inconsistency above and B8.

## 4. Missing common use cases — core capability exists, CLI never calls it

The engine's entire **manual rule-CRUD layer is unused by the CLI**. Today the only ways to shape the brain are: type in the `repl` sandbox (which drives the learning loop), or hand-edit `rules.toml`.

| Capability in `smartkey-core` | CLI exposure |
|---|---|
| `add_correction` (store.rs:1067), `add_context_correction` (:1126), `add_permanent_block` (:809), `add_personal_word` (:881), `add_telex_passthrough` (:1159) | **None.** Only the automatic learning loop (`learner.rs`, inside `SmartKeySession::process_key`) calls these. No command exists to manually add a fix, snippet, blocklist pair, or dictionary word. |
| Per-entry **removal** | **Does not exist in core either.** There is no `remove_correction` / `remove_context_correction` / `remove_snippet` / `remove_blocklist_pair`. The `toml_edit` table-removal pattern to copy already exists inside `add_permanent_block` (store.rs:820–845). Deleting one wrong learned rule today = hand-editing `rules.toml`. |
| Settings mutation (`dialect`, `aggressiveness`, `undo_window_ms`, and the six `[settings]` booleans) | Only `set_enabled` (store.rs:511) and `set_plugin_enabled` (:540) exist as comment-preserving setters. Everything else requires hand-editing TOML. |
| Durable JSONL event log (`~/.local/state/smartkey/logs/events-YYYY-MM-DD.jsonl`, per-record `app`/`lang`/`ts_ms` envelope, `crates/smartkey-core/src/event_log.rs`) | **Write-only — no code reads it.** `smartkey log` reads only the 256-event in-memory ring in `learner_state.json` (main.rs:480–701). `log --clear` calls `clear_diagnostic_logs` (store.rs:796), which clears counters + ring but **does not delete the JSONL day-files**. |
| Per-app / per-day statistics (every JSONL record carries `app` via `smartkey_session_set_app`) | Derivable from the JSONL, absent from the CLI. |
| `explain` rule provenance (`RuleLearned` events carry `stage`/`previous` provenance) | Not surfaced. Handoff §12.3 specifies the target UX: *"matches `[corrections] fomr→from` — learned 2x 2026-11-07 (discord)"*. |
| Bulk wordlist → `personal_dict` (Firefox `persdict.dat`, LibreOffice, plain `.txt`) | Absent. `dict install` only copies prebuilt `.skdict` binaries (main.rs:905–940). Handoff §12.6 plans browser/office-suite import. |
| Shell completions | Absent. |

## 5. Overlap with the existing roadmap

`docs/diagnostics-event-log-handoff.md` §12 (owner-approved backlog) already schedules part of §4:

- **§12.2** — exactly the missing `log` query surface: `--today / --yesterday / --since 2h / --grep / --json`; `--clear` should also delete JSONL files; document the state dir in help.
- **§12.3** — `explain` with learned-rule provenance (cross-referencing `RuleLearned` events).
- **§12.4** — counterfactual replay / aggressiveness tuning ("what would aggressive mode have done with my last 30 days") — a larger, separate effort.
- **§12.6** — personal-dictionary import from Firefox / LibreOffice / Chromium.

Note for the future GUI decision: there is **no IPC channel between the fcitx5 adapter and the CLI at all** — coupling is files-only (rules.toml mtime hot-reload on focus change, plus fcitx5's own DBus reload call from the CLI, main.rs:58–85). Anything "live" (current app, per-session state) would require new IPC; the CLI's only ground truth today is the shared files.

## 6. Severity grouping (triage, not decisions)

- **Quick wins / bug fixes** — B1–B8, exit-code policy, binary-name unification, per-command help, `log` row alignment, `smartkey-dev --version`, warn on bundled-lexicon fallback (B7).
- **Medium features** — `rules` CRUD (needs ~5 new removal methods in core, following the store.rs:820–845 pattern), `config get/set` (needs setters following `set_enabled`), JSONL log reading + `--clear` deleting files.
- **Bigger features** — `stats` aggregation over JSONL, `explain` provenance, wordlist/browser-dict import, shell completions, counterfactual replay (§12.4).
