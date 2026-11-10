# Fix-Plan Batching: Cross-Reference of the Three Audits

**Date:** 2026-11-10
**Inputs:** `docs/cli-audit.md`, `docs/system-integration-audit.md`, `docs/lotus-bridge-audit.md` (repo state `v0.1.1-5-gbe2c346-dirty`)
**Purpose:** Map where the three audits overlap so fixes can be batched by code path (not by audit doc), and define one-session batches that stay conflict-free and bisectable. This is a planning document — no fixes applied, no decisions locked.

---

## 0. Principle

Don't batch by audit file — batch by code path. The three docs overlap in specific places, and the overlaps don't follow doc boundaries. Grouping by subsystem keeps each session conflict-free and reviewable.

## 1. Where the audits cross-reference each other

1. **Byte/char `deleteSurroundingText` bug** — filed in system-integration-audit §1; lotus-bridge-audit identifies it as the rescue path's prime victim (§2) *and* explains why no test caught it (the harness char-pops deletes, §5). Three mentions, one fix.
2. **Store write-safety** (system-integration-audit §6: shared `.tmp` race, non-atomic `flush_rules`, lost-update windows) — the CLI audit's "rules CRUD" (§4) and "config get/set" proposals would write through the exact broken path. Adding CLI write commands *before* fixing atomicity means building on the data-loss bug.
3. **Detection paths** — cli-audit §2's `smartkeyctl`/status issues and system-integration-audit §4's multiarch/`~/.local` gaps (`detect_installed_addon`, `cmd_plugin`) are the same functions in main.rs.
4. **`parse_locked` silent suppression** — a core bug (system-integration-audit §6) that manifests as a CLI UX bug (`smartkeyctl toggle` prints success while nothing was written, cli-audit §1 cross-effect); belongs with CLI error-reporting work.
5. **Lotus deferred evaluation + `telex_passthrough`** (lotus-bridge-audit §1/§2) — one fix (deferred boundary evaluation) unblocks the rescue misses *and* gives `telex_passthrough` its only natural wiring point; the fix also requires the host-contract test harness (§5) and the byte/char conversion from #1 to be verifiable.

## 2. Suggested batches (one session each)

### Batch 1 — Store integrity *(prerequisite for everything that writes)*
- Unique tmp name (pid/nonce) + fsync + rename for both `flush_state` and `flush_rules` (sys-int §6 P0s)
- Quarantine corrupt `learner_state.json`/`rules.toml` instead of default-reset / preset fallback; broken-TOML fallback respects `enabled=false`
- Surface `parse_locked` to callers (fixes the CLI "toggle lies" manifestation)
- Fix sync-ordering gaps: `record_untouched_unknown` pre-write sync, `reload_if_modified` before `flush_pending_unknown` in `session.reset`, `record_manual_edit_inner` sync-before-mutate
- Files: `crates/smartkey-core/src/store.rs`, `learner.rs`, `session.rs` only
- **Do first** — P0 data-loss risk, and every later feature writes through this path.

### Batch 2 — Lotus bridge correctness *(biggest, most coupled)*
- Deferred boundary evaluation for lotus-filtered boundaries (replaces the `NAV_OR_CLICK` drop) — fixes the `duongx` class *and* is the wiring point for #2 below
- Wire up `telex_passthrough` + English-lexicon cross-check ("momentum shield"); make the REPLACE branch safe when lotus is not composing
- Echo window ≥ lotus's `kReplacementAckSlackMs` (200 ms), stop arming on non-burst keys
- W2U=Everywhere support (or read lotus's config)
- **Byte/char conversion in `applyReplaceAction`** — same code path; only verifiable with the fixed host-contract test harness, which is also in this batch (model lotus-filtered boundaries, echo bursts, byte-accurate deletes; add vi traces to learning_traces.toml)
- Files: `adapters/fcitx5-smartkey/src/*`, `crates/smartkey-core/src/vi_telex.rs`, vi paths in `session.rs`, `tests/`

### Batch 3 — CLI surface *(independent, lowest risk)*
- 3a: B1–B8 from cli-audit §1, per-command help, exit-code policy, binary-name unification, `log` row alignment, `smartkey-dev --version`, bundled-dict fallback warning, detection paths (multiarch/`~/.local` fix from sys-int §4 rides along)
- 3b (after Batch 1): `rules` CRUD + `config get/set` — needs the new store removal methods and safe writes; plus JSONL log reading (`--today/--since/--grep/--json`, `--clear` deleting files) per handoff §12.2
- Files: `crates/smartkey-cli/src/*`, plus new store methods in 3b

### Batch 4 — Packaging & CI *(zero code coupling; can run anytime, in parallel)*
- Rebuild portable bundle against old-glibc baseline (or musl-static CLI); decide fcitx5-ABI policy for the prebuilt `.so` (sys-int §2 P0/P1)
- `[Dependencies]` → `[Addon/Dependencies]` + floor `core:5.0.20` (sys-int §1/§2)
- PKGBUILD `curl` dep; checksums for bundles; version single-sourcing
- CI matrix: Debian 12 container (glibc floor), aarch64, ctest + cargo test — would have caught the P0 artifact issue
- README preconditions + IM-env (`XMODIFIERS` etc.) guidance
- Files: `packaging/`, `scripts/`, `README.md`, cmake conf only — no conflict with Batches 1–3

## 3. What not to combine

Batches 1 and 2 both touch `session.rs`, but the review context differs completely (file-integrity semantics vs input-method event flow). Keeping them separate keeps each change bisectable — if something regresses, you know which batch caused it without disentangling two concerns from one diff.

## 4. Ordering

**1 → 2 → 3** (3a doable anytime; 3b gated on Batch 1), **4 in parallel whenever**. Each batch fits comfortably in one session.
