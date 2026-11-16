# RFC: Multi-Language Dictionary Packs (`.skdict` v3 + generic builder)

**Status:** APPROVED & LOCKED (2026-11-16). Decisions in §2 are finalized and propagated through §3–§10 and Appendix A.
**Date:** 2026-11-12 (updated 2026-11-16)
**Scope:** `smartkey` (core/CLI/FFI/fcitx5) with `dicts/<lang>/` monorepo recipes (`smartkey-dicts` frozen at last v2 release)
**Goal:** A contributor adds a language by writing `dicts/<lang>/pack.toml` (pinned upstream
sources + sha256 + license) plus small curated overlay files. The generic build tool emits a
valid `.skdict` v3 pack. Adding a Tier A language requires **zero Rust edits**.

---

## 1. Verified current state

All facts below were re-verified by reading the code in this session unless marked *inherited*.

### 1.1 Format & lexicon (`crates/smartkey-core/src/lexicon.rs`)

| Fact | Reference |
|---|---|
| Magic `SKDICT01`/`SKDICT02`, positional length-prefixed sections: words_fst, typos_fst, typo_pool, uk↔us pairs, bigrams, smash entries, (v2+) proper nouns. No lang / version / variants / source metadata. | `lexicon.rs:50-51`, `lexicon.rs:227-373`, `lexicon.rs:1022-1071` |
| `WordMeta { us_freq: u8, gb_freq: u8, flags: u8, word_id: u16 }` packed into u64 (40 bits). Flags `FLAG_IN_US/GB/OFFENSIVE/TECH`. | `lexicon.rs:53-65` |
| `effective_freq` cross-dialect fallback (other slot − 25); `is_valid_for_dialect` checks IN_US/IN_GB or tech/offensive. | `lexicon.rs:97-127` |
| `word_id` u16: top 65,535 words by max freq get ids; bigram key `(prev_id<<16)|cand_id`. | `lexicon.rs:776-787`, `lexicon.rs:457-466` |
| `from_static_bytes` claims zero-copy but **both** branches build `Cow::Owned` (full copy of the FST slices). | `lexicon.rs:343-358` |
| `compile_skdict_from_sources` takes **8 positional English-specific inputs**; `parse_aosp` itself is language-agnostic except the US/GB flag argument. | `lexicon.rs:570-579`, `lexicon.rs:590-683` |
| English-specific build logic baked into the compiler: dev-term freq 172, proper-noun freq 160, variant-map shared freq ≥120, smash filter (ASCII, len 7–16, freq ≥105), 3 curated typo overrides, 15 curated bigram boosts, count_2w top-90k log quantization (80..=255), AOSP rank→score (210/185/160). | `lexicon.rs:689-699`, `:713-744`, `:790-812`, `:846-853`, `:911-933`, `:866-908` |
| `resolve_dict_path(lang)` is generic already (SMARTKEY_DICT_DIR → user dir → /usr/{local/,}share/smartkey/dicts). | `lexicon.rs:530-553` |
| Bundled asset `assets/en_lexicon.skdict` = **5.0 MB**, embedded under default feature `bundled-en`. | `lexicon.rs:212-215` |

### 1.2 Engine is single-language

| Fact | Reference |
|---|---|
| `SmartKeyEngine { scorer: Option<EnglishScorer>, … }` — one scorer, no registry. | `session.rs:16-19` |
| `new_auto` / `reload_dicts` hardcode `resolve_dict_path("en")`. | `session.rs:41,58` |
| vi routing via string compare `lang.as_str() == "vi"` + `vi_lotus_enabled`. | `session.rs:181` |
| `reconcile_surrounding` is gated `lang != "en"` — an English-specific assumption that will silently misbehave for future Tier A langs. | `session.rs:688` |
| CLI engine init also hardcodes `resolve_dict_path("en")`. | `main.rs:44` |
| `dict install` validates **any** pack as an English lexicon (`EnglishLexicon::from_owned_bytes`) — a French pack would install as "en". | `main.rs:1948-1958` |

### 1.3 Hardcoded English language rules (Rust)

| Table | Size | Reference |
|---|---|---|
| Unconditional contractions | ~47 pairs | `scorer.rs:90-137` |
| Transposed apostrophes | 18 pairs | `scorer.rs:150-169` |
| Unmerged compounds | 9 pairs | `scorer.rs:172-182` |
| Function words (missed-space split) | ~57 words | `scorer.rs:185-191` |
| Clitic suffixes | 7 | `scorer.rs:193` |
| Casing abbreviations/exclusions | whole module | `casing.rs` (537 lines) *inherited* |
| Curated typo overrides / bigram boosts | 3 / 15 | `lexicon.rs:846-853`, `:911-933` |

### 1.4 The split-brain bug (shorthand)

- English shorthand **expansions** come from `include_str!("../../../data/shorthand_en.tsv")` compiled into the binary (`lexicon.rs:12`), consulted at `scorer.rs:684`.
- The pack only carries the *trigger shields* (merged into words_fst at build time, `lexicon.rs:713-719`).
- Consequence: an installed pack can protect a trigger the binary cannot expand, or vice versa. A pack update alone can never change expansions.

### 1.5 Vietnamese (Tier B model)

- `vi_telex.rs:3-5` embeds 3 files via `include_str!`: `vietnamese.cm.dict` (7,886 syllables, 48K), `slang_vi.txt`, `shorthand_vi.tsv`. Runtime uses them as syllable/prefix validator + slang set; correction is rule-based Telex rescue (`session.rs:236-423`).
- **CORRECTED (review, observed):** the vendored dict has **not** drifted. A raw line diff shows
  wholesale differences, but those come from ordering only. As normalized sets (`grep -v '^#' | sort -u`) the two files are
  **identical: 0 differing entries**. The 7,886 vs 7,885 line count is a missing trailing newline in the
  installed file (fcitx5-lotus 6.0.7-1). Drift is still possible on a lotus update, so the set-based
  check in §6 stays. This mistake shows why the check must compare sets, not raw bytes.
- vi slang/shorthand lookups happen at **runtime** (`session.rs:262-333`) — vi needs a queryable slang set, unlike en where slang is build-time-merged flags only.

### 1.6 Repos & CI

- `smartkey-dicts` has real pack recipes already: `dicts/en/pack.toml` + `dicts/vi/pack.toml` (metadata + flat source-file list, `format_version = 2`).
- **NEW finding:** `dicts/en/shorthand_extended.tsv` (40 opt-in entries) exists **only** in smartkey-dicts — the mirror is already asymmetric; today the other 5 curated files are still identical.
- CI (`smartkey-dicts/.github/workflows/build-packs.yml:40-51`) copies dicts → `smartkey/data/`, builds **only** `en.skdict`, uploads artifact. vi pack is never built.
- Curated data is duplicated in `smartkey/data/` and `smartkey-dicts/dicts/` (5 files identical, 1 file dicts-only).
- `scripts/fetch_sources.sh` pins OpenBoard `v1.4.5` and crate-ci/typos `v1.29.4`, but **`AB_REF` defaults to `master`** (unpinned) and Norvig `count_2w.txt` has no ref at all.
- **License status:** AOSP lists Apache-2.0, typos MIT/Apache-2.0, British map MIT; **Norvig `count_2w.txt` license unverified** (flagged, not resolved here).

### 1.7 Toolchain facts relevant to the design

- `toml_edit` (with `serde`) is already a workspace dependency → `pack.toml` parsing needs **no new dependency**.
- Zero new external crates needed: SHA-256 verification uses an in-repo FIPS 180-4 implementation (~65 lines of pure safe Rust, keeping `Cargo.lock` unchanged for `--locked`/`--frozen` builds).
- `smartkey-dev` already has `build-lexicon` (calls the 8-arg compiler) and `eval` (golden corpus gate) — `dev.rs`.
- Sizes: en pack ~5.0 MB; raw sources: en_US.combined 15 MB, en_GB.combined 6.6 MB, count_2w 5.4 MB, typos 2.0 MB.

### 1.8 Tier A upstream verified: Helium314/aosp-dictionaries (Codeberg)

Observed via repo fetch: dozens of languages in the **exact `.combined` format `parse_aosp` already reads** (`word=…,f=…,flags=…` + `bigram=` next-word entries), including fr, de (DE/AT/CH), es, pt (BR/PT), it, nl, pl, vi; experimental lists carry next-word data; **licenses vary per language** (Apache-2.0, GPL-2/3, CC0, CC BY 4.0, CC BY-SA, and **CC BY-NC** for some, e.g. Slovak) → per-source license recording and NC/ND warnings are mandatory, not optional.

---

## 2. Decisions (LOCKED 2026-11-16)

| # | Decision | Resolution | Why |
|---|---|---|---|
| D1 | Core packaging | **Embedded v3 `en` (`bundled-en`) + `vi` (`bundled-vi`) fallbacks, default on.** Installed packs override everything, including shorthand. | Works offline on a fresh install; tests and eval need no fetch. |
| D2 | Format compat | **Clean break. No v1/v2 reader.** On load, an installed v1/v2 (or invalid) pack is **ignored**: the engine falls back to the bundled v3 pack and logs `DiagnosticEvent::PackNotice` (`legacy_pack_ignored` / `invalid_pack_ignored`). `dict list` shows `[legacy v2 — ignored; run: smartkey dict update <lang>]`. | D1 makes fallback free; keeping a v2 reader would retain a frozen shorthand table and perpetuate split-brain. |
| D3 | Curated data home | **Monorepo `dicts/<lang>/`.** Move `smartkey-dicts/dicts/` + `spec/` into `smartkey/dicts/` without touching `smartkey-dicts` (frozen at v2). Delete duplicate curated files in `smartkey/data/`, keeping `data/sources/` as the gitignored fetch cache. Publish v3 packs on `segfawlt/smartkey` releases and point `dict update` there. | Format, builder, rules schema, corpus gates, and bundled packs evolve atomically in one commit and one CI. |
| D4 | Tier A proof & routing | **`es` primary + `fr` secondary**, plus Fcitx5 `inputMethodEntry(ic)->languageCode()` routing via `resolveDirectKeyboardLang` (`keyboard-vn*` excluded from Lotus Telex). `clitics.prefixes` handles French elisions; `capitalize_standalone_i` gates `i -> I` to English only. | `es` verifies accents/ñ cleanly with zero Rust edits; `fr` proves prefix elision cleanly; `languageCode()` enables multi-language Fcitx5 keyboards without hardcoding `"en"`. |
| D5 | Zero new external crates | Implement FIPS 180-4 SHA-256 directly in `smartkey-core::pack_builder`; keep `Cargo.lock` external crate graph unchanged (`curl` + `gzip -dc` for `--fetch`). | `--locked`/`--frozen` builds (`install-local.sh`, `PKGBUILD`) stay self-contained. |
| D6 | Proper-noun homographs | **F1 + F2 + F3**: remove `polish` and `latin` from `dicts/en/proper_nouns.tsv`; keep AOSP `lint_proper_nouns` (`gap >= 60`); add `FLAG_AMBIGUOUS_PROPER = 0x04` skipped in Step 4b. | `count_2w.txt` has lowercase web-query bias on `texas`/`paris`/`asian`; AOSP case frequency gap separates true homographs cleanly. |
| D7 | Vietnamese Tier B | Bundle `vi_lexicon.skdict` (~55 KB) via `bundled-vi`; remove all 3 `include_str!`s in `vi_telex.rs`; runtime reads installed Lotus `vietnamese.cm.dict` (`SMARTKEY_LOTUS_DICT` or system/user paths) as syllable authority with normalized-set `lotus_dict_drift` notice. | Runtime tracks live `fcitx5-lotus` updates automatically while preserving offline fallback. |

### 2.1 Additional invariants from review

1. **vi runtime authority:** when the installed Lotus dict is present and non-empty, **it is the authority**, because Lotus uses it. The pack's `SYLLABLE_DICT` is the fallback. Real set divergence is logged once per load (`DiagnosticEvent::PackNotice { notice: "lotus_dict_drift", .. }`), never per keystroke and never on line-order / trailing-newline differences.
2. **Collapse the rule tables.** Contractions, transposed apostrophes, unmerged compounds, and typo overrides collapse into one `replacements = [{ typed, fix, kind }]` table in `dicts/<lang>/rules.toml`. Keep `kind` so `MatchKind` and the learner's 2x-evidence rule for contractions still work. Keep `capitalize_standalone_i`, `function_words`, `clitics { suffixes, prefixes }`, `casing { leading_apostrophes, abbreviations, exclusions }`, and `bigram_boosts`.
3. **Single source of truth for English rules.** Once `dicts/en/rules.toml` exists, delete the duplicated Rust rule consts (`UNCONDITIONAL_CONTRACTIONS`, `TRANSPOSED_APOSTROPHES`, `UNMERGED_COMPOUNDS`, `FUNCTION_WORDS`, `CLITIC_SUFFIXES`, `curated_typo_overrides`, `curated_boosts`). Tests use the bundled pack or explicit rule builders.
4. **Zero-copy static load:** `PackLexicon::from_static_bytes` slices `WORDS_FST`, `TYPOS_FST`, and `typo_pool` as `Cow::Borrowed`.
5. **Stronger Phase 1 gate:** before overwriting `en_lexicon.skdict`, dump every `(word, freq0, freq1, variant_mask, flags_remapped, word_id)`, every typo mapping, every UK↔US pair, every bigram, every smash entry, and every proper noun from v2 vs v3 and assert an **exact** match (0 differences).
6. **Staged renames:** keep `EnglishLexicon` / `EnglishScorer` names in Phases 1–3; generalize to `PackLexicon` / `LatinScorer` (with deprecated type aliases) in Phase 4 once `es` works.
7. **Norvig `count_2w.txt` stays `UNVERIFIED`:** mark `license = "UNVERIFIED"` in `dicts/en/pack.toml` (loud build warning + recorded in `SOURCE_MANIFEST`) and run a Phase 4 `--without-count2w` eval ablation to measure impact.
8. **Clean baseline:** prior dirty changes in `casing.rs`, `event_log.rs`, `session.rs`, `vi_telex.rs` were already committed (`9ab2ef5`..`cae2c7e`), and `from_static_bytes` zero-copy (`Cow::Borrowed`) is already landed for v2 and preserved in v3.

---

## 3. Design overview — two tiers

- **Tier A (data-only, Latin-script directly-typed):** es, fr, pt, it, nl, pl, etc. (`tier = "data"`). Pack recipe + generic builder (`smartkey-dev build-pack`) → generic `LatinScorer` (with per-pack `LANGUAGE_RULES`, `capitalize_standalone_i`, and `clitics.prefixes`/`suffixes`). **Zero Rust edits** to add a language.
- **Tier B (IME-coupled):** vi today (`fcitx5-lotus` bridge, `tier = "ime_bridge"`), CJK later. A small Rust module provides IME-specific correction logic (Telex rescue / composition simulation); the **pack supplies** shorthand, slang shields, and fallback syllable dictionary (`vi_lexicon.skdict`). The Lotus syllable validator reads at **runtime** from the installed IME dict (`SMARTKEY_LOTUS_DICT` or `/usr/{local/,}share/fcitx5/lotus/vietnamese.cm.dict`), using the pack's `SYLLABLE_DICT` as fallback and logging `lotus_dict_drift` once per load/reload if the normalized sets diverge.

Non-goals for this RFC: keyboard layout as data (AZERTY/QWERTZ), Unicode-aware smash masks, accent-folding candidates, CJK modules, the dead `telex_passthrough` path (lotus audit P1 — separate fix), user `rules.toml` format changes, FFI ABI breaking changes.

---

## 4. `.skdict` v3 specification

### 4.1 Container

```
offset  size  field
0       8     magic  "SKDICT03"
8       2     header_len      u16 LE — byte length of the header block below
-- header block (self-describing, then TLV sections follow) --
+0      1     lang_len        u8, then lang code ASCII lowercase ("en", "vi", "es", "fr", …)
+…      3     pack_version    semver (major, minor, patch) as u8 ×3
+…      1     tier            u8 (0 = Data, 1 = ImeBridge)
+…      1     variant_count   V, 1..=8
+…      V ×   variant tag     u8 len + ASCII ("en_US", "pt_BR", …); index = bit position
+…      1     freq_slot_count S, 0..=2
+…      S ×   u8              bitmask of variant indices sharing that frequency slot
+…      1     default_variant_idx u8
+…      2     feature_flags   u16 LE, reserved 0
-- sections (repeat until EOF) --
2       tag   u16 LE
4       len   u32 LE
len     payload
```

**Forward compatibility rule:** readers MUST skip unknown section tags (length-delimited) and MUST fail cleanly on malformed headers or non-v3 magics (`SKDICT01`/`SKDICT02` return `LexiconError::LegacyFormat(1|2)`).

### 4.2 Section tags

| Tag | Name | Payload |
|---|---|---|
| 0x0001 | `WORDS_FST` | fst `Map`, values = `WordMetaV3` packed u64 (§4.3) |
| 0x0002 | `KNOWN_TYPOS` | `u32 fst_len` + typo fst + pool (fst values: `offset \| len<<32` into pool) |
| 0x0003 | `VARIANT_REWRITES` | `u8 k`, then k × { `target_variant_idx` u8, `u32 pair_count`, pairs of len-prefixed `(from_other, to_target)` } |
| 0x0004 | `BIGRAMS` | `u32 count` + count × (`u32 key` LE + `u8 score`) — key = `(word_id<<16)\|word_id`, unchanged |
| 0x0005 | `SMASH_INDEX` | `u32 count` + count × (`u32 mask`, `u8 len`, `u8 freq`, len-prefixed word) — ASCII-only in v3.0 |
| 0x0006 | `PROPER_NOUNS` | `u32 count` + pairs of len-prefixed `(lower, Capitalized)` |
| 0x0007 | `SHORTHAND` | `u32 count` + pairs of len-prefixed `(trigger, expansion)`, sorted+deduped by trigger |
| 0x0008 | `SLANG_SET` | `u32 count` + len-prefixed lowercase words — runtime-queryable shield set (`vi` uses this; `en`/`es`/`fr` merge slang into `FLAG_SHIELD` at build time) |
| 0x0009 | `LANGUAGE_RULES` | `u8 rules_version = 1` + UTF-8 JSON: `capitalize_standalone_i`, `replacements [{ typed, fix, kind }]`, `function_words`, `clitics { suffixes, prefixes }`, `bigram_boosts`, `casing { leading_apostrophes, abbreviations, exclusions }` |
| 0x000A | `SYLLABLE_DICT` | `u32 count` + sorted, deduped syllables (`vi`; normalized as a set so upstream reordering ≠ drift) |
| 0x000B | `SOURCE_MANIFEST` | `u32 count` + count × { `u32`-len-prefixed name, `[u8; 32]` sha256, `u32`-len-prefixed license string } |

All sections are **optional**. A Tier A pack without bigram or typo sources omits `0x0004`/`0x0002`; the `vi` pack omits `WORDS_FST`-scale sections entirely.

### 4.3 `WordMeta` v3 (packed u64, 48 bits used)

```
bits  0..8   freq0         u8   frequency within freq-slot-0 variants
bits  8..16  freq1         u8   frequency within freq-slot-1 variants
bits 16..24  variant_mask  u8   bit i = valid in variant i (up to 8 variants)
bits 24..32  flags         u8   0x01 offensive · 0x02 shield (tech/slang/dev) ·
                                0x04 ambiguous_proper (findings F3) · 0xF8 reserved (0)
bits 32..48  word_id       u16
bits 48..64  reserved, must be 0 (validated on load)
```

**Why frequency slots:** two `u8` slots + header slot bitmasks reproduce today's English semantics (`freq0 ≡ us_freq`, `freq1 ≡ gb_freq`) while generalizing (`de` = slot0 `{de_DE, de_AT}`, slot1 `{de_CH}`; `pt` = slot0 `{pt_BR}`, slot1 `{pt_PT}`; `es`/`fr` = slot0 `{es_ES}`/`{fr_FR}`). `effective_freq` keeps the −25 cross-slot fallback and `max()` in variant-agnostic (`Mixed`) mode.

**Variants vs `Dialect`:** the user-facing `rules.toml` key `dialect` stays valid for `en` (back-compat). Internal lookups accept variant awareness without breaking public API/FFI types.

### 4.4 Clean break on v1/v2 (per D2)

No v1/v2 parser is included. `parse_sections` / `peek_skdict_header` identify `SKDICT01` / `SKDICT02` and return `LexiconError::LegacyFormat(1|2)`. `LexiconRegistry` ignores legacy/corrupt files on disk, falls back to the bundled v3 pack when present, and emits a one-time `DiagnosticEvent::PackNotice` (`legacy_pack_ignored` / `invalid_pack_ignored`). `smartkey dict list` displays `[legacy v2 — ignored; run: smartkey dict update <lang>]`.

---

## 5. `pack.toml` schema

Lives at `dicts/<lang>/pack.toml` in the `smartkey` monorepo:

```toml
[pack]
code = "en"                       # file becomes en.skdict; must match header lang
name = "English"
version = "0.3.0"                 # pack semver
format_version = 3
tier = "data"                     # data | ime_bridge
maintainer = "segfawlt"
description = "…"
license = "GPL-3.0-or-later"      # license of the pack compilation itself

[variants]
en_US = { freq_slot = 0, default = true }
en_GB = { freq_slot = 1 }

[sources.en_us]                   # every source: url + sha256 + license, build FAILS if missing
format = "aosp_combined"          # aosp_combined | typos_csv | bigram_counts | variant_map_json | wordlist | fcitx_cm_dict
url = "https://raw.githubusercontent.com/openboard-team/openboard/v1.4.5/dictionaries/en_wordlist.combined.gz"
decompress = "gzip"               # none | gzip
sha256 = "<sha256 of the decoded content the parser ingests>"
license = "Apache-2.0"
variants = ["en_US"]
capture_bigrams = true

[sources.en_gb]                   # … variants = ["en_GB"], capture_bigrams = false
[sources.typos]                   # format = "typos_csv", license = "MIT OR Apache-2.0"
[sources.variant_map]             # format = "variant_map_json"
[sources.bigram_counts]           # format = "bigram_counts", license = "UNVERIFIED" (warning, never silent)

[overlays]                        # curated files, relative to dicts/<lang>/, merged in order
shorthand    = ["shorthand.tsv"]
slang        = ["slang.txt"]
proper_nouns = ["proper_nouns.tsv"]
dev_terms    = ["dev_terms.txt"]
rules        = ["rules.toml"]     # curated rule tables → LANGUAGE_RULES section

[build]                           # optional tuning; sane defaults per format
bigram_top_n = 90000
smash_min_freq = 105
proper_noun_freq = 160
tech_freq = 172
```

`vi` example (Tier B):

```toml
[pack]
code = "vi"
name = "Vietnamese"
version = "0.2.0"
format_version = 3
tier = "ime_bridge"
maintainer = "segfawlt"
license = "GPL-3.0-or-later"

[variants]
vi = { freq_slot = 0, default = true }

[sources.lotus_dict]
format = "fcitx_cm_dict"
path = "vietnamese.cm.dict"       # or pinned url + sha256
sha256 = "<sha256>"
license = "GPL-3.0-or-later"

[overlays]
shorthand = ["shorthand.tsv"]
slang = ["slang.txt"]

[build]
syllable_dict = true
```

English rule tables move from Rust consts (`scorer.rs`, `lexicon.rs`, `casing.rs`) into `dicts/en/rules.toml`, with the Rust consts deleted once `dicts/en/rules.toml` is wired in.

---

## 6. Runtime changes

| Today | Becomes |
|---|---|
| `EnglishLexicon` (en-shaped) | `PackLexicon` (generic; `pub type EnglishLexicon = PackLexicon` kept as compat alias in Phase 4). Parses v3 only (`SKDICT03`). `from_static_bytes` slices `WORDS_FST`, `TYPOS_FST`, `typo_pool` via `Cow::Borrowed`. |
| `EnglishScorer` (en rules hardcoded) | `LatinScorer` implementing `LanguageScorer` (`pub type EnglishScorer = LatinScorer` alias); contractions, apostrophe slips, compounds, typo overrides, function words, clitics (`prefixes` + `suffixes`), `capitalize_standalone_i`, and casing tables read from `LANGUAGE_RULES`. |
| `Option<EnglishScorer>` in engine | `LexiconRegistry` inside `SmartKeyEngine` (`HashMap<LangId, LoadedPack>`). `scorer()` / `lexicon()` return `"en"` pack for back-compat + Lotus momentum shield; `scorer_for(lang)` / `lexicon_for(lang)` resolve any loaded language. |
| `resolve_dict_path("en")` (`session.rs`, `main.rs`) | Registry loads installed packs (user/system/`SMARTKEY_DICT_DIR`), ignoring non-v3 files with `PackNotice` and falling back to `bundled-en` / `bundled-vi`. |
| Shorthand via `include_str!` (`lexicon.rs:12`) | `PackLexicon::lookup_shorthand` over `SHORTHAND` section. Split-brain eliminated. |
| `vi` data via 3 × `include_str!` (`vi_telex.rs:3-5`) | Bundled `vi_lexicon.skdict` (`bundled-vi`, default on, ~55 KB) + runtime read of `resolve_lotus_dict_path()` (`SMARTKEY_LOTUS_DICT` → `/usr/{local/,}share/fcitx5/lotus/vietnamese.cm.dict` → `~/.local/share/fcitx5/lotus/vietnamese.cm.dict`). Normalized-set diff once on load/reload emits `lotus_dict_drift` only when sets differ. |
| Fcitx5 addon hardcoded `"en"` on `keyboard-*` | `resolveDirectKeyboardLang(ic)` inspects `instance_->inputMethodEntry(ic)->languageCode()`, extracts lowercase primary subtag, skips empty/`"vi"`, and passes the active language (`"en"`, `"es"`, `"fr"`, …) to `smartkey_session_set_lang`. |
| `session.rs` `!= "en"` / `== "en"` gates | Capability/tier + `scorer_for(lang)` gates (`tier == Data` enables surrounding-text reconcile + `RealWordDeclined` + lexicon-backed learning). |
| `dict install` / `list` / `update` | Validates `header.lang == requested_lang`; `dict list [-v]` inspects headers (flagging legacy v1/v2 as ignored) and displays `SOURCE_MANIFEST`; `dict update` points to `segfawlt/smartkey` releases. |

The learner's `rules.toml` format, JSONL event log layout, session state machine, and `smartkey.h` C ABI stay backwards-compatible.

---

## 7. Generic builder: `smartkey-dev build-pack <lang|all> [options]`

Options: `--dicts-dir <path>` (defaults to `<repo>/dicts`), `--out <path>`, `--fetch`, `--verify`.

Pipeline:
1. **Parse** `dicts/<lang>/pack.toml` (`toml_edit` — no new dep).
2. **Fetch/verify** each source (via `curl` + optional `gzip -dc` when `--fetch` is set; cache under `data/sources/<lang>/`), verify **SHA-256** using pure-Rust FIPS 180-4 (`pack_builder::sha256_hex` — zero new external crates), fail hard on mismatch or missing `license`.
3. **Parse** per `format` handler: `aosp_combined`, `typos_csv`, `bigram_counts`, `variant_map_json`, `wordlist`, `fcitx_cm_dict`.
4. **Merge overlays**: `dev_terms`, `slang`, `proper_nouns`, `shorthand`, `rules` (`rules.toml` → `LanguageRules`).
5. **Lint:**
   - Proper-noun homograph lint (`lint_proper_nouns`, AOSP frequency gap `>= 60`, **F2**); `FLAG_AMBIGUOUS_PROPER` runtime safety bit (**F3**).
   - Duplicate/invalid shorthand checks.
   - License gate: missing → hard fail; `UNVERIFIED` / NC / ND → warning + recorded in `SOURCE_MANIFEST`.
6. **Serialize** v3 deterministically (`BTreeMap` ordering).
7. **Verify** roundtrip load and optional corpus eval (`--verify`).

`build-lexicon` rebuilds the bundled assets (`en_lexicon.skdict` and `vi_lexicon.skdict`).

---

## 8. Migration path

| Component | Today | After |
|---|---|---|
| Compiler | `compile_skdict_from_sources(8 args)` | `PackBuilder` driven by `dicts/<lang>/pack.toml` |
| Shorthand | `include_str!` binary-side, shields pack-side | `SHORTHAND` pack section, single source |
| En rule tables | Rust consts (`scorer.rs`, `lexicon.rs`, `casing.rs`) | `dicts/en/rules.toml` → `LANGUAGE_RULES` (consts deleted) |
| Engine | `Option<EnglishScorer>` | `LexiconRegistry` |
| Fcitx5 IM lang | Hardcoded `"en"` on `keyboard-*` | `inputMethodEntry(ic)->languageCode()` via `resolveDirectKeyboardLang` |
| Dict install | Any pack validates as `en` | Header `lang` validation + `segfawlt/smartkey` releases |
| Curated data | Duplicate `smartkey/data` + `smartkey-dicts` | Monorepo `smartkey/dicts/` only (`smartkey-dicts` frozen at v2) |
| `vi` data | 3 × `include_str!` | `vi_lexicon.skdict` + runtime Lotus dict authority + drift warning |

---

## 9. Phase plan (each phase independently green, conventional commit, staged files only)

0. **`docs(rfc): finalize multilang dict pack v3 specification and decisions`**
   Propagate locked D1–D7 decisions cleanly across `docs/rfc-multilang-dict-packs.md`.
1. **`feat(core): skdict v3 TLV format and WordMetaV3`**
   v3 header/TLV parse & serialize; clean-break `LegacyFormat` error on v1/v2; zero-copy `Cow::Borrowed` static load; exact v2-vs-v3 6-table dump diff gate + `en_lexicon.skdict` rebuilt as v3.
2. **`feat(core): shorthand, slang, and language rules in v3 packs`**
   Migrate curated data into `dicts/{en,vi,spec}/`; create `dicts/en/rules.toml` (collapsed `replacements` + `capitalize_standalone_i` + `clitics` + `function_words` + `casing` + `bigram_boosts`); remove `include_str!` and duplicated Rust consts; F1 (`polish`, `latin` removed) + F3 (`FLAG_AMBIGUOUS_PROPER`).
3. **`feat(engine,fcitx5,cli): LexiconRegistry, language routing, and header-aware dict CLI`**
   `LexiconRegistry` + `PackNotice` on ignored legacy packs; Fcitx5 `resolveDirectKeyboardLang` (`inputMethodEntry->languageCode()`); `smartkey dict list/install/update` header checks + `explain --lang`.
4. **`feat(cli,dicts): generic build-pack, PackLexicon/LatinScorer, and es + fr proof packs`**
   `pack_builder` with FIPS 180-4 SHA-256 (0 new external crates); `PackLexicon` / `LatinScorer` renames + `clitics.prefixes` French elision; `dicts/en/pack.toml` (`count_2w` `--without-count2w` ablation); `dicts/es/` + `dicts/fr/` packs & corpus gates.
5. **`feat(vi): vi_lexicon.skdict pack and runtime Lotus dictionary authority`**
   Build `vi_lexicon.skdict` (`bundled-vi`); remove all 3 `include_str!`s from `vi_telex.rs`; runtime `resolve_lotus_dict_path()` with normalized-set `lotus_dict_drift` notice; remove legacy curated files from `data/` and update install/packaging scripts.
6. **`docs: align README, AGENTS, and RFC with v3 multi-language packs`**
   Update docs, run full verification (`cargo test`, `clippy`, `eval` en/es/fr, `check-versions.sh`), commit, and run `install-local.sh` + live smoke checks.

---

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| En behavior drift on v2→v3 rebuild | Phase 1 exact 6-table dump diff + A/B `smartkey-dev eval` (`95/95`, `141/141`) |
| Upstream drift/unavailability | URL + tag + SHA-256 pinned in `pack.toml`, enforced in `build-pack` |
| License landmines (`count_2w` unverified; NC lists) | Per-source license mandatory; `UNVERIFIED` / NC / ND recorded in `SOURCE_MANIFEST` with warnings |
| French elision false positives (`l'homme`, `qu'il`) | `clitics.prefixes` in `LANGUAGE_RULES` strips known elision prefix for valid-stem shield |
| `keyboard-vn*` accidentally activating Lotus Telex | `resolveDirectKeyboardLang` excludes `languageCode == "vi"` on `keyboard-*` |
| `vi` false-positive drift alarms | Compare normalized deduplicated syllable **sets**, never raw file bytes |
| Lockfile drift on `--locked`/`--frozen` builds | Zero new external crates (pure-Rust SHA-256, `curl` + `gzip`) |

---

## Appendix A — `en` v2 → v3 field equivalence

| v2 | v3 | Note |
|---|---|---|
| `us_freq` | `freq0`, slot0 = `{en_US}` | identical semantics incl. −25 fallback |
| `gb_freq` | `freq1`, slot1 = `{en_GB}` | identical |
| `FLAG_IN_US/GB` | `variant_mask` bits 0/1 | identical validity logic |
| `FLAG_OFFENSIVE` (0x04) | `flags` bit 0x01 | value remap only |
| `FLAG_TECH` (0x08) | `flags` bit 0x02 (shield) | value remap only |
| (none) | `flags` bit 0x04 (`ambiguous_proper`) | F3 proper-noun guard |
| `word_id: u16` | `word_id: u16` | unchanged, same ranking rule |
| `uk_to_us` / `us_to_uk` | `VARIANT_REWRITES` for `en_US` / `en_GB` | `Mixed` mode uses default-variant (`en_US`) rewrites |
| positional sections | TLV tags | clean break: v3 only (`LegacyFormat` on v1/v2) |
| (none) | `SHORTHAND`, `SLANG_SET`, `LANGUAGE_RULES`, `SYLLABLE_DICT`, `SOURCE_MANIFEST` | modular pack data |
