use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA, SINK_STATE};

use crate::keyboard::ascii_letter_mask;
use crate::types::Dialect;

pub const MAGIC_V1: &[u8; 8] = b"SKDICT01";
pub const MAGIC_V2: &[u8; 8] = b"SKDICT02";

pub const FLAG_IN_US: u8 = 0x01;
pub const FLAG_IN_GB: u8 = 0x02;
pub const FLAG_OFFENSIVE: u8 = 0x04;
pub const FLAG_TECH: u8 = 0x08;

/// Unpacked metadata stored in `words_fst` for every known lowercase word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordMeta {
    pub us_freq: u8,
    pub gb_freq: u8,
    pub flags: u8,
    pub word_id: u16,
}

impl WordMeta {
    #[inline]
    pub fn pack(self) -> u64 {
        (self.us_freq as u64)
            | ((self.gb_freq as u64) << 8)
            | ((self.flags as u64) << 16)
            | ((self.word_id as u64) << 24)
    }

    #[inline]
    pub fn unpack(v: u64) -> Self {
        Self {
            us_freq: (v & 0xFF) as u8,
            gb_freq: ((v >> 8) & 0xFF) as u8,
            flags: ((v >> 16) & 0xFF) as u8,
            word_id: ((v >> 24) & 0xFFFF) as u16,
        }
    }

    #[inline]
    pub fn is_offensive(self) -> bool {
        (self.flags & FLAG_OFFENSIVE) != 0
    }

    #[inline]
    pub fn is_tech(self) -> bool {
        (self.flags & FLAG_TECH) != 0
    }

    #[inline]
    pub fn effective_freq(self, dialect: Dialect) -> u8 {
        match dialect {
            Dialect::EnUs => {
                if self.us_freq > 0 {
                    self.us_freq
                } else {
                    self.gb_freq.saturating_sub(25)
                }
            }
            Dialect::EnGb => {
                if self.gb_freq > 0 {
                    self.gb_freq
                } else {
                    self.us_freq.saturating_sub(25)
                }
            }
            Dialect::Mixed => self.us_freq.max(self.gb_freq),
        }
    }

    #[inline]
    pub fn is_valid_for_dialect(self, dialect: Dialect) -> bool {
        if self.is_tech() || self.is_offensive() {
            return true;
        }
        match dialect {
            Dialect::Mixed => (self.flags & (FLAG_IN_US | FLAG_IN_GB)) != 0,
            Dialect::EnUs => (self.flags & FLAG_IN_US) != 0,
            Dialect::EnGb => (self.flags & FLAG_IN_GB) != 0,
        }
    }
}

/// Entry in the `POPCNT` bitmask smash-rescue index for long words (`len >= 7`).
#[derive(Debug, Clone)]
pub struct SmashEntry {
    pub mask: u32,
    pub len: u8,
    pub freq: u8,
    pub word: Box<str>,
}

#[derive(Debug)]
pub enum LexiconError {
    InvalidMagic,
    TruncatedData,
    Fst(fst::Error),
    Utf8(std::str::Utf8Error),
}

impl fmt::Display for LexiconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid .skdict magic header"),
            Self::TruncatedData => write!(f, "truncated .skdict binary image"),
            Self::Fst(e) => write!(f, "fst error: {e}"),
            Self::Utf8(e) => write!(f, "utf-8 error: {e}"),
        }
    }
}

impl std::error::Error for LexiconError {}

impl From<fst::Error> for LexiconError {
    fn from(e: fst::Error) -> Self {
        Self::Fst(e)
    }
}

/// Adapter implementing `fst::Automaton` directly over Tantivy's `levenshtein_automata::DFA`
/// (with Damerau transpositions enabled).
struct DfaAutomaton<'a>(&'a DFA);

impl fst::Automaton for DfaAutomaton<'_> {
    type State = u32;

    #[inline]
    fn start(&self) -> u32 {
        self.0.initial_state()
    }

    #[inline]
    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }

    #[inline]
    fn can_match(&self, state: &u32) -> bool {
        *state != SINK_STATE
    }

    #[inline]
    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

/// Loaded English lexicon, known-smash table, dialect mapper, and bigram scorer.
pub struct EnglishLexicon {
    words_fst: Map<Cow<'static, [u8]>>,
    typos_fst: Map<Cow<'static, [u8]>>,
    typo_pool: Cow<'static, [u8]>,
    uk_to_us: Vec<(Box<str>, Box<str>)>,
    us_to_uk: Vec<(Box<str>, Box<str>)>,
    proper_nouns: Vec<(Box<str>, Box<str>)>,
    bigram_keys: Vec<u32>,
    bigram_scores: Vec<u8>,
    smash_entries: Vec<SmashEntry>,
    lev1: LevenshteinAutomatonBuilder,
    lev2: LevenshteinAutomatonBuilder,
}

impl EnglishLexicon {
    /// Loads the bundled English `.skdict` dictionary when compiled with `feature = "bundled-en"`.
    #[cfg(feature = "bundled-en")]
    pub fn from_bundled() -> Result<Self, LexiconError> {
        static BUNDLED: &[u8] = include_bytes!("../assets/en_lexicon.skdict");
        Self::from_static_bytes(BUNDLED)
    }

    /// Zero-copy load over a `'static` byte slice (`include_bytes!` or `mmap`).
    pub fn from_static_bytes(bytes: &'static [u8]) -> Result<Self, LexiconError> {
        Self::parse_image(bytes, true)
    }

    /// Loads from an owned byte buffer (e.g. Android asset stream or custom file).
    pub fn from_owned_bytes(bytes: Vec<u8>) -> Result<Self, LexiconError> {
        Self::parse_image(&bytes, false)
    }

    fn parse_image(bytes: &[u8], is_static: bool) -> Result<Self, LexiconError> {
        let mut cursor = 0usize;
        if bytes.len() < 8 {
            return Err(LexiconError::TruncatedData);
        }
        let version = match &bytes[..8] {
            b"SKDICT01" => 1,
            b"SKDICT02" => 2,
            _ => return Err(LexiconError::InvalidMagic),
        };
        cursor += 8;

        let read_u32 = |buf: &[u8], pos: &mut usize| -> Result<u32, LexiconError> {
            if *pos + 4 > buf.len() {
                return Err(LexiconError::TruncatedData);
            }
            let val = u32::from_le_bytes(buf[*pos..*pos + 4].try_into().unwrap());
            *pos += 4;
            Ok(val)
        };

        let read_slice = |buf: &[u8], pos: &mut usize| -> Result<(usize, usize), LexiconError> {
            let len = read_u32(buf, pos)? as usize;
            if *pos + len > buf.len() {
                return Err(LexiconError::TruncatedData);
            }
            let start = *pos;
            *pos += len;
            Ok((start, start + len))
        };

        let (wf_start, wf_end) = read_slice(bytes, &mut cursor)?;
        let (tf_start, tf_end) = read_slice(bytes, &mut cursor)?;
        let (tp_start, tp_end) = read_slice(bytes, &mut cursor)?;

        // UK -> US pairs
        let uk_count = read_u32(bytes, &mut cursor)? as usize;
        let mut uk_to_us = Vec::with_capacity(uk_count);
        let mut us_to_uk = Vec::with_capacity(uk_count);
        for _ in 0..uk_count {
            let (ks, ke) = read_slice(bytes, &mut cursor)?;
            let (vs, ve) = read_slice(bytes, &mut cursor)?;
            let uk = std::str::from_utf8(&bytes[ks..ke])
                .map_err(LexiconError::Utf8)?
                .to_string()
                .into_boxed_str();
            let us = std::str::from_utf8(&bytes[vs..ve])
                .map_err(LexiconError::Utf8)?
                .to_string()
                .into_boxed_str();
            us_to_uk.push((us.clone(), uk.clone()));
            uk_to_us.push((uk, us));
        }
        uk_to_us.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        us_to_uk.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        us_to_uk.dedup_by(|a, b| a.0 == b.0);

        // Bigrams
        let bg_count = read_u32(bytes, &mut cursor)? as usize;
        if cursor + bg_count * 5 > bytes.len() {
            return Err(LexiconError::TruncatedData);
        }
        let mut bigram_keys = Vec::with_capacity(bg_count);
        let mut bigram_scores = Vec::with_capacity(bg_count);
        for _ in 0..bg_count {
            let key = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
            let score = bytes[cursor + 4];
            cursor += 5;
            bigram_keys.push(key);
            bigram_scores.push(score);
        }

        // Smash rescue entries
        let smash_count = read_u32(bytes, &mut cursor)? as usize;
        let mut smash_entries = Vec::with_capacity(smash_count);
        for _ in 0..smash_count {
            if cursor + 6 > bytes.len() {
                return Err(LexiconError::TruncatedData);
            }
            let mask = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap());
            let len = bytes[cursor + 4];
            let freq = bytes[cursor + 5];
            cursor += 6;
            let (ws, we) = read_slice(bytes, &mut cursor)?;
            let word = std::str::from_utf8(&bytes[ws..we])
                .map_err(LexiconError::Utf8)?
                .to_string()
                .into_boxed_str();
            smash_entries.push(SmashEntry {
                mask,
                len,
                freq,
                word,
            });
        }

        let mut proper_nouns = Vec::new();
        if version >= 2 && cursor < bytes.len() {
            let pn_count = read_u32(bytes, &mut cursor)? as usize;
            for _ in 0..pn_count {
                let (ks, ke) = read_slice(bytes, &mut cursor)?;
                let (vs, ve) = read_slice(bytes, &mut cursor)?;
                let lower = std::str::from_utf8(&bytes[ks..ke])
                    .map_err(LexiconError::Utf8)?
                    .to_string()
                    .into_boxed_str();
                let proper = std::str::from_utf8(&bytes[vs..ve])
                    .map_err(LexiconError::Utf8)?
                    .to_string()
                    .into_boxed_str();
                proper_nouns.push((lower, proper));
            }
            proper_nouns.sort_unstable_by(|a, b| a.0.cmp(&b.0));
            proper_nouns.dedup_by(|a, b| a.0 == b.0);
        }

        let (words_cow, typos_cow, pool_cow) = if is_static {
            // Safe static subslice reconstruction via pointer math unneeded if called from `from_static_bytes`;
            // instead, pass Owned for `from_owned_bytes` and Borrowed via helper in `from_static_bytes_inner`.
            (
                Cow::Owned(bytes[wf_start..wf_end].to_vec()),
                Cow::Owned(bytes[tf_start..tf_end].to_vec()),
                Cow::Owned(bytes[tp_start..tp_end].to_vec()),
            )
        } else {
            (
                Cow::Owned(bytes[wf_start..wf_end].to_vec()),
                Cow::Owned(bytes[tf_start..tf_end].to_vec()),
                Cow::Owned(bytes[tp_start..tp_end].to_vec()),
            )
        };
        let _ = is_static;

        Ok(Self {
            words_fst: Map::new(words_cow)?,
            typos_fst: Map::new(typos_cow)?,
            typo_pool: pool_cow,
            uk_to_us,
            us_to_uk,
            proper_nouns,
            bigram_keys,
            bigram_scores,
            smash_entries,
            lev1: LevenshteinAutomatonBuilder::new(1, true),
            lev2: LevenshteinAutomatonBuilder::new(2, true),
        })
    }

    #[inline]
    pub fn lookup_word(&self, lower_word: &str) -> Option<WordMeta> {
        self.words_fst.get(lower_word).map(WordMeta::unpack)
    }

    #[inline]
    pub fn contains_valid(&self, lower_word: &str, dialect: Dialect) -> bool {
        self.lookup_word(lower_word)
            .is_some_and(|m| m.is_valid_for_dialect(dialect))
    }

    #[inline]
    pub fn contains_any_english(&self, lower_word: &str) -> bool {
        self.lookup_word(lower_word)
            .is_some_and(|m| m.is_valid_for_dialect(Dialect::Mixed))
    }

    /// Maps a word to the preferred dialect if a UK<->US counterpart exists.
    pub fn map_dialect<'a>(&'a self, lower_word: &'a str, dialect: Dialect) -> &'a str {
        match dialect {
            Dialect::Mixed | Dialect::EnUs => {
                if let Ok(idx) = self
                    .uk_to_us
                    .binary_search_by(|(uk, _)| uk.as_ref().cmp(lower_word))
                {
                    &self.uk_to_us[idx].1
                } else {
                    lower_word
                }
            }
            Dialect::EnGb => {
                if let Ok(idx) = self
                    .us_to_uk
                    .binary_search_by(|(us, _)| us.as_ref().cmp(lower_word))
                {
                    &self.us_to_uk[idx].1
                } else {
                    lower_word
                }
            }
        }
    }

    /// Strictly checks if `lower_word` requires cross-dialect rewriting under `EnUs` or `EnGb`.
    pub fn strict_dialect_rewrite(&self, lower_word: &str, dialect: Dialect) -> Option<&str> {
        match dialect {
            Dialect::Mixed => None,
            Dialect::EnUs => self
                .uk_to_us
                .binary_search_by(|(uk, _)| uk.as_ref().cmp(lower_word))
                .ok()
                .map(|i| self.uk_to_us[i].1.as_ref()),
            Dialect::EnGb => self
                .us_to_uk
                .binary_search_by(|(us, _)| us.as_ref().cmp(lower_word))
                .ok()
                .map(|i| self.us_to_uk[i].1.as_ref()),
        }
    }

    /// Looks up curated human keyboard smashes (`crate-ci/typos` `words.csv`).
    pub fn lookup_known_typos(&self, lower_word: &str) -> Option<impl Iterator<Item = &str>> {
        let packed = self.typos_fst.get(lower_word)?;
        let offset = (packed & 0xFFFF_FFFF) as usize;
        let len = (packed >> 32) as usize;
        let slice = std::str::from_utf8(&self.typo_pool[offset..offset + len]).ok()?;
        Some(slice.split('\n').filter(|s| !s.is_empty()))
    }

    /// Returns normalized bigram boost `0.0..=1.0` for `(prev_lower, cand_lower)`.
    #[inline]
    pub fn bigram_score(&self, prev_lower: &str, cand_lower: &str) -> f32 {
        let Some(pm) = self.lookup_word(prev_lower) else {
            return 0.0;
        };
        let Some(cm) = self.lookup_word(cand_lower) else {
            return 0.0;
        };
        self.bigram_score_ids(pm.word_id, cm.word_id)
    }

    #[inline]
    pub fn bigram_score_ids(&self, prev_id: u16, cand_id: u16) -> f32 {
        if prev_id == 0 || cand_id == 0 {
            return 0.0;
        }
        let key = ((prev_id as u32) << 16) | (cand_id as u32);
        match self.bigram_keys.binary_search(&key) {
            Ok(idx) => self.bigram_scores[idx] as f32 / 255.0,
            Err(_) => 0.0,
        }
    }

    /// Queries `words_fst` using a Damerau–Levenshtein DFA (`max_dist = 1` or `2`).
    pub fn search_fuzzy(&self, query_lower: &str, max_dist: u8, out: &mut Vec<(String, WordMeta)>) {
        let dfa = if max_dist <= 1 {
            self.lev1.build_dfa(query_lower)
        } else {
            self.lev2.build_dfa(query_lower)
        };
        let aut = DfaAutomaton(&dfa);
        let mut stream = self.words_fst.search(aut).into_stream();
        while let Some((k, v)) = stream.next() {
            let meta = WordMeta::unpack(v);
            if meta.is_offensive() {
                continue;
            }
            if meta.us_freq == 0 && meta.gb_freq == 0 {
                continue;
            }
            if let Ok(w) = std::str::from_utf8(k) {
                out.push((w.to_string(), meta));
            }
        }
    }

    #[inline]
    pub fn smash_entries(&self) -> &[SmashEntry] {
        &self.smash_entries
    }

    #[inline]
    pub fn lookup_proper_noun(&self, lower_word: &str) -> Option<&str> {
        self.proper_nouns
            .binary_search_by(|(lower, _)| lower.as_ref().cmp(lower_word))
            .ok()
            .map(|idx| self.proper_nouns[idx].1.as_ref())
    }

    #[inline]
    pub fn proper_nouns(&self) -> &[(Box<str>, Box<str>)] {
        &self.proper_nouns
    }

    /// Builds an in-memory minimal test lexicon from word lists (used when `.skdict` is not bundled).
    pub fn from_word_list(
        words: &[(&str, u8)],
        typos: &[(&str, &str)],
        bigrams: &[(&str, &str, u8)],
    ) -> Result<Self, LexiconError> {
        Self::from_word_list_with_proper(words, typos, &[], bigrams, &[])
    }

    pub fn from_word_list_with_proper(
        words: &[(&str, u8)],
        typos: &[(&str, &str)],
        uk_pairs: &[(&str, &str)],
        bigrams: &[(&str, &str, u8)],
        proper_nouns: &[(&str, &str)],
    ) -> Result<Self, LexiconError> {
        let bytes = compile_skdict_in_memory(words, typos, uk_pairs, bigrams, proper_nouns);
        Self::from_owned_bytes(bytes)
    }
}

pub fn resolve_dict_path(lang: &str) -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("SMARTKEY_DICT_DIR") {
        if !dir.trim().is_empty() {
            let p = std::path::PathBuf::from(dir).join(format!("{lang}.skdict"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.trim().is_empty() {
            let p = std::path::PathBuf::from(xdg)
                .join("smartkey/dicts")
                .join(format!("{lang}.skdict"));
            if p.exists() {
                return Some(p);
            }
        }
    } else if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            let p = std::path::PathBuf::from(home)
                .join(".local/share/smartkey/dicts")
                .join(format!("{lang}.skdict"));
            if p.exists() {
                return Some(p);
            }
        }
    }
    for sys in [
        "/usr/local/share/smartkey/dicts",
        "/usr/share/smartkey/dicts",
    ] {
        let p = std::path::PathBuf::from(sys).join(format!("{lang}.skdict"));
        if p.exists() {
            return Some(p);
        }
    }
    None
}

pub fn default_user_dict_dir() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.trim().is_empty() {
            return std::path::PathBuf::from(xdg).join("smartkey/dicts");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            return std::path::PathBuf::from(home).join(".local/share/smartkey/dicts");
        }
    }
    std::path::PathBuf::from(".smartkey/dicts")
}

/// Compiles raw AOSP `en_US.combined`, `en_GB.combined`, `dev_terms.txt`, `slang.txt`,
/// `proper_nouns.tsv`, `typos_words.csv`, `british_to_american.json`, and `count_2w.txt` into a compact `.skdict` binary image.
#[allow(clippy::too_many_arguments)]
pub fn compile_skdict_from_sources(
    en_us_combined: &str,
    en_gb_combined: &str,
    dev_terms: &str,
    slang_terms: &str,
    proper_nouns_tsv: &str,
    typos_csv: &str,
    british_to_american_json: &str,
    count_2w_tsv: &str,
) -> Vec<u8> {
    #[derive(Default, Clone, Copy)]
    struct RawWord {
        us_freq: u8,
        gb_freq: u8,
        flags: u8,
    }

    let mut words: BTreeMap<String, RawWord> = BTreeMap::new();
    let mut aosp_bigrams: Vec<(String, String, u8)> = Vec::new();

    let mut parse_aosp = |content: &str, lang_flag: u8, capture_bigrams: bool| {
        let mut current_word: Option<String> = None;
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("word=") {
                current_word = None;
                let mut raw_w = "";
                let mut freq: u8 = 0;
                let mut not_a_word = false;
                let mut offensive = false;
                let mut is_abbrev = false;

                for part in rest.split(',') {
                    if raw_w.is_empty() && !part.contains('=') {
                        raw_w = part;
                    } else if let Some(val) = part.strip_prefix("f=") {
                        freq = val.parse::<u8>().unwrap_or(0);
                    } else if part == "not_a_word=true" {
                        not_a_word = true;
                    } else if part == "possibly_offensive=true" {
                        offensive = true;
                    } else if let Some(fl) = part.strip_prefix("flags=") {
                        if fl.contains("abbreviation") {
                            is_abbrev = true;
                        }
                    }
                }

                if raw_w.is_empty() || not_a_word {
                    continue;
                }

                // Skip ALL_CAPS abbreviations (`NCAA`, `FIFA`, `DVD`) from polluting lowercase typos;
                // ALL_CAPS user tokens are already bypassed by `casing::analyze_token`.
                let alpha: Vec<char> = raw_w.chars().filter(|c| c.is_alphabetic()).collect();
                if alpha.is_empty() {
                    continue;
                }
                if (is_abbrev || alpha.len() >= 2) && alpha.iter().all(|c| c.is_uppercase()) {
                    continue;
                }

                // Allow pure lowercase or clean TitleCase (`English`, `Monday`) normalized to lowercase.
                if !raw_w
                    .chars()
                    .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
                {
                    continue;
                }
                if raw_w.contains('-') {
                    continue;
                }

                let lower = raw_w.to_lowercase();
                let entry = words.entry(lower.clone()).or_default();
                if offensive || freq == 0 {
                    entry.flags |= FLAG_OFFENSIVE;
                } else {
                    entry.flags |= lang_flag;
                    if lang_flag == FLAG_IN_US {
                        entry.us_freq = entry.us_freq.max(freq);
                    } else {
                        entry.gb_freq = entry.gb_freq.max(freq);
                    }
                    if capture_bigrams {
                        current_word = Some(lower);
                    }
                }
            } else if capture_bigrams {
                if let (Some(prev), Some(rest)) =
                    (current_word.as_ref(), trimmed.strip_prefix("bigram="))
                {
                    let mut target = "";
                    let mut rank: u8 = 3;
                    for part in rest.split(',') {
                        if target.is_empty() && !part.contains('=') {
                            target = part;
                        } else if let Some(fval) = part.strip_prefix("f=") {
                            rank = fval.parse::<u8>().unwrap_or(3);
                        }
                    }
                    if !target.is_empty() {
                        // Note: in AOSP `.combined`, f=1 is top rank, f=3 is 3rd rank!
                        let score = match rank {
                            1 => 210,
                            2 => 185,
                            _ => 160,
                        };
                        aosp_bigrams.push((prev.clone(), target.to_lowercase(), score));
                    }
                }
            }
        }
    };

    parse_aosp(en_us_combined, FLAG_IN_US, true);
    parse_aosp(en_gb_combined, FLAG_IN_GB, false);

    // Merge curated developer / Linux / CLI vocabulary shield (`dev_terms.txt`)
    for line in dev_terms.lines() {
        let w = line.trim();
        if w.is_empty() || w.starts_with('#') {
            continue;
        }
        let lower = w.to_lowercase();
        let entry = words.entry(lower).or_default();
        entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_TECH | FLAG_IN_US | FLAG_IN_GB;
        entry.us_freq = entry.us_freq.max(172);
        entry.gb_freq = entry.gb_freq.max(172);
    }

    // Merge curated internet abbreviations & slang shield (`slang.txt`)
    for line in slang_terms.lines() {
        let w = line.trim();
        if w.is_empty() || w.starts_with('#') {
            continue;
        }
        let lower = w.to_lowercase();
        let entry = words.entry(lower).or_default();
        entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_TECH | FLAG_IN_US | FLAG_IN_GB;
        entry.us_freq = entry.us_freq.max(185);
        entry.gb_freq = entry.gb_freq.max(185);
    }

    // Parse curated unambiguous proper nouns (`proper_nouns.tsv`)
    let mut proper_nouns: Vec<(String, String)> = Vec::new();
    for line in proper_nouns_tsv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lower, proper) = if let Some((l, p)) = line.split_once('\t') {
            (l.trim().to_lowercase(), p.trim().to_string())
        } else {
            (line.to_lowercase(), line.to_string())
        };
        if lower.is_empty() || proper.is_empty() || lower == proper {
            continue;
        }
        let entry = words.entry(lower.clone()).or_default();
        entry.flags = (entry.flags & !FLAG_OFFENSIVE) | FLAG_IN_US | FLAG_IN_GB;
        entry.us_freq = entry.us_freq.max(160);
        entry.gb_freq = entry.gb_freq.max(160);

        proper_nouns.push((lower, proper));
    }
    proper_nouns.sort_unstable();
    proper_nouns.dedup_by(|a, b| a.0 == b.0);

    // Parse UK -> US spelling map (`british_to_american.json`)
    let raw_uk_map: BTreeMap<String, String> =
        serde_json::from_str(british_to_american_json).unwrap_or_default();
    let mut uk_pairs: Vec<(String, String)> = Vec::new();
    for (uk_raw, us_raw) in raw_uk_map {
        let uk = uk_raw.to_lowercase();
        let us = us_raw.to_lowercase();
        if uk == us || uk.contains(' ') || us.contains(' ') || uk.contains('-') || us.contains('-')
        {
            continue;
        }
        // Propagate frequencies so both dialect forms are first-class valid words in their dialect
        let us_f = words.get(&us).map(|w| w.us_freq).unwrap_or(0);
        let gb_f = words.get(&uk).map(|w| w.gb_freq).unwrap_or(0);
        let shared_f = us_f.max(gb_f).max(120);
        {
            let uk_entry = words.entry(uk.clone()).or_default();
            uk_entry.flags |= FLAG_IN_GB;
            uk_entry.gb_freq = uk_entry.gb_freq.max(shared_f);
        }
        {
            let us_entry = words.entry(us.clone()).or_default();
            us_entry.flags |= FLAG_IN_US;
            us_entry.us_freq = us_entry.us_freq.max(shared_f);
        }
        uk_pairs.push((uk, us));
    }
    uk_pairs.sort_unstable();
    uk_pairs.dedup_by(|a, b| a.0 == b.0);

    // Assign `word_id` (`1..=65535`) to the top 65,535 words by max frequency
    let mut freq_rank: Vec<(&String, u8)> = words
        .iter()
        .filter(|(_, m)| (m.flags & FLAG_OFFENSIVE) == 0 && m.us_freq.max(m.gb_freq) > 0)
        .map(|(w, m)| (w, m.us_freq.max(m.gb_freq)))
        .collect();
    freq_rank.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));

    let mut word_ids: HashMap<String, u16> = HashMap::with_capacity(65535);
    for (idx, (w, _)) in freq_rank.into_iter().take(65535).enumerate() {
        word_ids.insert(w.clone(), (idx + 1) as u16);
    }

    // Build `words_fst` and `smash_entries`
    let mut words_builder = MapBuilder::memory();
    let mut smash_entries: Vec<(u32, u8, u8, String)> = Vec::new();
    for (w, m) in &words {
        let wid = word_ids.get(w).copied().unwrap_or(0);
        let meta = WordMeta {
            us_freq: m.us_freq,
            gb_freq: m.gb_freq,
            flags: m.flags,
            word_id: wid,
        };
        words_builder.insert(w.as_bytes(), meta.pack()).unwrap();

        let max_f = m.us_freq.max(m.gb_freq);
        if (m.flags & FLAG_OFFENSIVE) == 0
            && w.is_ascii()
            && !w.contains('\'')
            && (7..=16).contains(&w.len())
            && max_f >= 105
        {
            smash_entries.push((ascii_letter_mask(w), w.len() as u8, max_f, w.clone()));
        }
    }
    let words_fst_bytes = words_builder.into_inner().unwrap();

    // Parse `typos_words.csv` (`crate-ci/typos`), strictly skipping any valid English or tech word!
    let mut typos_map: BTreeMap<String, String> = BTreeMap::new();
    for line in typos_csv.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split(',');
        let Some(raw_typo) = parts.next() else {
            continue;
        };
        let typo = raw_typo.trim().to_lowercase();
        if typo.len() < 3 || !typo.chars().all(|c| c.is_alphabetic()) {
            continue;
        }
        // NEVER override a valid dictionary/tech/offensive word in the unconditional typo map!
        if let Some(existing) = words.get(&typo) {
            if (existing.flags & (FLAG_IN_US | FLAG_IN_GB | FLAG_TECH | FLAG_OFFENSIVE)) != 0 {
                continue;
            }
        }
        let targets: Vec<&str> = parts
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.contains(' '))
            .collect();
        if targets.is_empty() {
            continue;
        }
        typos_map.insert(typo, targets.join("\n"));
    }

    // Curated morphological & motor overrides where upstream `crate-ci/typos` has wrong/missing mappings
    let curated_typo_overrides: &[(&str, &str)] = &[
        ("preventation", "prevention"),
        ("apdater", "adapter"),
        ("apdapter", "adapter"),
    ];
    for &(bad, good) in curated_typo_overrides {
        typos_map.insert(bad.to_string(), good.to_string());
    }

    let mut typos_builder = MapBuilder::memory();
    let mut typo_pool: Vec<u8> = Vec::new();
    for (typo, joined_targets) in &typos_map {
        let offset = typo_pool.len() as u64;
        let len = joined_targets.len() as u64;
        typo_pool.extend_from_slice(joined_targets.as_bytes());
        let packed = offset | (len << 32);
        typos_builder.insert(typo.as_bytes(), packed).unwrap();
    }
    let typos_fst_bytes = typos_builder.into_inner().unwrap();

    // Build calibrated prose bigrams from `count_2w.txt` + AOSP bigrams + locative boosts
    let mut bigram_map: BTreeMap<u32, u8> = BTreeMap::new();

    for (p, n, sc) in aosp_bigrams {
        if let (Some(&pid), Some(&nid)) = (word_ids.get(&p), word_ids.get(&n)) {
            let key = ((pid as u32) << 16) | (nid as u32);
            let e = bigram_map.entry(key).or_insert(0);
            *e = (*e).max(sc);
        }
    }

    // Parse Norvig `count_2w.txt` (`<w1> <w2>\t<count>`)
    // Aggregate case-insensitively first so `In their` + `in their` sum cleanly!
    let mut raw_counts: HashMap<u32, u64> = HashMap::new();
    for line in count_2w_tsv.lines() {
        let Some((pair, count_str)) = line.split_once('\t') else {
            continue;
        };
        let Some((w1, w2)) = pair.split_once(' ') else {
            continue;
        };
        let Ok(cnt) = count_str.trim().parse::<u64>() else {
            continue;
        };
        let l1 = w1.to_lowercase();
        let l2 = w2.to_lowercase();
        if let (Some(&pid), Some(&nid)) = (word_ids.get(&l1), word_ids.get(&l2)) {
            let key = ((pid as u32) << 16) | (nid as u32);
            *raw_counts.entry(key).or_insert(0) += cnt;
        }
    }

    // Keep top 90,000 prose bigrams quantized to log-scale `u8` (`80..=255`)
    let mut ranked_bg: Vec<(u32, u64)> = raw_counts.into_iter().collect();
    ranked_bg.sort_unstable_by_key(|a| std::cmp::Reverse(a.1));
    for (key, cnt) in ranked_bg.into_iter().take(90_000) {
        // `cnt` typically ranges from ~80,000 (`ln ≈ 11.3`) to ~1,000,000,000 (`ln ≈ 20.7`)
        let ln_val = (cnt as f64).ln();
        let norm = ((ln_val - 10.5) / 10.0).clamp(0.0, 1.0);
        let q = (80.0 + norm * 175.0).round() as u8;
        let e = bigram_map.entry(key).or_insert(0);
        *e = (*e).max(q);
    }

    // Curated spoken/chat locative & grammar disambiguators where raw web 2-grams have right-context bias
    let curated_boosts: &[(&str, &str, u8)] = &[
        ("over", "there", 248),
        ("out", "there", 245),
        ("right", "there", 245),
        ("up", "there", 242),
        ("down", "there", 242),
        ("in", "their", 248),
        ("of", "their", 245),
        ("for", "their", 242),
        ("with", "their", 242),
        ("too", "much", 250),
        ("so", "much", 248),
        ("very", "much", 248),
        ("sent", "from", 245),
        ("away", "from", 245),
        ("apart", "from", 245),
    ];
    for &(p, n, sc) in curated_boosts {
        if let (Some(&pid), Some(&nid)) = (word_ids.get(p), word_ids.get(n)) {
            let key = ((pid as u32) << 16) | (nid as u32);
            bigram_map.insert(key, sc);
        }
    }

    serialize_skdict(
        &words_fst_bytes,
        &typos_fst_bytes,
        &typo_pool,
        &uk_pairs,
        &bigram_map,
        &smash_entries,
        &proper_nouns,
    )
}

fn compile_skdict_in_memory(
    words: &[(&str, u8)],
    typos: &[(&str, &str)],
    uk_pairs: &[(&str, &str)],
    bigrams: &[(&str, &str, u8)],
    proper_nouns: &[(&str, &str)],
) -> Vec<u8> {
    let mut sorted_words: BTreeMap<String, u8> = BTreeMap::new();
    for &(w, f) in words {
        sorted_words.insert(w.to_lowercase(), f);
    }
    let mut word_ids: HashMap<String, u16> = HashMap::new();
    for (idx, w) in sorted_words.keys().take(65535).enumerate() {
        word_ids.insert(w.clone(), (idx + 1) as u16);
    }

    let mut wb = MapBuilder::memory();
    let mut smash_entries = Vec::new();
    for (w, &f) in &sorted_words {
        let wid = word_ids.get(w).copied().unwrap_or(0);
        let meta = WordMeta {
            us_freq: f,
            gb_freq: f,
            flags: FLAG_IN_US | FLAG_IN_GB,
            word_id: wid,
        };
        wb.insert(w.as_bytes(), meta.pack()).unwrap();
        if w.is_ascii() && w.len() >= 7 && f >= 100 {
            smash_entries.push((ascii_letter_mask(w), w.len() as u8, f, w.clone()));
        }
    }

    let mut sorted_typos: BTreeMap<String, String> = BTreeMap::new();
    for &(t, target) in typos {
        sorted_typos.insert(t.to_lowercase(), target.to_string());
    }
    let mut tb = MapBuilder::memory();
    let mut pool = Vec::new();
    for (t, target) in &sorted_typos {
        let off = pool.len() as u64;
        let len = target.len() as u64;
        pool.extend_from_slice(target.as_bytes());
        tb.insert(t.as_bytes(), off | (len << 32)).unwrap();
    }

    let uk_vec: Vec<(String, String)> = uk_pairs
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();

    let mut bg_map: BTreeMap<u32, u8> = BTreeMap::new();
    for &(p, n, sc) in bigrams {
        if let (Some(&pid), Some(&nid)) = (
            word_ids.get(&p.to_lowercase()),
            word_ids.get(&n.to_lowercase()),
        ) {
            bg_map.insert(((pid as u32) << 16) | (nid as u32), sc);
        }
    }

    let pn_vec: Vec<(String, String)> = proper_nouns
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();

    serialize_skdict(
        &wb.into_inner().unwrap(),
        &tb.into_inner().unwrap(),
        &pool,
        &uk_vec,
        &bg_map,
        &smash_entries,
        &pn_vec,
    )
}

fn serialize_skdict(
    words_fst: &[u8],
    typos_fst: &[u8],
    typo_pool: &[u8],
    uk_pairs: &[(String, String)],
    bigrams: &BTreeMap<u32, u8>,
    smash_entries: &[(u32, u8, u8, String)],
    proper_nouns: &[(String, String)],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        words_fst.len() + typos_fst.len() + typo_pool.len() + bigrams.len() * 5 + 65536,
    );
    out.extend_from_slice(MAGIC_V2);

    let write_slice = |buf: &mut Vec<u8>, data: &[u8]| {
        buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
        buf.extend_from_slice(data);
    };

    write_slice(&mut out, words_fst);
    write_slice(&mut out, typos_fst);
    write_slice(&mut out, typo_pool);

    out.extend_from_slice(&(uk_pairs.len() as u32).to_le_bytes());
    for (uk, us) in uk_pairs {
        write_slice(&mut out, uk.as_bytes());
        write_slice(&mut out, us.as_bytes());
    }

    out.extend_from_slice(&(bigrams.len() as u32).to_le_bytes());
    for (&key, &score) in bigrams {
        out.extend_from_slice(&key.to_le_bytes());
        out.push(score);
    }

    out.extend_from_slice(&(smash_entries.len() as u32).to_le_bytes());
    for (mask, len, freq, word) in smash_entries {
        out.extend_from_slice(&mask.to_le_bytes());
        out.push(*len);
        out.push(*freq);
        write_slice(&mut out, word.as_bytes());
    }

    out.extend_from_slice(&(proper_nouns.len() as u32).to_le_bytes());
    for (lower, proper) in proper_nouns {
        write_slice(&mut out, lower.as_bytes());
        write_slice(&mut out, proper.as_bytes());
    }

    out
}
