---
title: Zero-Copy EnglishLexicon::from_static_bytes with Cow::Borrowed
category: performance
module: crates/smartkey-core/src/lexicon.rs
tags: [lexicon, fst, zero-copy, performance]
problem_type: performance-regression
date: 2026-11-16
---

# Zero-Copy `EnglishLexicon::from_static_bytes` with `Cow::Borrowed`

## Problem

`EnglishLexicon::from_static_bytes(&'static [u8])` claimed in its doc-comment to borrow the embedded `.skdict` asset without heap allocation, yet delegated to `parse_image(bytes)` which copied the embedded FST and typo-pool sections into owned `Vec<u8>` buffers (~5 MB copied at every startup / FFI engine init).

## Symptoms

- Unnecessary ~5 MB heap allocation and memcpy on every `EnglishLexicon::from_static_bytes` construction despite `'static` slice lifetime.
- `from_static_bytes` could not be verified as zero-copy in unit tests.

## What Didn't Work

Reaching for `unsafe` raw-pointer casts or self-referential structs (`ouroboros` / `Pin`) is unnecessary in Rust here because the caller already passes `&'static [u8]` for the embedded binary blob and dynamic disk loads use `Vec<u8>`.

## Solution

Split section header/offset parsing from buffer ownership:

1. Store FST bytes and the curated typo pool as `Cow<'static, [u8]>` (or slice-backed `fst::Map<Cow<'static, [u8]>>`).
2. Add a pure slice-parsing helper `parse_sections(bytes: &[u8])` returning byte offsets/ranges.
3. Have `from_static_bytes(bytes: &'static [u8])` slice `'static` sub-slices directly into `Cow::Borrowed(...)` via `from_parsed`, while disk/dynamic `from_bytes` copies slices into `Cow::Owned(...)`.

## Why This Works

Separating range calculation (`parse_sections(&[u8])`) from lifetime binding preserves full `from_bytes(&[u8])` compatibility for dynamically loaded `.skdict` files while giving `'static` embedded lexicons true zero-copy borrowing with zero `unsafe` blocks.

## Prevention

Keep `from_static_bytes_borrows_fst_and_typo_pool_without_copy` passing in `crates/smartkey-core/src/lexicon.rs`, and require new `.skdict` sections added to `parse_sections` to preserve `Cow::Borrowed` in `from_parsed`.

## References

0
