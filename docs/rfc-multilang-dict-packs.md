# RFC: Multi-Language Dictionary Packs (`.skdict` v3 + generic builder)

**Status:** APPROVED WITH CHANGES (2026-11-12). Decisions resolved in §2 — §2 overrides any
conflicting text in §3–§10 (fr→es, v2 reader removed, monorepo). Propagate before Phase 1.
**Date:** 2026-11-12
**Scope:** `smartkey` (core/CLI/FFI) + `smartkey-dicts` (recipes, CI)
**Goal:** A contributor adds a language by writing `dicts/<lang>/pack.toml` (pinned upstream
sources + sha256 + license) plus small curated overlay files. The generic build tool emits a
valid pack. Adding a Tier A language requires **zero Rust edits**.

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
- No sha256 crate in the workspace → `sha2` is the **only** proposed new dependency.
- `smartkey-dev` already has `build-lexicon` (calls the 8-arg compiler) and `eval` (golden corpus gate) — `dev.rs`.
- Sizes: en pack 5.0 MB; raw sources: en_US.combined 15 MB, en_GB.combined 6.6 MB, count_2w 5.4 MB, typos 2.0 MB.

### 1.8 Tier A upstream verified: Helium314/aosp-dictionaries (Codeberg)

Observed via repo fetch: dozens of languages in the **exact `.combined` format `parse_aosp` already reads** (`word=…,f=…,flags=…` + `bigram=` next-word entries), including fr, de (DE/AT/CH), es, pt (BR/PT), it, nl, pl, vi; experimental lists carry next-word data; **licenses vary per language** (Apache-2.0, GPL-2/3, CC0, CC BY 4.0, CC BY-SA, and **CC BY-NC** for some, e.g. Slovak) → per-source license recording and NC/ND warnings are mandatory, not optional.

---

## 2. Decisions (RESOLVED 2026-11-12 — these override §3–§10)

| # | Decision | Resolution | Why |
|---|---|---|---|
| D1 | Core packaging | **ACCEPTED as proposed.** Embedded v3 en fallback + bundled vi pack (`bundled-vi`, default on). Installed packs override everything, including shorthand. | Works offline on a fresh install; tests and eval need no fetch. |
| D2 | Format compat | **OVERRIDDEN → clean break. No v2 reader.** On load, an installed v2 (or otherwise invalid) pack is **ignored**: the engine falls back to the bundled v3 pack and logs `legacy_pack_ignored`. `dict list` shows `legacy v2 — ignored, run: smartkey dict update <lang>`. | D1 makes the fallback free, so users lose nothing. A v2 reader would keep a frozen shorthand table alive just for v2 packs, which is the split-brain this RFC removes. The project is a pre-1.0 beta, and the only v2 packs in the wild are the maintainer's. |
| D3 | Curated data home | **OVERRIDDEN → monorepo.** Move `smartkey-dicts/dicts/` and `spec/` into `smartkey/dicts/` (single source). Delete the curated files in `smartkey/data/`, keep `data/sources/` as a gitignored fetch cache. Publish v3 packs as release assets of `segfawlt/smartkey`, and point `dict update` (`main.rs:2177`) there. **Freeze `smartkey-dicts` at its last v2 release** (the maintainer archives it; agents never do), so old binaries' `dict update` keeps getting v2 packs they can read. | The pack format, builder, `rules.toml` schema, corpus gates and the bundled pack all change together. Today the dicts CI checks out *unpinned* smartkey `main`, so any format change breaks it. A monorepo gives one atomic commit per change and one CI. Contributors still edit a single text file. Third parties can still build their own packs with `build-pack --dicts-dir`. |
| D4 | Tier A proof language | **OVERRIDDEN → es first, fr second.** | es has accents/ñ (exercises Unicode and confirms the ASCII-only smash index omits words rather than breaking) with no elision and little compounding, so the "zero Rust edits" claim is tested cleanly. fr elision (`l'homme`, `qu'il`, `j'ai`) goes through the English apostrophe/contraction paths and would pull Rust work into the proof. Do fr next, with elision documented as a known Tier A gap. de needs a compound-noun false-positive design first and is out of scope. |

### 2.1 Additional requirements from review

1. **vi runtime authority:** when the installed lotus dict is present and parses, **it is the authority**, because lotus uses it. The pack's SYLLABLE_DICT is the fallback. Real set divergence is logged once per load (`info`), never per keystroke. Make the lotus dict path configurable, resolved next to `resolve_dict_path`.
2. **Collapse the rule tables.** Contractions, transposed apostrophes, unmerged compounds and typo overrides are all unconditional typed→fix maps, so merge them into one `replacements = [{ typed, fix, kind }]` table. Keep `kind` so `MatchKind` and the learner's 2x-evidence rule for contractions still work. Keep `function_words`, `clitics { suffixes, prefixes }` (prefixes reserved for fr elision, unused for now), `casing { abbreviations, exclusions }` and `bigram_boosts`.
3. **Keep only one copy of the en rules.** Once `dicts/en/rules.toml` exists, delete the Rust consts. They must not stay as defaults, or there will be two copies again. Tests use the bundled pack or an explicit test fixture.
4. **`sha2` goes only in `smartkey-cli`** (dev builder), never in `smartkey-core`.
5. **Stronger Phase 1 gate:** besides the corpus eval A/B, dump every `(word, meta)`, bigram, typo, variant pair and proper noun from the v2 and v3 images and require an **exact** diff (after remapping flags).
6. **Defer the `EnglishScorer → LatinScorer` / `EnglishLexicon → PackLexicon` renames** to Phase 4, once es works. That keeps Phase 1 about the format and avoids alias churn.
7. **Norvig `count_2w.txt` stays `UNVERIFIED`.** In Phase 4, also run the en eval *without* it and record how much it matters, so it can be dropped if the license check fails.
8. **Dirty tree before Phase 1:** `casing.rs`, `event_log.rs`, `session.rs` and `vi_telex.rs` have uncommitted changes (~92+/104−) that this RFC didn't make. Their owner must commit or resolve them first. Never sweep them into an RFC commit.

---

## 3. Design overview — two tiers

- **Tier A (data-only, Latin-script directly-typed):** fr, de, es, pt, it, nl, pl. Pack recipe + generic builder → generic `LatinScorer` (the current EnglishScorer algorithm, with rule tables from the pack). **Zero Rust edits** to add a language.
- **Tier B (IME-coupled):** vi today (fcitx5-lotus bridge), CJK later. A small Rust module provides the IME-specific correction logic (Telex rescue / composition simulation); the **pack supplies** shorthand, slang shields, and the syllable dictionary. The lotus syllable validator is read at **runtime** from the IME's installed dict, with the pack's embedded copy as fallback and a **drift warning** when the two differ.

Non-goals for this RFC: keyboard layout as data (AZERTY/QWERTZ), Unicode-aware smash masks, accent-folding candidates, CJK modules, the dead `telex_passthrough` path (lotus audit P1 — separate fix), rules.toml format changes, FFI ABI changes.

---

## 4. `.skdict` v3 specification

### 4.1 Container

```
offset  size  field
0       8     magic  "SKDICT03"
8       2     header_len      u16 LE — byte length of the header block below
-- header block (self-describing, then TLV sections follow) --
+0      1     lang_len        u8, then lang code ASCII lowercase ("en", "vi", "fr", …)
+…      3     pack_version    semver (major, minor, patch) as u8 ×3
+…      1     variant_count   V, 1..=8
+…      V ×   variant tag     u8 len + ASCII ("en_US", "pt_BR", …); index = bit position
+…      1     freq_slot_count S, 0..=2
+…      S ×   u8              bitmask of variant indices sharing that frequency slot
+…      2     feature_flags   u16, reserved 0
-- sections (repeat until EOF) --
2       tag   u16 LE
4       len   u32 LE
len     payload
```

**Forward compatibility rule:** readers MUST skip unknown section tags (length-delimited) and MUST fail cleanly on a malformed header. Newer packs on older runtimes degrade by omission, never by crash.

### 4.2 Section tags

| Tag | Name | Payload |
|---|---|---|
| 0x0001 | `WORDS_FST` | fst `Map`, values = `WordMetaV3` packed u64 (§4.3) |
| 0x0002 | `KNOWN_TYPOS` | `u32 fst_len` + typo fst + pool (fst values: `offset | len<<32` into pool) — same encoding as v2, wrapped in TLV |
| 0x0003 | `VARIANT_REWRITES` | `u8 k`, then k × { variant_idx u8, `u32 pair_count`, pairs of len-prefixed `(other_variant_spelling, this_variant_spelling)` } |
| 0x0004 | `BIGRAMS` | `u32 count` + count × (`u32 key` LE + `u8 score`) — key = `(word_id<<16)|word_id`, unchanged |
| 0x0005 | `SMASH_INDEX` | `u32 count` + count × (`u32 mask`, `u8 len`, `u8 freq`, len-prefixed word) — ASCII-only in v3.0 |
| 0x0006 | `PROPER_NOUNS` | `u32 count` + pairs of len-prefixed `(lower, Capitalized)` |
| 0x0007 | `SHORTHAND` | `u32 count` + pairs of len-prefixed `(trigger, expansion)`, sorted+deduped by trigger |
| 0x0008 | `SLANG_SET` | `u32 count` + len-prefixed lowercase words — runtime-queryable shield set (vi today; en merges slang into flags at build time and omits this section) |
| 0x0009 | `LANGUAGE_RULES` | `u8 rules_version = 1` + UTF-8 JSON: contractions, transposed_apostrophes, unmerged_compounds, function_words, clitic_suffixes, typo_overrides, bigram_boosts, casing { abbreviations, exclusions } |
| 0x000A | `SYLLABLE_DICT` | `u32 count` + sorted, deduped syllables (vi; built from the upstream cm.dict, normalized as a set so upstream reordering ≠ drift) |
| 0x000B | `SOURCE_MANIFEST` | `u32 count` + count × { name, 32-byte sha256, license string } — provenance, shown by `smartkey dict list -v` |

All sections are **optional**. A Tier A pack without bigram or typo sources simply omits 0x0004/0x0002; the vi pack omits WORDS_FST-scale sections entirely.

### 4.3 `WordMetaV3` (packed u64, 48 bits used)

```
bits  0..8   freq[0]  u8   frequency within freq-slot-0 variants
bits  8..16  freq[1]  u8   frequency within freq-slot-1 variants
bits 16..24  variant_mask  u8   bit i = valid in variant i (up to 8 variants)
bits 24..32  flags         u8   0x01 offensive · 0x02 shield (tech/slang/dev) ·
                                0x04 ambiguous_proper (findings F3) · 0xF8 reserved
bits 32..48  word_id       u16
bits 48..64  reserved, must be 0 (validated on load)
```

**Why frequency slots:** two u8 slots + a header mapping (which variants share which slot) exactly reproduces today's en semantics — `freq[0]`≡`us_freq`, `freq[1]`≡`gb_freq` — while generalizing: `de` = slot0 {de_DE, de_AT}, slot1 {de_CH} (ß/ss); `pt` = slot0 {pt_BR}, slot1 {pt_PT}. `effective_freq` keeps the −25 cross-slot fallback and the max() behavior in variant-agnostic ("Mixed") mode. **English behavior is bit-for-bit isomorphic** (appendix A).

**Variants vs `Dialect`:** the user-facing `rules.toml` key `dialect` stays valid for en (back-compat); internally it maps to a variant index. New generic key: `[lang.<code>] variant = "en_GB"`. `VariantSelection` replaces `Dialect` in internal APIs; the public enum stays as a deprecated alias.

### 4.4 v2 read-compat (per D2)

The v2 parser is retained, mapped onto the same `PackLexicon` struct with synthesized header data (`lang="en"`, variants `[en_US, en_GB]`, slot map {0:{0}, 1:{1}}). v2 packs have no SHORTHAND/LANGUAGE_RULES sections → shorthand expansion falls back to the built-in (frozen) table and rules to the built-in consts. `smartkey dict list` flags v2 packs `legacy — update available`. Deletion planned after one release cycle of v3-only CI packs.

---

## 5. `pack.toml` schema

Existing `[pack]` keys are kept and extended; `[sources]` becomes structured; curated local files move to `[overlays]`.

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
format = "aosp_combined"          # aosp_combined | typos_csv | bigram_counts | variant_map_json | fcitx_cm_dict
url = "https://raw.githubusercontent.com/openboard-team/openboard/v1.4.5/dictionaries/en_wordlist.combined.gz"
decompress = "gzip"
sha256 = "<sha256 of the decoded content the parser ingests>"
license = "Apache-2.0"
variants = ["en_US"]              # which variant bucket this source feeds
capture_bigrams = true

[sources.en_gb]                   # … variants = ["en_GB"], capture_bigrams = false
[sources.typos]                   # format = "typos_csv", license = "MIT OR Apache-2.0"
[sources.variant_map]             # format = "variant_map_json" (british_to_american.json)
[sources.bigram_counts]           # format = "bigram_counts", license = "UNVERIFIED"  (warning, never silent)

[overlays]                        # curated files, relative to dicts/<lang>/, merged in order
shorthand   = ["shorthand.tsv"]
slang       = ["slang.txt"]
proper_nouns = ["proper_nouns.tsv"]
dev_terms   = ["dev_terms.txt"]
rules       = ["rules.toml"]      # curated rule tables → LANGUAGE_RULES section

[build]                           # optional tuning; sane defaults per format
bigram_top_n = 90000
smash_min_freq = 105
proper_noun_freq = 160
tech_freq = 172
```

vi example (Tier B, overlays-only — no upstream wordlist):

```toml
[pack]
code = "vi"; tier = "ime_bridge"; version = "0.1.0"; format_version = 3; …

[variants]
vi = { freq_slot = 0, default = true }

[sources.lotus_dict]              # provenance only; runtime prefers the installed IME dict
format = "fcitx_cm_dict"
url = "<upstream lotus vietnamese.cm.dict raw URL>"
sha256 = "…"
license = "TBD — verify against fcitx5-lotus upstream at phase 5"

[overlays]
shorthand = ["shorthand.tsv"]
slang = ["slang.txt"]

[build]
syllable_dict = true              # emits SYLLABLE_DICT from the lotus source
```

Curated rule tables move from Rust consts into `dicts/en/rules.toml` (contractions, apostrophe slips, compounds, function words, clitics, typo overrides, bigram boosts, casing tables). The Rust consts remain as **defaults** used when a pack has no LANGUAGE_RULES section (test helpers `from_word_list`, v2 packs).

---

## 6. Runtime changes

| Today | Becomes |
|---|---|
| `EnglishLexicon` (en-shaped) | `PackLexicon` (generic; `pub type EnglishLexicon = PackLexicon` deprecated alias for FFI/one cycle). Parses v3 + v2. `from_static_bytes` fixed to real `Cow::Borrowed` (v3 path) — kills the `lexicon.rs:343-358` copy. |
| `EnglishScorer` (en rules hardcoded) | `LatinScorer` implementing `LanguageScorer`; contractions/compounds/function-words/casing read from the pack's LANGUAGE_RULES, falling back to built-in consts. |
| `Option<EnglishScorer>` in engine | `LexiconRegistry { packs: HashMap<LangId, LoadedPack> }`; `LoadedPack = { lexicon, scorer: Latin | ViLotus }`. `evaluate_token` resolves by session lang; missing pack → today's user-rules-only path. |
| `resolve_dict_path("en")` at `session.rs:41,58`, `main.rs:44` | `registry.load_installed(lang)` with fallback chain: installed pack → bundled pack (per D1) → none. |
| Shorthand via `include_str!` (`lexicon.rs:12` → `scorer.rs:684`) | `PackLexicon::lookup_shorthand` over the SHORTHAND section. Split-brain fixed: pack overrides everything. |
| vi data via 3 × `include_str!` (`vi_telex.rs:3-5`) | Bundled v3 vi pack (feature `bundled-vi`, default on — parity with today's unconditional embed, ~<100 KB) + optional runtime read of the installed lotus dict: parse `/usr/share/fcitx5/lotus/vietnamese.cm.dict`, compare as a **normalized set** vs the pack's SYLLABLE_DICT, emit an `event_log` warning on real divergence (set difference or count change), never on mere reordering. |
| `reconcile_surrounding` gate `lang != "en"` (`session.rs:688`) | Capability gate: reconciliation enabled for `tier = "data"` packs, disabled for `tier = "ime_bridge"`. |
| `dict install` validating everything as English (`main.rs:1948`) | Header validation: `lang` must equal the requested `<lang>`; prints pack version, variants, licenses. `dict list` parses headers (cheap header read) and shows lang/version/variants/legacy flag; `-v` prints SOURCE_MANIFEST. |
| `Dialect` (en-only semantics) | `VariantSelection` internally; `dialect` key still accepted for en. |

The learner, `rules.toml` user format, event log, session state machine, and FFI ABI are unchanged.

---

## 7. Generic builder: `smartkey-dev build-pack <lang> [options]`

Options: `--dicts-dir <path>` (default: sibling `../smartkey-dicts/dicts`, else explicit error with hint), `--out <path>`, `--fetch` (download pinned sources), `--verify` (run pack eval gate after build).

Pipeline:

1. **Parse** `dicts/<lang>/pack.toml` (`toml_edit` — no new dep).
2. **Fetch** each source with `curl` (same pattern as `dict install`), verify **sha256** (`sha2` — the one new dependency); cache under `data/sources/<lang>/`. Fail hard on mismatch or missing `license`.
3. **Parse** per `format` handler: `aosp_combined` (existing `parse_aosp`, parameterized by variant bucket + `capture_bigrams`), `typos_csv`, `bigram_counts`, `variant_map_json`, `wordlist` (plain, optional `,freq`), `fcitx_cm_dict`.
4. **Merge overlays** with today's exact semantics (dev-terms freq 172, slang shield-only, proper-noun freq 160, variant-map propagation ≥120, shorthand trigger shields).
5. **Lint:**
   - Proper-noun homograph lint (findings F2): flag candidates whose lowercase form is a valid word; when a case-sensitive bigram corpus is present use lowercase-vs-capitalized share (the `polish` 73% / `april` 19% evidence), setting `FLAG_AMBIGUOUS_PROPER` (F3) or failing under `--strict`; warn+skip when no corpus exists.
   - Duplicate shorthand triggers, trigger-is-valid-word collisions, empty expansions.
   - License gate: missing → hard fail; `UNVERIFIED` / NC / ND → loud warning recorded in SOURCE_MANIFEST.
6. **Serialize** v3 deterministically (BTreeMap ordering — byte-reproducible for identical inputs).
7. **Verify:** load the image back; with `--verify`, run the language's corpus gate.

`build-lexicon` remains as a deprecated alias for `build-pack en`.

**CI (smartkey-dicts):** checkout both repos → `build-pack en --verify`, `build-pack fr --verify` (phase 4+) → upload artifacts → release on tags. **The dicts→data copy step is deleted** (D3). vi pack builds from phase 5.

---

## 8. Migration path

| Component | Today | After |
|---|---|---|
| Compiler | `compile_skdict_from_sources(8 args)` | `PackBuilder` driven by pack.toml |
| Shorthand | `include_str!` binary-side, shields pack-side | SHORTHAND pack section, single source |
| En rule tables | Rust consts `scorer.rs:90-193`, `casing.rs` | `dicts/en/rules.toml` → LANGUAGE_RULES section (consts = defaults) |
| Engine | `Option<EnglishScorer>` | `LexiconRegistry` |
| Dict install | any pack validates as en | header-lang validation |
| Curated data | two mirrors (`smartkey/data` + `smartkey-dicts/dicts`) | `smartkey-dicts/dicts/` only |
| Source pinning | `fetch_sources.sh` (AB_REF unpinned master) | pack.toml url+sha256+ref, enforced |
| vi data | 3 × `include_str!`, vendored dict already drifted | vi v3 pack + runtime lotus dict read + drift warning |
| CI | copy step, en only | `--dicts-dir` builds en/fr(/vi), no copying |
| FST copy on load | `Cow::Owned` both branches | true zero-copy on v3 static path |

---

## 9. Phase plan (each phase independently green, conventional commit, staged files only)

1. **`feat(core): skdict v3 TLV format, generic WordMeta, v2 read-compat`**
   PackLexicon rename behind alias; v3 parse/serialize; real zero-copy static load; en pack rebuilt as v3 through the existing pipeline temporarily.
   *Gate:* `cargo test --workspace` (incl. `corpus_eval`, `vi_lotus_eval`, `learning_eval`), `cargo clippy --workspace`, and a scripted A/B: `smartkey-dev eval` output (fix recall / must-not-touch / p50-p99) **identical** before vs after; pasted in the report.
2. **`feat(core): shorthand + language rules as pack sections`**
   Delete `lexicon.rs:12` include_str; scorer reads pack sections (consts as fallback); F3 ambiguous flag plumbed through proper-noun pass; en pack regenerated.
   *Gate:* same eval A/B + new section roundtrip unit tests.
3. **`feat(engine): LexiconRegistry + lang-aware dict commands`**
   Registry replaces `Option<EnglishScorer>`; session/CLI rewiring; capability gate replaces `!= "en"`; `dict install/list/update` header-aware.
   *Gate:* full tests + CLI smoke (install mismatched-lang pack must fail), eval unchanged.
4. **`feat(cli): generic build-pack + en recipe + fr proof pack`**
   pack.toml schema, `sha2` dep, lints, fr recipe from aosp-dictionaries, small `tests/corpus/fr` gate; smartkey-dicts CI switches to `build-pack --dicts-dir` (copy step deleted).
   *Gate:* en A/B eval identical; fr pack loads and corrects (demo transcript incl. accented words; smash index demonstrably omits non-ASCII without breaking).
5. **`feat(vi): vi pack + lotus runtime dict with drift warning`**
   vi pack.toml → vi.skdict (SHORTHAND + SLANG_SET + SYLLABLE_DICT + manifest); runtime installed-dict read with normalized-set drift warning; remove the 3 `include_str!`s.
   *Gate:* `vi_lotus_eval` + `corpus_eval` unchanged; drift warning demonstrated against the already-drifted installed dict (observed in §1.5).
6. **`docs: align both READMEs`**
   smartkey: "English fallback embedded, fully overridable; every language a hot-swappable pack". smartkey-dicts: contributor flow = pack.toml + overlays + licenses; CI builds en+fr; vi documented.

Verification ritual per phase: full test suite + clippy + scripted before/after `smartkey-dev eval` with pasted outputs; every claim labeled **Observed** (command output) vs **Inferred**.

---

## 10. Risks & mitigations

| Risk | Mitigation |
|---|---|
| En behavior drift on rebuild | A/B corpus eval per phase; format mapping is bit-isomorphic (appendix A); word_id ranking rule unchanged |
| Upstream drift/unavailability | url+ref+sha256 pinned per source; hard fail on mismatch; `AB_REF@master` hole closed by pack.toml |
| License landmines (Norvig unverified; NC lists e.g. Slovak; per-language variety) | per-source license mandatory; NC/ND/UNVERIFIED warnings recorded in SOURCE_MANIFEST; distribution calls stay with the maintainer, never silently stripped |
| u16 word_id cap | bigram coverage limited to top 65,535 words — acceptable for ranking; documented per pack (fr large vocab still fine for top words) |
| ASCII-only smash index on accented langs | builder includes ASCII words only; non-ASCII gracefully uncovered; Unicode folding explicitly future work |
| vi false-positive drift alarms | compare normalized syllable **sets**, not raw file bytes; ordering/header changes don't warn |
| Legacy v2 debt | read-only support, flagged in `dict list`, deleted after one release cycle |
| Pack size growth (fr etc. in releases) | artifacts via GitHub Releases as today; core binary grows only if fallbacks are enabled (en 5 MB + vi <0.1 MB, no regression vs today) |
| Determinism/reproducibility | BTreeMap-ordered serialization; CI builds are byte-reproducible for pinned inputs |

---

## Appendix A — en v2 → v3 field equivalence

| v2 | v3 | Note |
|---|---|---|
| `us_freq` | `freq[0]`, slot0 = {en_US} | identical semantics incl. −25 fallback |
| `gb_freq` | `freq[1]`, slot1 = {en_GB} | identical |
| `FLAG_IN_US/GB` | `variant_mask` bits 0/1 | identical validity logic |
| `FLAG_OFFENSIVE` (0x04) | `flags` bit 0x01 | value remap only |
| `FLAG_TECH` (0x08) | `flags` bit 0x02 (shield) | value remap only |
| `word_id: u16` | `word_id: u16` | unchanged, same ranking rule |
| uk_to_us / us_to_uk | VARIANT_REWRITES for en_US / en_GB | `Mixed` mode uses default-variant rewrites (today's behavior) |
| positional sections | TLV tags | v3 reader; v2 reader retained |
| (none) | SHORTHAND, LANGUAGE_RULES, SOURCE_MANIFEST | new capability; fallback to built-ins when absent |
