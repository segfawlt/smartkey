# SmartKey — Architecture & Implementation Plan (v2 Hardened Spec)

> **Status:** v2 Architecture Locked — Ready for Milestone 1  
> **Companion doc:** [competitor-analysis.md](./competitor-analysis.md)  
> **Core Strategy:** Pure Rust engine (`smartkey-core`) combining a **zero-copy `mmap` static DAWG + Leipzig `CC-BY-4.0` bigram noisy-channel decoder** with a **guarded, deterministic, plain-text rule layer (`rules.toml`)**, integrated first into **`fcitx5-lotus`** (`uinput` non-preedit bilingual VI+EN & pure EN) and portable unchanged to standalone English Linux adapters (`fcitx5` Module / `IBus`), Android (`HeliBoard`), and Windows (`TSF`).

---

## 1. Core Architectural Principles

1. **Functional Core, Imperative Shell (`Engine` + Per-Window `Session`):**
   - `smartkey-core` is **100% pure, synchronous, single-threaded Rust** with zero OS, clock, thread, or IME dependencies.
   - Split state cleanly into:
     - **`SmartKeyEngine`** (shared singleton per process): owns the `mmap`-backed static DAWG (`~1–2.5 MB`, `0 KB` heap), bigram table (`~400 KB`), and `toml_edit`-backed `RuleStore`.
     - **`SmartKeySession`** (1 lightweight struct per `InputContext` / window): owns compose/shadow state, `prev_word`, rolling language momentum, and `JustAutocorrected` undo state so Alt-Tabbing between Firefox and Discord never cross-contaminates context.
   - Every transition takes `now_ms: u64` explicitly (`session.handle_event(&mut engine, event, now_ms) -> EngineOutput`), keeping all timing windows 100% deterministic in unit tests.
2. **Asymmetric Learning Thresholds (`1x` Confirmed Block, `2x` Guarded Graduation):**
   - **Negative signal (Undo):** Pressing `BackSpace` within `5000 ms` of an autocorrect immediately snaps back to the verbatim word on screen and stages `pending_block = (typed, rejected)`. As soon as the user keeps, edits, or moves forward (rather than holding `BackSpace` past index 0 to erase the entire clause), the **pair-level** block rule persists permanently (`1x`) to `rules.toml`.
   - **Positive signal (Manual edit or kept unknown word):** Must pass **all 3 Anti-Poisoning Guards**, stages in `learner_state.json` at `count = 1`, and promotes to `rules.toml` at **`count = 2`**.
3. **Plain-Text Source of Truth (`Editing the file = Editing the brain`):**
   - Graduated rules live in `~/.config/smartkey/rules.toml`, mutated via **`toml_edit`** so user comments and formatting survive automatic rule graduations.
   - Pre-write `stat()` check + parse-error write lock ensures neither Syncthing updates nor temporary user syntax errors in Neovim ever get clobbered.
   - High-churn counters (1-count staging, slip telemetry) live separately in `~/.local/share/smartkey/learner_state.json`.
4. **Host Topology (`fcitx5-lotus` Primary on Linux, Clean Core for English-Only & Mobile):**
   - **Core Repo (`smartkey` — `MPL-2.0` or `MIT OR Apache-2.0`):** Pure Rust engine, lexicon compiler, C/JNI FFI, CLI/REPL, and optional lightweight English-only adapters (`fcitx5-smartkey` `Category=Module` + `ibus-smartkey` for stock GNOME).
   - **Primary Linux Host (`fcitx5-lotus-smartkey` — `GPL-3.0+`):** Integrates `smartkey-ffi` directly into `fcitx5-lotus` (pure C++, zero Go runtime, `uinput` underline-free replacement + rollover key buffering), handling both **seamless 24/7 bilingual VI+EN** and **pure English mode** (when VI Telex is toggled off).
   - **Mobile Host (`heliboard-smartkey` — `GPL-3.0`):** Links `smartkey-ffi` via `cargo-ndk` JNI into HeliBoard's suggestion & revert pipeline.

---

## 2. Repository & Packaging Topology

```text
Repo 1: smartkey  (MPL-2.0 / MIT-Apache-2.0 — 0% Vietnamese code or data)
├── Cargo.toml
├── docs/
│   ├── competitor-analysis.md
│   └── architecture-and-plan.md
├── data/
│   └── sources/                # AOSP main_en_US.source (Apache-2.0) + Harper tech dict (Apache-2.0)
│                               # + Leipzig CC-BY-4.0 prose bigrams -> compiled to en_US.skdict
├── crates/
│   ├── smartkey-lexicon/       # Offline compiler + zero-copy mmap loader for static u64 DAWG + bigrams
│   ├── smartkey-core/          # Waterfall, noisy-channel scorer, Guards 1-3, SessionState, toml_edit store
│   ├── smartkey-ffi/           # C ABI (`cbindgen`, Engine + Session handles, catch_unwind)
│   └── smartkey-cli/           # `smartkey repl`, `explain`, `rules`, `stats`, `compile-dict`
└── adapters/                   # Optional lightweight English-only Linux wrappers (no Lotus/Vietnamese)
    ├── fcitx5-smartkey/        # ~250 LOC C++ `Category=Module` addon (augments keyboard-* directly)
    └── ibus-smartkey/          # ~250 LOC Rust IBus engine for Ubuntu/Fedora GNOME users

Repo 2: fcitx5-lotus-smartkey  (GPL-3.0+ — Primary Linux Daily-Driver Target)
└── Fork/integration of LotusInputMethod/fcitx5-lotus linking `smartkey-ffi`
    • Uses Lotus's pure-C++ Telex engine + `/dev/uinput` (`fcitx5-lotus-server`) key buffer
    • Adds Bilingual Collision Shield (`"it is"` vs `"it í"`, `"message bus"` vs `"bú"`)
    • Adds 1-Backspace Telex Immunity Learning (`ảu` -> `aur` -> `telex_passthrough`)

Repo 3 (Later): heliboard-smartkey  (GPL-3.0 — Android)
└── Fork of HeliBoard linking `smartkey-ffi` via JNI (`Suggest.kt` + `InputLogic.java`)
```

---

## 3. Per-App Context Profiles & Terminal Policy

Before scoring any word, the host resolves an **`AppProfile`** from `InputContext::capabilityFlags()` and `InputContext::program()`:

| Profile | Detection Rule | Behavior |
|---|---|---|
| **`Bypass` (Off)** | `PasswordOrSensitive`, `Terminal`, `Url`, `Email`, `Digit`, `Number`, `NoSpellCheck`, `Disable`, or app in `ignored_apps` (`kitty`, `alacritty`, `foot`, `ghostty`, `wezterm`, `code`, `nvim`, `jetbrains`, etc.) | **100% disabled** — zero autocorrect, zero staging/learning. *(Future opt-in hook can selectively enable `Prose` inside `git commit` / Markdown / AI prompt CLIs via `/proc/<pid>/stat` `tpgid` or editor hooks, never at raw `zsh`/`bash`/`sudo`/`ssh` prompts.)* |
| **`Browser`** | `firefox`, `zen`, `chromium`, `brave`, `chrome`, etc. (non-URL/non-password text fields) | Full autocorrect + contractions (`dont`→`don't`) + `i`→`I` + **bigram-gated** situational fixes (`"it os"`→`"it is"`, while `"arch os"` and `"amd gpu"` stay untouched) + Markdown `` `code` `` immunity. |
| **`Chat`** | `discord`, `vesktop`, `slack`, `telegram`, `signal`, `element` | Same as `Browser`, with independently tunable `auto_capitalize_sentence` / `auto_capitalize_i` for casual lowercase chatters + `@mention`, `#channel`, and `/command` immunity. |
| **`Prose`** | Notes / office / email apps (`obsidian`, `libreoffice`, `thunderbird`, default GTK/Qt text views) | Full prose assistance including contractions, `THis`→`This`, and sentence capitalization. |

---

## 4. Lexicon & Zero-Copy Static DAWG (`crates/smartkey-lexicon`)

### 4.1 Clean Open-License Data Pipeline (Eliminating Raw `.combined` Landmines)
1. **Base English Unigrams & Profanity Shield (`Apache-2.0`):** `codeberg.org/Helium314/aosp-dictionaries` (`wordlists/main_en_US.source`).
   - Case-collision entries (`us`/`US`, `ids`/`IDs`) merge via `max(f)` for valid-word protection while preserving proper-noun capitalization flags.
   - **Curated Contraction Whitelist Only:** Strip AOSP's hostile `not_a_word` shortcuts (`os → is`, `amd → and`); keep genuine contractions (`im→I'm`, `ive→I've`, `id→I'd` [bigram-gated!], `dont→don't`, `cant→can't`, `wont→won't`, `alot→a lot`) and restrict the apostrophe-omission index strictly to verbal clitics (`n't`, `'re`, `'ve`, `'ll`, `'d`, `'m` — excluding open `*'s` possessives so unknown plurals never turn into `*'s`).
2. **Modern Developer & Linux Vocabulary Shield (`Apache-2.0`):** Merge technical/modern lemmas from `Automattic/harper` + Linux/Wayland ecosystem terms (`git`, `systemd`, `wayland`, `niri`, `hyprland`, `fcitx`, `ibus`, `uinput`, `toml`, `json`, `yaml`, `struct`, `impl`, `async`, `tokio`, `serde`, `nvim`, `ghostty`, `aur`, `pacman`, `flatpak`, `oauth`, `llm`, `webhook`, `backend`, `frontend`, `codebase`, `workflow`, `hotfix`) so `"git "` never "corrects" to `"got "`.
3. **Calibrated Prose Bigrams (`CC-BY-4.0`):** Extract ~80k top prose bigrams from **Leipzig Wortschatz English News + Wikipedia `*-co_n.txt`**, packed as sorted `(prev_id: u16, target_id: u16, log_prob_q: u8)` (`5 bytes/entry ≈ 400 KB`) over the top `65,535` words.

### 4.2 Bit-Packed `u64` Static DAWG (`~0.8–2.5 MB` `mmap`, `0 KB` Heap)
Replaces `SymSpell`'s 30–150 MB delete-dictionary with an `mmap`-able flat `u64` node array:

```text
u64 Node Layout (8 bytes exact, zero struct padding):
  bits  0..20 (20b): first_child index (up to 1,048,575 nodes)
  bits 20..26 ( 6b): child_count (0..63 -> O(1) slice &nodes[fc .. fc + count])
  bits 26..34 ( 8b): edge UTF-8 byte
  bits 34..42 ( 8b): terminal unigram freq (0..255)
  bits 42..50 ( 8b): max_subtree_freq (for branch-and-bound pruning)
  bits 50..64 (14b): flags (is_terminal, f_zero_shield, not_a_word, proper_cap, clitic_ok...)
```

**Admissible Branch-and-Bound Traversal (`< 15 µs`):**
Because adjacent transposition cost (`0.55`) is smaller than single substitution/indel (`0.75..1.45`), standard row-minimum $E_{\text{partial}}(i) = \min_j D[i][j]$ can temporarily drop from `0.90` to `0.55` when completing a transposition (`"chan"` → `"chang"` vs `"chagnes"`). To guarantee zero false pruning of transpositions, branch-and-bound uses the exact admissible lower bound at depth $i$ (edge char $c_i$):

$$E_{\text{lb}}(i) = \min\Big(\min_j D[i][j],\; \min_{\substack{1 \le j < |t| \\ c_i == t_j}} \big(D[i-1][j-1] + C_{\text{transpose}}\big)\Big)$$

---

## 5. Evaluation Waterfall & Scoring

Triggered on word boundaries (`Space`, `,`, `!`, `?`, `;`, closing `)`, `]`, `}`, `"`, `”`) **plus** deferred sentence-final `.` / `:` via `PendingTrailingDot` (see §6.1). `Enter` flushes literally without guessing.

1. **Step 0 — Context, Token & Situational Prefix Guards:**
   - Skip if `AppProfile::Bypass`, `host_lang == VietnameseValid`, or inside Markdown `` `inline_code` `` / ` ```fence``` ` (tracked via 64-char ring buffer / `surrounding_before` parity so `BackSpace` over `` ` `` cleanly pops code mode).
   - Skip tokens starting with `@`, `#`, `/` (slash commands like `/boost`), `~/`, `./`, tokens containing digits/underscores/mid-word dots, `camelCase`/`PascalCase`, `ALL_CAPS` (`len >= 2`), and unit suffixes right after numbers (`16 ms`, `32 gb`, `60 hz`, `2 em`).
   - **Desktop `InitialDoubleCap` normalization (`THis` → `This`):** When a token matches `^[A-Z]{2}[a-z]{2,}$` (excluding pluralized acronyms `^[A-Z]{2,3}e?s$` like `IDs`, `PRs`, `UIs`, `OSes`), normalize to Title Case (`This`) if the word is valid in the lexicon.
2. **Step 1 — User `[snippets]` (`rules.toml`):**
   - Exact trigger match (`"shrug"`, `"addr"`). Expands on boundary; `1x` `BackSpace` reverts to literal trigger **without** adding to `blocklist`.
3. **Step 2 — Contextual Personal Rules (`[context_corrections]`):**
   - Exact `(prev_word, typed)` match (e.g. `"sent form" = "from"`), unless blocked.
4. **Step 3 — Unigram Personal Rules (`[corrections]`):**
   - Exact `typed` match (e.g. `"fomr" = "form"`), unless `(typed, target) ∈ blocklist`.
5. **Step 4 — Valid-Word Fast-Path & Situational Bigram Overrides:**
   - If `typed` is in `personal_dict` or is a valid lexicon word (`not_a_word == false`, including `f=0` profanity shields) *without* an unblocked contraction shortcut:
     - Check **High-Confidence Situational Bigram Overrides** (e.g. `"it os"` → `"it is"`, `<start> ill <verb>` → `"I'll"`, `"think id"` → `"think I'd"`, while `"arch os"`, `"user id"`, and `"feel ill"` pass straight through!).
     - Otherwise keep `typed` untouched.
6. **Step 5 — Unconditional Contraction Shortcuts:**
   - Apply unblocked `im→I'm`, `dont→don't`, `ive→I've`, `cant→can't`, `wont→won't`, `alot→a lot`, standalone `i→I` (per active `AppProfile`).
7. **Step 6 — Length-Aware Noisy-Channel DAWG Decoder & Function-Word Splitter:**
   - Skip unigram statistical guessing on `len <= 2` tokens; for `len == 3`, require adjacent transposition (`teh→the`, `adn→and`) or strong bigram boost.
   - **2-word missed-space splitter:** Only split `w1 + " " + w2` when `(w1, w2)` is a frequent Leipzig bigram **and** at least one half is a closed-class function word (`ofthe`, `inthe`, `tobe`, `isnot`) — never splitting closed compounds (`backend`, `codebase`, `workflow`, `hotfix`).
   - **Length-damped score:**
     $$S(c \mid t, w_{-1}) = \alpha \cdot \frac{f(c)}{255.0} + \beta \cdot \text{bigram}(w_{-1}, c) - \gamma(L) \cdot E_{\text{layout}}(t, c)$$
     with $\gamma(L) = \gamma_0 / (1 + \kappa \cdot \min(L - 3, 6))$, hardware keycode (`evdev` / `xkb_keymap`) adjacency (`0.75`), transposition (`0.55`), unstressed vowel/geminate orthographic confusion (`0.65` for `len >= 6`, catching `definately`, `seperate`, `occured`), indel (`0.90`), and non-adjacent substitution (`1.45`).
   - Auto-apply top candidate $c_1$ iff $S(c_1) \ge T_{\text{auto}}$ (slightly raised on slow deliberate keystroke cadence `> 350 ms/key`) and $S(c_1) - S(c_2) \ge M_{\text{margin}}$, then project casing.

---

## 6. Hardened Approach #3 Learning State Machine & Guards 1–3

### 6.1 Key State-Machine Upgrades
1. **`PendingTrailingDot` (Fixes `"I made chagnes. "` While Protecting `"teh.com"`):**
   - Typing `.` or `:` at the end of `Composing { buffer: "chagnes" }` enters `PendingTrailingDot { word: "chagnes", punct: '.' }`.
   - Next key is identifier/URL char (`'c'` in `teh.com`, `'/'`, digit) → bypasses token cleanly.
   - Next key is `Space`, `Enter`, or closing quote/paren (`"chagnes. "`) → evaluates `"chagnes"` and replaces `"chagnes."` → `"changes. "`!
2. **Multi-Char `trailing_boundaries` Chord Support (`"fomr, "` → `BackSpace`):**
   - `JustAutocorrected` stores `trailing_boundaries: SmallString` (up to 3 chars, e.g. `", "`, `". "`, `"\") "`). Typing `Space` right after `,` appends `' '` to `trailing_boundaries` instead of wiping undo state, so 1 `BackSpace` still snaps `"foam, "` back to `"fomr"`.
3. **Commit-Confirmed `1x` Block & Clause-Erase Safety (`"Actually teh "` → Hold `BackSpace`):**
   - On 1st `BackSpace` in `JustAutocorrected`: visually snap `"the "` → `"teh"` immediately and set `pending_block = Some(("teh", "the"))` in `EditingRestored`.
   - If the user keeps holding `BackSpace` **past index 0** (`current` becomes empty and another `BackSpace` deletes into `"Actually "`), or hits `Ctrl+BackSpace` twice — **discard `pending_block` and exit to `Idle`**! Deleting a sentence never poisons `"teh → the"`.
   - As soon as the user commits `raw`, edits `raw` into a valid repair, or types forward, `pending_block` flushes permanently (`1x`) to `rules.toml`.
4. **Intra-Clause `prev_word` Hygiene:**
   - Only `' '` carries `prev_word = Some(word)` forward; `.`, `!`, `?`, `;`, `:`, `,`, `\n`, `(`, `)` reset `prev_word = None` so `"I sent. Form..."` never triggers `"sent form" = "from"`.

### 6.2 The Three Anti-Poisoning Guards (Before Staging Any Manual Edit)
1. **Guard 1 — Mind-Change Similarity Filter:**
   - Require `orig_lower != fixed_lower`, `fixed.len() >= 2`, and weighted Damerau–Levenshtein distance:
     - $\max(|a|, |b|) \le 4 \implies d \le 1$ (allowing keyboard-adjacent 2–3 char slips like `if↔of` only into Guard 2/3 contextual routing)
     - $5 \le \max(|a|, |b|) \le 7 \implies d \le 2$
     - $\max(|a|, |b|) \ge 8 \implies d \le 3$, with shared initial char or $\ge 60\%$ char overlap.
   - Rejects `"Monday" → "Tuesday"` and `"yes" → "no"`.
2. **Guard 2 — Valid-Word Shield:**
   - If `orig` is NOT a valid lexicon/personal word (`woryr`, `chagnes`, `fomr`): stage as unigram `[corrections]` (`2x` graduation).
   - If `orig` IS a valid word (`form` → `from`, `their` → `there`): **never** stage as a unigram rule; pass to Guard 3 for `[context_corrections]`.
3. **Guard 3 — Contextual Bigram Asymmetry Shield:**
   - Only stage `(prev_word, orig) → fixed` when `(prev_word, orig)` has **near-zero / ungrammatical probability** in the Leipzig bigram table ($P(\text{orig} \mid \text{prev}) < P_{\text{floor}}$ — true for `"sent form"`, `"instead if"`, `"over their"`, **false** for grammatical phrases like `"the cat"`, `"based in"`, `"is in"`) **and** $\log P(\text{fixed} \mid \text{prev}) - \log P(\text{orig} \mid \text{prev}) \ge \Delta_{\text{ctx}}$.

---

## 7. Portable `rules.toml` Schema (v2)

```toml
version = 1

[settings]
enabled = true
aggressiveness = "balanced"     # "conservative" | "balanced" | "aggressive"
undo_window_ms = 5000
auto_capitalize_i = true
auto_capitalize_sentence = false
fix_double_cap = true           # THis -> This (skips IDs, PRs, UIs)
enable_contractions = true
enable_missed_space_split = true
markdown_code_immunity = true
ignored_apps = [
    "alacritty", "kitty", "foot", "ghostty", "wezterm", "konsole",
    "code", "nvim", "neovim", "vim", "emacs", "jetbrains",
]

[profiles.chat]
apps = ["discord", "vesktop", "slack", "telegram", "signal"]
auto_capitalize_sentence = false

personal_dict = [
    "fcitx5",
    "promptcord",
    "segfawlt",
]

# Phase 2 / fcitx5-lotus: ASCII tokens kept immune from Vietnamese Telex accents
# (learned automatically after 2x undoing Telex on e.g. `ảu` -> `aur`).
telex_passthrough = [
    "aur",
    "os",
    "ux",
]

# Permanent pair-level rejections (1x confirmed undo).
blocklist = [
    { typed = "fomr", rejected = "foam" },
    { typed = "cant", rejected = "can't" },
]

# Learned typo pairs (2x confirmed manual edits).
[corrections]
"chagnes" = "changes"
"fomr" = "form"
"woryr" = "worry"

# Context-scoped corrections (guarded by Guards 2 & 3).
[context_corrections]
"sent form" = "from"
"over ther" = "there"
"in ther" = "their"

# User-authored expansions: 1x BackSpace reverts to literal trigger WITHOUT blocklisting!
[snippets]
"shrug" = "¯\\_(ツ)_/¯"
```

---

## 8. `fcitx5-lotus` Primary Integration Design (Bilingual VI + EN)

Inside `fcitx5-lotus` (`lotus-state.cpp`), `lotus` already intercepts keys, runs C++ Telex composition, buffers rollover keys during replacement (`bufferedKeys_`), and emits hardware-speed `/dev/uinput` Backspaces via `fcitx5-lotus-server` (filtering its own `uinput` echoes).

Our ~200-line C++ hook connects `lotus` to `smartkey-ffi` at three clean points:

1. **Bilingual Collision Resolution at Word Commit:**
   - **Case A — Unambiguous Vietnamese (`"tiếng"`, `"được"`, `"anh"` when `lang_momentum != English`):** Set `session.record_vietnamese_word()`, skip English autocorrect.
   - **Case B — Restored Non-Vietnamese ASCII (`AutoNonVnRestore` produced `"chagnes"`, `"woryr"`):** Query `smartkey` waterfall; if corrected (`"changes"`), emit delta via `lotus`'s existing `uinput` Backspace + commit queue!
   - **Case C — Dual-Valid Collision (`raw_keys` = `"is"`, `"as"`, `"of"`, `"if"`, `"or"`, `"has"`, `"bus"`, `"aur"`, `"os"`, `"ux"` while `lotus` produced `"í"`, `"há"`, `"bú"`, `"ảu"`):**
     - If `raw_keys ∈ telex_passthrough ∪ personal_dict`, **or** `lang_momentum == English` (e.g. `"it"` before `"is"`, `"message"` before `"bus"`): override `lotus` back to raw English (`"is"`, `"bus"`, `"aur"`)!
2. **Unified `BackSpace` Undo (English Blocklist + Telex Immunity):**
   - If the previous commit was an **English autocorrect** (`"fomr"` → `"foam "`): `1x` `BackSpace` uses `lotus` `uinput` to snap `"foam "` → `"fomr"` and arms `pending_block("fomr", "foam")`.
   - If the previous commit was a **Telex transformation** (`"aur"` → `"ảu "`) and the user immediately repairs `"ảu "` → `"aur "`: stages `"aur"` toward `telex_passthrough` (`2x` graduation).

---

## 9. Build Milestones

1. **Milestone 1 — `smartkey-lexicon` & `smartkey-core` Scorer:**
   - Build `compile-dict` (AOSP `.source` + Harper tech dict + Leipzig bigrams → static `u64` `mmap` DAWG `.skdict`), admissible branch-and-bound scorer, situational bigram overrides (`"it os"` vs `"arch os"`, `"think id"` vs `"user id"`), and benchmark suite (`< 15 µs`/word, 0 false positives on dev logs).
2. **Milestone 2 — Hardened State Machine, Guards 1–3, & `toml_edit` Store:**
   - Implement `SmartKeyEngine` + `SmartKeySession`, `PendingTrailingDot`, `trailing_boundaries`, clause-erase `pending_block` cancel, `[snippets]`, and deterministic test suite covering all edge cases.
3. **Milestone 3 — `smartkey-cli` (`repl`, `explain`, `rules`, `stats`) & `smartkey-ffi`:**
   - Interactive raw-terminal sandbox to test typing, undo, triangle repairs, and score explanations live, plus `cbindgen` C staticlib.
4. **Milestone 4 — `fcitx5-lotus-smartkey` Primary Linux Integration:**
   - Clone `LotusInputMethod/fcitx5-lotus`, wire `smartkey-ffi` into `lotus-state.cpp`, add bilingual momentum & Telex immunity learning, and install on Arch Wayland for daily-driver dogfooding.
5. **Milestone 5 — Secondary Targets (`fcitx5-smartkey` Module / `ibus-smartkey` / Android `HeliBoard`):**
   - Package lightweight English-only adapters for non-Vietnamese Linux users and JNI bridge for HeliBoard (`Suggest.kt` + `InputLogic.java`).
