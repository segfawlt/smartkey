# Findings: Manage Surface, Competitor Gaps, and Revised Plans (2026-11-24)

> **Status:** Revised after code-level audit against `HEAD` (following the `SpaceShift` commit).
> **Sources:** Repo scan, `docs/findings-2026-11-18-ngosen-comparison.md`, `docs/competitor-analysis.md`, `docs/ekakey-core-learnings.md`, and live code/runtime verification over `crates/`, `adapters/`, and `packaging/`.

## TL;DR

1. The CLI covers every file-side operation; what is missing is an interactive TUI manager (`smartkey tui`), three Vietnamese repair classes, and a native Fcitx5 toggle hotkey.
2. Original Gap 1, Gap 4, and Gap 5 designs contained bugs exposed during code audit (`tienf -> tiẻn` hijack, bare-ASCII mark corruption, unsafe compound splits, inverted HTTP method security, and `reloadConfig()` wiping per-IC safety state) and are corrected below.
3. Execution order: **Gap 5** (Fcitx5 hotkey) → **Gap 1** (Vietnamese Strategies 5a/6 & 7, deferring Strategy 8) → pause for layout check-in → **Gap 4** (`smartkey tui` with `ratatui` + `crossterm` + POSIX `flock`).

---

## Part A — Manage-Surface Scan

### What exists today

The `smartkey` CLI covers 14 command groups (`status`, `enable/disable/toggle`, `reload`, `init`, `rules`, `config`, `log`, `trace`, `explain`, `app`, `plugin`, `dict`, `repl`, `help`) using hand-rolled argument parsing (`crates/smartkey-cli/src/main.rs`).

User-visible state is file-backed:

- `~/.config/smartkey/rules.toml` — settings (`20` canonical keys in `RuleStore::list_settings` at `store.rs:3708-3730`), plugins, personal dict, blocklist, learned rules, app profiles, and diagnostics. Comment-preserving via `toml_edit`; syntax errors quarantine the file as `<file>.corrupt-<epoch_ms>` (`store.rs:829`).
- `learner_state.json` sibling — per-rule lifecycle (`ConfidenceTier` at `store.rs:219`, `classify_rule_confidence` at `store.rs:1653`), staged counters, `space_habits`, and recent events.
- `~/.local/state/smartkey/logs/` — day-partitioned `events-YYYY-MM-DD.jsonl` and `trace-YYYY-MM-DD.jsonl` (`event_log.rs`).
- Hot reload is mtime-based on focus change / session reset (`store.rs:1280`, `session.rs:1353`). Files are the only IPC surface between the Fcitx5 addon and the CLI (`docs/cli-audit.md:76`).

The workspace previously had no HTTP, async, or GUI/TUI crates (`Cargo.lock`), and packaging (`packaging/arch/PKGBUILD`, `scripts/package-portable.sh`) ships zero GUI toolkit dependencies.

### UI shape options considered

| # | Option | Tradeoff |
|---|---|---|
| A | **`smartkey tui` (`ratatui` + `crossterm`)** | Keyboard-first terminal UI inside the single `smartkey` binary; zero system libraries, no local server/CSRF surface, instant launch |
| B | `smartkey ui`: local HTTP server + static web page | Rich mouse-driven tables, but requires browser tab lifecycle, `Host`/CSRF token hardening, and keep-alive handling |
| C | Native GUI (`egui`, `tauri`, GTK4, Qt6) | Heavyweight packaging and system library dependencies; fights the minimal `PKGBUILD` and portable tarball |
| D | Fcitx5 `Configurable=True` dialog for all rules | Settings only; poor fit for rule tables, confidence tiers, and logs |
| E | Polish CLI output only | Cheapest, nothing interactive |

**Decision: A (`smartkey tui`).** It preserves zero-GUI-library packaging while eliminating browser-tab friction and HTTP security surface. Skip a `clap` CLI rewrite as out of scope.

---

## Part B — Competitor Gap Analysis (`ngosen`, `EkaKey`)

| # | Gap | Who has it | SmartKey state |
|---|---|---|---|
| 1 | Omitted vowel mark (`tienf` → `tiền`), `-ng` coda drop (`nhuwg` → `nhưng`), unspaced compounds (`coongty` → `công ty`) | `ngosen` `BambooEngine` | Missing from `vi_telex.rs` Strategies 1–5; Strategies 5a/6 & 7 scheduled, Strategy 8 deferred |
| 2 | Retroactive EN-to-VI retype on IM switch ("Cứu chữ") | `ngosen` | Rejected by design; delegated to Lotus |
| 3 | Multi-frontend wrapper (IBus, XIM, fcitx4) | `ngosen` | Fcitx5-only by design |
| 4 | Visual rule manager (per-entry delete, bulk-prune, staged queue, config, logs) | EkaKey sidepanel; Bamboo/Lotus GUI lists | CLI only today; `smartkey tui` chosen |
| 5 | Global master on/off hotkey (EkaKey `Ctrl+Alt+D`) | EkaKey | Scheduled via Fcitx5 `KeyListOption` (unbound by default) |

Deliberate non-adoptions: EkaKey phonetic matching (`docs/ekakey-core-learnings.md` §2.1), `ngosen`'s five-strategy XTEST delivery ladder (`docs/findings-2026-11-18-ngosen-comparison.md` §2), candidate popups, and global keyhooks.

---

## Part C — Detailed Findings and Corrected Plans

### Gap 1 — Vietnamese repair classes (`vi_telex.rs`)

**Audit corrections to original assumptions:**

- **`tienf` was NOT handled natively:** Running `smartkey explain --lang vi --dir /nonexistent tienf` showed that Lotus composes `tièn` (not in `vietnamese.cm.dict`), triggers restore fallback, and existing Strategy 5 hijacked `tienf` to **`tiẻn`**.
- **Unrestricted mark insertion on bare toneless ASCII corrupts names and words:** `vietnamese.cm.dict` lacks `duong` and `tien`, but inserting one mark on bare ASCII hits unique valid words for names (`Nguyen` → `Nguyên`, `Duong` → `Dương`, `Tien` → `Tiên`, `Tuan` → `Tuân`, `Phuong` → `Phương`). Meanwhile, `moi` is already in `vietnamese.cm.dict` and exits early at `vi_telex.rs:1762`.
- **Unrestricted compound splitting collides with `must_not_touch.txt`:** Checking only "both halves dict-valid, at least one composed" collides with mixed-case/stretched tokens (`macOS` → `ma có`, `APIs` → `a pí`, `soooo` → `sô ô`, `noooo` → `nô ô`). Meanwhile, `nina` and `cola` are already shielded upstream by the English lexicon, making them vacuous negative tests.
- **Trailing `-g` repair needs raw-Telex suffixing, not bare open-syllable checks:** Stripping `g` and requiring a dict-valid open remainder misses diphthong stems (`duowg`, `cuowg`, `tieeg`), and appending `"ng"` after composition misplaces tones (`hòa` + `ng` instead of `hoàng`).

**Corrected plan (Strategies 5a/6 & 7; Strategy 8 deferred):**

1. **Strategy 5a (before Strategy 5) — Mandatory `ie`/`ye` + coda circumflex insertion:** Trigger when mid-word Vietnamese composition occurred (`needs_uinput_restore_on_break || is_composed_vietnamese`), `n >= 4`, trailing byte is a strict tone key (`s/f/r/x/j`), and `!lower.contains("ee")`. Insert `'e'` after `"ie"` or valid `"ye"` (`pos == 0` or after `u`/`q`) before standard codas (`c|m|n|ng|p|t|u`), keeping the user's exact tone key (`tienf` → `tiền`, `vietj` → `việt`, `nguyenx` → `nguyễn`, `ddiems` → `điếm`), preventing Strategy 5's `tienf` → `tiẻn` hijack.
2. **Strategy 6 (after Strategy 5) — Unambiguous restore-class 1-mark insertion:** Trigger when mid-word Vietnamese composition occurred (`needs_uinput_restore_on_break || is_composed_vietnamese`), `n >= 3`, and trailing byte is a strict tone key (`s/f/r/x/j`). Try 1-mark Telex insertions (`d -> dd` at start, vowel double on `a/e/o`, `w` after `a/o/u`) and rescue only when `try_candidate_repair` yields **exactly one** distinct replacement (`buocs` → `bước`, `dduocj` → `được`). Preserves Strategy 5 (`duongx` → `dương`, `duongj` → `dương`), leaves ambiguous checked-coda rimes (`nuocs`, `thuocs`, `luots`) untouched, and never touches bare toneless ASCII (`Nguyen`, `Duong`, `Tien`, `moi`).
3. **Strategy 7 (after Strategy 6) — Raw-Telex `-n` insertion before trailing `-g`:** Trigger when `n >= 3`, `lower.ends_with('g')`, and `!lower.ends_with("ng")`. Require the prefix to compose Vietnamese runes mid-word (`is_composed_vietnamese || needs_uinput_restore_on_break` so incomplete diphthong stems like `duow` also qualify), then run `try_candidate_repair(&format!("{prefix}ng"))` so `nhuwg` → `nhưng`, `nhuwxg` → `những`, `duowg` → `dương`, and `hoafg` → `hoàng` (correct tone placement) while English `bag`/`thing`/`config`/`frog` stay `None`.
4. **Strategy 8 (compound split) — Deferred:** Revisit only with a mixed EN/VI false-positive corpus and explicit guards for casing, 3x stretch tokens, minimum half length (`>= 2`), and real Vietnamese syllable structure.
5. **Verification:** Unit tests in `vi_telex.rs`, session + undo + `must_not_touch.txt` tests in `vi_lotus_eval.rs`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `smartkey-dev eval`, and `install-local.sh`.

### Gap 5 — Fcitx5 native toggle hotkey

**Audit corrections to original plan:**

- **Do not call `reloadConfig()` on hotkey toggle:** `SmartKeyAddon::reloadConfig()` re-mmaps `.skdict` packs, closes `trace_`, and calls `resetSurroundingTrust()` on every IC, wiping learned dropped-delete circuit breakers.
- **Wire the full `Configuration` lifecycle:** `SmartKeyAddon` needs `FCITX_CONFIGURATION(SmartKeyConfig, ...)` (`KeyListOption toggleKey`, default `{}`), `getConfig()`, `setConfig()`, `readAsIni`, `safeSaveAsIni("conf/smartkey.conf")`, `Fcitx5Config` in CMake and `package-portable.sh`, and optional `notifications::showTip` OSD (`[Addon/OptionalDependencies] 0=notifications`, never `[Addon/Dependencies]`).
- **Sync all `Configurable=True` sites:** `adapters/fcitx5-smartkey/smartkey.conf.in`, `scripts/package-portable.sh`, and `adapters/fcitx5-smartkey/tests/addon_load_smoke.cpp`.
- **Intercept before IM/`BYPASS` early returns:** Check `config_.toggleKey` at the top of `onKeyEvent` (while keeping `smartkey-addon.h` free of inline FFI calls so `addon_load_smoke` links without `libsmartkey_ffi.a`).

**Corrected plan:**

1. FFI: add `smartkey_engine_is_enabled`, `smartkey_engine_set_enabled`, and `smartkey_engine_toggle_enabled` in `crates/smartkey-ffi` (calling `reload_if_modified()` before toggling and handling `parse_locked` cleanly), plus unit tests.
2. Addon: define `SmartKeyConfig` (`KeyListOption toggleKey`, default `{}`), implement `getConfig`/`setConfig`/`readAsIni`/`safeSaveAsIni`, match `toggleKey` at top of `onKeyEvent`, call `smartkey_engine_toggle_enabled`, reset active contexts via `resetContext` (without `reloadConfig()`), and emit `notifications::showTip` OSD.
3. Packaging & verification: update `CMakeLists.txt`, `smartkey.conf.in`, `scripts/package-portable.sh`, and `addon_load_smoke.cpp` (`getConfig`/`setConfig`/INI round-trip), run `cargo test --workspace`, `ctest`, and deploy locally.

### Gap 4 — Interactive TUI manager (`smartkey tui`)

**Corrected plan (pending layout check-in after Phases 0–2):**

1. **Dependencies:** Add `ratatui` + `crossterm` to `crates/smartkey-cli/Cargo.toml` (zero new system libraries).
2. **Concurrency safety:** Add POSIX advisory `flock` (`libc` already in deps) around `rules.toml` and `learner_state.json` read-modify-write sequences in `RuleStore` so live Fcitx5 flushes and TUI/CLI writes cannot clobber each other.
3. **Shared DTO layer:** Extract structured query/mutation helpers from `cmd_*` in `crates/smartkey-cli/src/main.rs` (also closing missing `--json` gaps on `dict` and `plugin`).
4. **TUI implementation (`crates/smartkey-cli/src/tui.rs`):** Tabs for Overview/Toggle, Rules & Staged, Settings (`20` keys), App Profiles, and Logs & Explain, ending writes with `notify_fcitx5_reload()` (`fcitx5-remote -r` / `busctl` fallback).
