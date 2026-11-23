---
title: "RuleStore Sidecar Lockfile and Atomic Rename Invariants"
category: architecture-patterns
module: crates/smartkey-core/src/store.rs
tags: [store, concurrency, locking, atomic-write, fcitx5]
problem_type: design-decision
date: 2026-11-24
---

# RuleStore Sidecar Lockfile and Atomic Rename Invariants

## Context

The Fcitx5 addon, `smartkey` CLI, and `smartkey tui` all run concurrent read-modify-write mutations against `rules.toml` and `learner_state.json` under the same configuration directory.

## Guidance

Serialize persistent `RuleStore` mutations on a stable sibling sentinel file (`<config_dir>/.smartkey.lock`) using Rust stdlib `std::fs::File::lock()` rather than locking `rules.toml` or `learner_state.json` directly.

- Open `<config_dir>/.smartkey.lock` with `OpenOptions::new().create(true).read(true).write(true).truncate(false)` so clippy `suspicious_open_options` stays clean and existing lock bytes stay untouched.
- Route all mutating methods through the re-entrant `RuleStore::with_file_lock` helper guarded by `lock_depth` so nested mutators do not deadlock on themselves.
- Keep `lock_path: None` in `RuleStore::load_readonly_or_default`, `RuleStore::in_memory`, and `RuleStore::from_toml_str` so inspect-only commands and tests never create `.smartkey.lock`.

## Why This Matters

`atomic_write_bytes` persists changes by writing a temporary file and renaming it over `rules.toml` or `learner_state.json`. Locking either target file directly locks an unlinked inode after the first rename, allowing subsequent writers to overwrite updates silently.

## When to Apply

Apply this pattern to every new constructor or persistent mutator on `RuleStore` in `crates/smartkey-core/src/store.rs`. Read-only loaders and pure in-memory helpers must never touch `.smartkey.lock`.

## Examples

- `RuleStore::with_file_lock` and `RuleStore::acquire_lock_file_at` in `crates/smartkey-core/src/store.rs`
- Multi-thread regression test `concurrent_store_writers_preserve_all_updates_under_file_lock` verifying concurrent `add_user_personal_word` and `set_user_correction` writes preserve all rules without temporary files left behind

## References

0
