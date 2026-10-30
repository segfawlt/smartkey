# Competitor & Ecosystem Feature Analysis

> **Project:** `smartkey` (working repo name — renameable anytime)  
> **Scope:** Privacy-first, deterministic learning autocorrect & typing assistant across **Linux (`fcitx5` / `fcitx5-lotus`)**, **Android (`HeliBoard`)**, and **Windows** (future).  
> **Core Thesis:** Combine **Gboard/AOSP-grade base autocorrect** (`main_en_US.combined` noisy-channel decoding) with **Pilcrow/EkaKey-style Undo & Edit Learning**, backed by a **100% local, plain-text, human-editable rule store (`rules.toml`)** and **developer/bilingual safety guards** that existing keyboards miss.

---

## 1. Executive Summary: Why Existing Tools Fall Short

Across mobile and desktop, existing typing tools split into four disconnected camps — none of which solve the full loop:

1. **Mainstream Mobile Keyboards (Gboard, iOS Keyboard):** Strong noisy-channel + neural autocorrect and 1-tap Backspace undo, **but learning is an opaque black box**. Undone corrections resurface later, editing a bad correction into a *third* word (`fomr` → auto `foam` → user fixes to `form`) loses the link to `fomr`, and nothing is inspectable or available on Linux desktop.
2. **Privacy & Learning Niche Keyboards (Pilcrow on iOS, EkaKey on Windows):** Prove that *"undo once = never auto-apply that pair again"* and *"Backspace → manual edit → Space = learn personal rule"* feel transformative. However, **Pilcrow is closed-source and iOS-only**, while **EkaKey uses raw Win32 keyhooks** that cannot detect password fields, strips all non-`a-z` characters (breaking `don't`/`I'm` and non-English scripts), blocks whole words instead of `(typed, wrong_target)` pairs, and lacks edit-distance guards against mind-changes (`Monday` → `Tuesday`).
3. **Linux Desktop IMEs (`fcitx5` built-ins, `quan-autocorrect`, `SmartType`, `ibus-typing-booster`):** Either have **zero real autocorrect** (`fcitx5` `spell` only shows manual `Ctrl+Alt+H` hints; `quan-autocorrect` is a static 173-word map with no undo or learning) or force **intrusive CJK-style candidate popups and underlined preedit** on every word (`ibus-typing-booster`, `my7bropx/SmartType` even hijacks number keys `1–5` mid-word).
4. **Vietnamese Bilingual Linux IMEs (`fcitx5-bamboo`, `fcitx5-lotus`):** Solve Telex/VNI composition and `uinput` non-preedit replacement (`fcitx5-lotus`), **but have zero English autocorrect** and rely on single-word `AutoNonVnRestore`, which fails whenever short English/tech words happen to be valid Vietnamese syllables (`it is` → `it í`, `she has` → `she há`, `aur` → `ảu`, `os` → `ó`, `ux` → `ũ`).

---

## 2. Master Feature Comparison Matrix

| Product | Platform | License | Base Autocorrect Engine | 1-Key Undo Restore | Undo → Permanent Blocklist | Manual Edit (`BS→fix→Space`) Learning | Triangle Capture (`raw→bad→fixed`) | Human-Editable Plain File | Password / Terminal / Code Safe | Contractions & Context (`im`→`I'm`, Bigrams) | Bilingual EN+VI Aware |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Pilcrow** | iOS / iPadOS | Proprietary (Paid) | Context-aware local neural/statistical | Yes (BS + chip) | **Yes (pair-level)** | Partial (confusion mining) | Partial | UI list only (no raw file) | iOS text-trait aware; no `code` guard | Yes + 40-lang mixed | Multi-lang, not Telex |
| **EkaKey** | Windows | MIT | `autoTrie` + Double Metaphone (`a-z` only) | Yes (BS via `SendInput`) | Word-level block only | **Yes** (SQLite) | No | SQLite via Flutter sidepanel | **No** (warns to disable for passwords!) | **No** (`a-z` strip removes `'`) | No (26 ASCII chars only) |
| **Gboard** | Android, iOS | Proprietary | Spatial + AOSP dict + quantized neural LM | Yes (BS + tap chip) | **No** (opaque decay; recurs) | Opaque dynamic cache | **No** (drops `raw` link) | **No** | Mobile field-flag aware; mangles `` `code` `` | **Best-in-class** | Good mobile bilingual |
| **HeliBoard** | Android | **GPL-3.0** | AOSP `liblatinime` (`.combined` unigram+bigram) | Revert on BS (no rule) | **No** | **No** (only word freq) | **No** | Import/export dict | Incognito + Android password flags | **Yes** (`whitelist` + bigrams) | Separate layout switch |
| **FUTO Keyboard** | Android | FUTO Source First (non-OSS) | AOSP dict + local Llama Transformer LM | Revert on BS | **No** | **No** | **No** | UI personal dict | Android field flags | Yes (Transformer LM) | Limited outside EN |
| **FlorisBoard** | Android | Apache-2.0 | Rust `florisboard/nlp` (alpha) | Basic | **No** | **No** | **No** | JSON user dict | Android field flags | Incomplete (alpha) | No |
| **`quan-autocorrect`** | Linux (`fcitx5`) | MIT | Hardcoded 173-word C++ `unordered_map` | **No** (`Esc` before Space only) | **No** | **No** | **No** | Edit C++ & recompile | **Strong** (`CapabilityFlag` + app blocklist) | Static `dont`→`don't` only; no `im`→`I'm` | No |
| **`my7bropx/SmartType`** | Linux (`IBus`) | Open Source | Rust `BTreeMap` prefix + edit-dist 1 | **No** | **No** | Frequency boost only | **No** | `learned_words.json` | **Weak** (hijacks `1–5` keys; inline preedit) | Smart quotes/dashes only | No |
| **`Mekhanic/smarttype`** | Linux (`fcitx5`) | Proprietary binary | Closed RU/EN context + layout fixer | Yes (1-BS undo) | Unknown / closed | Unknown / closed | No | **No** | Wayland KDE/GNOME only (no Arch) | Yes (closed) | RU+EN only |
| **`ibus-typing-booster`** | Linux (`IBus`) | GPL-3.0+ | Python + Hunspell + CLDR + SQLite trigrams | No (tab-completion UI) | **No** | Learns n-gram frequencies | **No** | SQLite `user.db` | Field flags, heavy popup UI | Context trigrams, no silent `im`→`I'm` | Multi-dict, not Telex |
| **macOS Native** | macOS | Proprietary | Apple lexicon + Sonoma inline Transformer | Yes (BS / `Esc` bubble) | Temporary dampening | Dynamic opaque cache | **No** | Partial (`System Settings`) | **Weak for devs** (mangles `` `code` `` & CLI terms) | **Yes** | Separate VI Telex IM |
| **`fcitx5-lotus`** | Linux (`fcitx5`) | **GPL-3.0+** | None for EN (C++ port of `bamboo-core` for VI) | VI restore key only | **No** | Macro table (manual) | **No** | Config files | App exclusion list + `uinput` non-preedit | **No** | VI Telex + `AutoNonVnRestore` |
| **`smartkey` (Ours)** | **Linux → Android → Win** | **GPL-3.0+** | **AOSP `.combined` + QWERTY/Transpose + Bigrams** | **Yes (1-BS snap)** | **Yes (pair-level)** | **Yes (similarity + valid-word guarded)** | **Yes** | **`rules.toml` (hot-reloaded)** | **Full (`CapabilityFlag` + app list + `` `backtick` `` guard)** | **Yes (`whitelist` + free `'` + bigrams)** | **Phase 2: `lotus` bridge + Telex shields** |

---

## 3. Deep-Dive Competitor Breakdowns

### 3.1 Pilcrow Keyboard (`pilcrowtype.com` — iOS / iPadOS)
*The closest product-philosophy reference on mobile.*

- **Complete Feature Set:**
  - **Smart Backing Off (Undo → Blocklist):** Undoing an automatic correction once (via Backspace or the undo chip) permanently prevents that specific `(typed → corrected)` pair from firing again.
  - **Sentence-Level Context Spellcheck:** Evaluates surrounding sentence context rather than greedily rewriting rare valid words.
  - **Adaptive Touch Landing Zones:** Learns per-key finger tap offsets on glass (+3.8 percentage points tap accuracy reported).
  - **Quoted Verbatim Slot:** Keeps the exact typed string accessible as a 1-tap escape hatch.
  - **One-Tap Whole-Message Repair:** Scans the entire drafted message on demand, highlights repaired words, and supports 1-tap full revert.
  - **Simultaneous Multi-Language Typing:** Supports typing across up to 40 languages in the same sentence without manual layout switching, plus automatic accent restoration.
  - **Inspectable Learned Dictionary:** Dedicated UI screen listing learned words with per-item deletion.
  - **Zero-Network Architecture:** Ships with zero networking code linked into the binary; works completely with iOS *"Allow Full Access"* turned **off**.
  - **Visual Customization:** Custom photo backgrounds, 23 keycap font styles, adjustable height, number row, haptics, emoji memory.
- **Limitations & Takeaways for `smartkey`:**
  - **Locked to iOS & closed-source:** Cannot run on Linux, Android, or Windows, and rules cannot be synced as a plain text file via Git/Syncthing.
  - **What to adopt:** Pair-level permanent blocklist on single undo; strict zero-network binary guarantee; inspectable/deletable rule list; deferred adaptive touch zones for the Android HeliBoard phase.

---

### 3.2 EkaKey (`RanvirRox/EkaKey-autocorrect-globally` — Windows)
*The closest open-source desktop proof of the Undo → Edit → Learn loop — and a goldmine of architectural lessons on what NOT to do.*

- **Complete Feature Set:**
  - **Global Win32 Hook Architecture:** C++ `WH_KEYBOARD_LL` low-level keyboard listener (non-intercepting) paired with Windows `SendInput` synthetic Backspaces to rewrite words across Notepad, browsers, Discord, and IDEs.
  - **Hybrid Trie + Phonetics Engine:** Combines `autotrie` prefix/frequency lookup with **Double Metaphone** phonetic matching (`dart_phonetics`) so sound-alike spelling errors (`definately` / `enviroment`) match even at edit distance > 1.
  - **1-Backspace Undo + Permanent Block:** Pressing `Backspace` immediately after an autocorrect restores the original word (e.g., `'Rizzler'`) and marks it non-autocorrectable in SQLite.
  - **"Editing Mode" (`Space → Backspace → Edit → Space`):** Entering `Backspace` right after committing a word + `Space` starts editing mode; pressing `Space` again saves `lastIncorrectWord → currentWord` to SQLite as a personal correction/abbreviation (`hlo → hello`, `ur → your`).
  - **SQLite Side-Panel Manager (Flutter UI):** Floating snappable overlay (`Ctrl+Alt+S` pin, `Shift+Tab` cycle suggestions, `Ctrl+Alt+D` master on/off) with a side panel to delete individual learned entries or bulk-purge entries older than $N$ days.
- **Verified Architectural Flaws (from EkaKey's source & README) That `smartkey` Must Fix:**
  1. **Severe Password / Security Blindspot:** Because Win32 global keyhooks have zero input-context metadata, **EkaKey cannot tell when you are typing in a password prompt**. Its own README warns: *"I recommend disabling EkaKey with `Ctrl+Alt+D` when entering passwords or sensitive login IDs."* On Linux, `fcitx5` gives us `CapabilityFlag::PasswordOrSensitive` natively so password fields are 100% bypassed automatically.
  2. **26-Character ASCII Sanitizer Breaks Contractions & Non-English:** EkaKey's `sanitize_dictionary.dart` strips every word containing anything outside `a–z`. It cannot represent `don't`, `I'm`, `can't`, hyphens, or accented scripts like Vietnamese.
  3. **Word-Level Block Instead of Pair-Level Block:** Undoing `'fomr' → 'foam'` marks `'fomr'` globally un-correctable rather than blocking the pair `('fomr', 'foam')`, preventing `'fomr'` from subsequently learning `'form'`.
  4. **No Mind-Change or Valid-Word Filter (`Ctrl+Backspace` Workaround):** Because EkaKey saves *any* `word + Space → Backspace → newWord + Space` transition blindly, changing your mind (`Monday` → `Tuesday`) or fixing a real word (`form` → `from`) corrupts the personal dictionary. EkaKey forced users to memorize `Ctrl+Backspace` as a manual *"delete without learning"* escape key instead of filtering by edit-distance similarity and valid-word protection.

---

### 3.3 Gboard (Google — Android & iOS)
*The gold standard for how English base autocorrect should feel.*

- **Complete Feature Set:**
  - **Noisy-Channel + Quantized Neural LM:** Combines spatial key-hitbox Gaussian probabilities $P(\text{tap} \mid \text{key})$, motor transposition/omission probabilities, AOSP `.combined` unigram/whitelist rules (`im`→`I'm`, `dont`→`don't`, `cant`→`can't`), missed-space word splitting (`ofthe`→`of the`), and a quantized on-device neural language model for multi-word context (`over ther`→`over there` vs. `in ther house`→`in their house`).
  - **Undo UX:** Immediate `Backspace` reverts an autocorrection to the verbatim typed string; recent builds (Oct 2025+) also allow tapping a highlighted corrected word later in the sentence to pop up an inline revert chip.
  - **Ecosystem Extras:** Glide/swipe typing, inline grammar check, Smart Compose ghost completions, multi-item clipboard manager, emoji kitchen, simultaneous bilingual layouts, federated learning.
- **Why Power Users Still Rebel Against It:**
  - **Opaque, Non-Deterministic Memory:** Rejecting a bad correction decays a hidden dynamic weight rather than writing a hard block rule — weeks later, the exact same unwanted correction comes back.
  - **Broken Triangle Repair:** When `fomr` autocorrects to `foam`, and you backspace and type `form`, Gboard breaks the causal chain back to `fomr`.
  - **Zero Inspectability:** No way to open your personal slip table, audit learned words, or delete one poisoned rule.
  - **Mobile-Only & Cloud-Coupled:** Unavailable on Linux/Windows desktops; closed binary with telemetry/cloud integration.

---

### 3.4 Open-Source Android Privacy Keyboards (HeliBoard, FUTO, FlorisBoard)

#### HeliBoard (`HeliBorg/HeliBoard` — GPL-3.0, **Chosen Android Base**)
- **Features:** Mature fork of OpenBoard / AOSP `LatinIME`. Full `.combined` / `.dict` dictionary support (`Helium314/aosp-dictionaries`) including calibrated `0..255` word frequencies, `whitelist` shortcuts (`im`→`I'm`), `bigram` tables, `f=0` profanity no-suggest shields, emoji search dictionaries, personal dictionary, dynamic user-history dictionary, strict incognito mode, full offline backup/restore, rich theming, zero network permission.
- **Gaps `smartkey` will fill:** Uses stock AOSP learning (remembers typed words/bigrams, **zero** undo→blocklist memory, **zero** manual-edit typo-pair learning). Swipe typing currently requires an optional closed Google `libjni_latinimegoogle.so` until NLnet's open gesture library matures.

#### FUTO Keyboard (`keyboard.futo.tech` — FUTO Source First License)
- **Features:** Runs AOSP dictionary lookup in parallel with an on-device **Llama-architecture Transformer LM** (tens of millions of parameters, user-tunable *Transformer LM Strength* slider) for strong contextual repairs on badly mangled words, plus built-in offline Whisper voice-to-text and open swipe typing.
- **Limitations:** Source-available license forbids commercial/open-source forks (cannot merge with GPL-3.0); Transformer LM increases RAM/battery footprint and still lacks deterministic undo-blocklist rules or desktop support.

#### FlorisBoard (`florisboard/florisboard` — Apache-2.0)
- **Features:** Modern Kotlin + Jetpack Compose architecture, clipboard manager, rich extension/theme format, and a native Rust suggestion core (`florisboard/nlp`).
- **Limitations:** Autocorrect and prediction engine have remained in alpha/incomplete state across v0.4.x; adopting it would mean building basic mobile autocorrect plumbing before even starting on learning features.

---

### 3.5 Linux Desktop Competitors (`fcitx5` & `IBus` Ecosystem)

#### `0xQuan93/quan-autocorrect` (`fcitx5`, C++20, MIT — 290 LOC)
- **Features:**
  - Single-file `fcitx5` `InputMethodEngine` built for Arch/Wayland (`5.1.22+`).
  - Hardcoded `unordered_map` of **173 unambiguous English typos** (`teh`→`the`, `recieve`→`receive`, `dont`→`don't`).
  - Triggers on `Space`, `,`, `!`, `?`, `;` — deliberately **excludes `.` and `:`** so domains (`teh.com`), filenames, and `key:value` tokens are never mangled.
  - Preserves lowercase, Title Case, and `ALL CAPS`, while skipping `camelCase`/`PascalCase` mixed-case identifiers and tokens containing digits/symbols.
  - Clean `eligible(InputContext*)` guard: auto-disables on `PasswordOrSensitive`, `Terminal`, `NoSpellCheck`, `Url`, `Email`, `Digit`, `Number`, `Disable`, and hardcoded terminal/editor binaries (`alacritty`, `kitty`, `foot`, `ghostty`, `wezterm`, `code`, `nvim`, `emacs`, `jetbrains`).
  - Preserves XCompose / dead-key sequences, offers `Esc` before Space to keep raw spelling, `Ctrl+Alt+S` + `Alt+1..5` to query `fcitx5`'s `spell` Enchant/Hunspell hints, and ships with a clean headless `ctest` suite.
- **Gaps:** No real dictionary (misses 99% of typos, including `im`→`I'm`, `woryr`→`worry`, `chagnes`→`changes`), uses underlined preedit, **has zero Backspace-undo** (state clears immediately on `commitString`), and **has zero learning**.
- **What we adopt:** Its MIT-licensed `fcitx5` capability/editor bypass checklist, `.`/`:` separator discipline, XCompose pass-through, and headless `ctest` pattern.

#### `my7bropx/SmartType` (`IBus`, Rust + Go)
- **Features:** Buffers every word ≥ 2 chars in underlined preedit, queries a Rust `BTreeMap` prefix + edit-distance-1 engine, pops up a 5-slot candidate window at the caret on every word, boosts accepted words in `~/.local/share/smarttype/learned_words.json`, and converts `--`→`—` / `...`→`…`.
- **Why it fails for fast desktop touch-typists:** Built on IBus (not `fcitx5`); spawns a flickering candidate window at your cursor on every single English word; **binds bare number keys `1`–`5` to candidate selection while typing**, breaking alphanumeric words (`mp4`, `v2`, `fcitx5`); has no Backspace-undo or typo-pair learning.

#### `Mekhanic/smarttype` (`fcitx5`, Closed-Source Binary)
- **Features:** Context-aware Russian + English autocorrect on `fcitx5`, automatic wrong-keyboard-layout switching (`ghbdtn` → `привет`), interactive cursor hints, and 1-key `Backspace` undo.
- **Limitations:** Proprietary closed-source binary distributed via Telegram; officially targets only Fedora/Ubuntu (excludes Arch, X11, XWayland). Validates demand on `fcitx5` Wayland, unusable as open base.

#### `ibus-typing-booster` (`IBus`, Python + SQLite, GPL-3.0+)
- **Features:** The veteran Linux predictive IME. Combines multiple Hunspell dictionaries + CLDR data + `~/.local/share/ibus-typing-booster/user.db` SQLite user trigram frequencies, offline text-corpus training, inline completion, emoji/symbol search, and simultaneous multilingual dictionaries.
- **Why it solves a different problem:** Designed around **visible candidate lists and Tab prefix completion** (assistive / low-keystroke predictive entry) on IBus, rather than invisible high-WPM post-word autocorrect + 1-key undo-to-blocklist on `fcitx5`.

#### Built-in `fcitx5` `keyboard` + `spell` Module (`en_dict.fscd`)
- **Features:** Ships with a 1.3 MB `en_dict.fscd` binary list and Enchant/Hunspell bridge (`spell_public.h`), also reused by `fcitx5-android`.
- **Limitations:** **Zero automatic replacement on Space** and **zero undo/learning** — only populates manual hint lists (`Ctrl+Alt+H`), and currently suffers from an upstream `.fscd` bug where word frequencies are ignored during `hint()` ranking.

---

### 3.6 Adjacent Desktop References (macOS, Espanso, Harper)

- **macOS Native Autocorrect + Sonoma Predictive Text:**
  - *Strengths:* Seamless 1-Backspace revert, subtle blue underline under recently corrected words, good contraction/capitalization handling, inline gray ghost completions (`Tab` to accept).
  - *Infamous developer pain points:* Constantly rewrites identifiers inside Slack/Discord/GitHub Markdown backticks (`` `im` `` → `` `I'm` ``), mangles CLI commands and acronyms in chat, and stores learned adaptations in opaque `~/Library/Spelling` files that users frequently nuke wholesale when poisoned.
- **Espanso (`espanso/espanso` — Cross-platform, Rust, GPL-3.0):**
  - *Strengths:* Deterministic plain-text YAML rule files (`~/.config/espanso/match/base.yml`) with live hot-reload and `Backspace` undo on expansions.
  - *Gaps:* Purely static trigger→expansion (`O(1)` exact string match) — zero dictionary, zero edit-distance/transposition scoring, zero bigram context, and zero automatic learning from user edits.
- **Harper (`Automattic/harper` — Rust, Apache-2.0):**
  - *Strengths:* Sub-millisecond offline English grammar & typo checker built in Rust specifically for developers (natively understands Markdown code blocks, inline backticks, and code comments). Great architectural inspiration for **prose-vs-code awareness**, though built as an editor/LSP linter rather than a live IME.

---

### 3.7 Bilingual Vietnamese + English Linux IMEs (`fcitx5-bamboo` vs. `fcitx5-lotus`)

| Capability | `fcitx5-bamboo` (`fcitx/fcitx5-bamboo`) | `fcitx5-lotus` (`LotusInputMethod/fcitx5-lotus`) |
|---|---|---|
| **Tech Stack** | C++ wrapper + Go `bamboo-core` (`cgo` static archive) | **Pure C++** (ported `bamboo-core`, zero Go runtime) + `libevdev`/`uinput` |
| **License** | LGPL-2.1+ (`bamboo-core` MIT) | **GPL-3.0-or-later** |
| **Typing Mode** | Inline underlined preedit default (SurroundingText/BS modes flaky on Wayland Electron) | **Non-preedit (underline-free)** everywhere via `uinput` (`fcitx5-lotus-server`) |
| **Bilingual Handling** | Single-word `AutoNonVnRestore` + manual macro/spellcheck toggle | Single-word `AutoNonVnRestore` + app exclusions + Emoji picker |
| **Shared Blindspots** | 1. **Zero English autocorrect** (`chagnes`, `woryr`, `im`, `dont` stay broken).<br>2. **Short-word Telex collisions:** English words that happen to be valid Vietnamese syllables get accented mid-English sentence (`it is` → `it í`, `she has a bus` → `she há a bú`, `of` → `ò`, `if` → `ì`, `or` → `ỏ`).<br>3. **Tech acronym collisions:** `aur` → `ảu`, `os` → `ó`, `ux` → `ũ`, `qa` → `qả` unless manually added to a GUI exception list. | Same bilingual blindspots — making `fcitx5-lotus` the ideal Phase 2 consumer of `smartkey`'s public `fcitx5` addon API (`smartkey_public.h`). |

---

## 4. Product Synthesis: What `smartkey` Adopts, Rejects, and Invents

### What We Adopt (Proven Winners)
1. **From Gboard / HeliBoard (AOSP):** Ingest `main_en_US.combined` directly (`160k` words, `0..255` log-frequencies, `im`→`I'm` / `cant`→`can't` `whitelist` shortcuts, `bigram` context boosts, `f=0` profanity no-touch shield, near-free apostrophe omission, and missed-space splitting).
2. **From Pilcrow:** Pair-level permanent blocklist `(typed, rejected)` on a single `BackSpace` undo; strict zero-network architecture; inspectable learned state.
3. **From `quan-autocorrect`:** Desktop `fcitx5` safety discipline — auto-bypass `PasswordOrSensitive`, `Terminal`, `Url`, `Email`, `NoSpellCheck`, and terminal/IDE binaries; never trigger guessed replacements on `.` or `:` so domains/paths (`teh.com`, `foo:bar`) stay intact; headless `ctest` suite.
4. **From Espanso:** Plain-text, human-readable, diffable config (`rules.toml`) as the single source of truth — editing the file *is* editing the brain (with `stat()` `mtime` hot-reload, Syncthing-ready).

### What We Explicitly Reject (Competitor Anti-Patterns)
1. **No flickering cursor candidate popups on every English word** (unlike `SmartType` / `ibus-typing-booster`). Silent pass-through / underline-free composition by default on desktop.
2. **Never hijack number keys `1–5` or `Tab` during normal typing** (unlike `SmartType`).
3. **Never use global raw keyhooks without field metadata where an IME framework exists** (avoids EkaKey's password-capture hazard).
4. **Never strip apostrophes or non-ASCII characters**, and **never block whole source words** when only one wrong target candidate failed.

### What Makes `smartkey` Uniquely Better (Our Differentiators)
1. **Full Triangle Repair (`raw_slip` → `wrong_auto` → `user_fixed`):**
   - If `fomr` autocorrects to `foam `, pressing `BackSpace` once snaps `"foam "` back to `"fomr"` **and** permanently blocklists `("fomr", "foam")`; editing `"fomr"` → `"form "` then **also** learns `"fomr" → "form"`. Works both for single-key Backspace edits and desktop **`Ctrl+BackSpace`** whole-word rewrites.
2. **Two-Shield Manual-Edit Learner (No EkaKey Database Poisoning):**
   - **Mind-change shield:** Bounded weighted Damerau–Levenshtein similarity rejects rewrites like `"Monday"` → `"Tuesday"`.
   - **Valid-word shield:** Never promotes a valid base word (`"form"` → `"from"`) into an unconditional global rule; routes real-word confusions into **2-word contextual rules** (`"sent form" = "sent from"`).
3. **Developer Prose Hygiene — Markdown Backtick Immunity (`` `...` ``):**
   - Automatically suspends autocorrect inside inline `` `code` `` and fenced ` ``` ` blocks in Discord, Slack, and GitHub PRs so `` `im` `` and `` `teh_var` `` are never mangled.
4. **Decoupled Architecture (`smartkey` Standalone + `fcitx5-lotus` Bilingual Bridge):**
   - **Phase 1 (Repo 1 — `smartkey`):** Pure, lightweight English core + `fcitx5` addon for any Linux English user, exporting `smartkey_public.h`.
   - **Phase 2 (`fcitx5-lotus` integration):** Seamless 24/7 bilingual VI+EN mode adding **English-Sentence Momentum Shield** (stopping `it is` → `it í` and `she has` → `she há`) and **1-Backspace Telex Immunity Learning** (`ảu` → `aur`, `ó` → `os` learned automatically into `rules.toml`).
   - **Phase 3 (`HeliBoard` Android):** Same Rust `smartkey-core` + `rules.toml` format over JNI.
