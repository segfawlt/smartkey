use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::OnceLock;

use fst::{IntoStreamer, Map, MapBuilder, Streamer};
use levenshtein_automata::{Distance, LevenshteinAutomatonBuilder, DFA, SINK_STATE};
use serde::{Deserialize, Serialize};

use crate::keyboard::ascii_letter_mask;
use crate::types::Dialect;

#[cfg(feature = "bundled-en")]
pub(crate) fn bundled_en_lexicon() -> Option<&'static PackLexicon> {
    static BUNDLED_LEX: OnceLock<Option<PackLexicon>> = OnceLock::new();
    BUNDLED_LEX
        .get_or_init(|| PackLexicon::from_bundled().ok())
        .as_ref()
}

#[cfg(not(feature = "bundled-en"))]
pub(crate) fn bundled_en_lexicon() -> Option<&'static PackLexicon> {
    None
}

/// Looks up a curated unambiguous English shorthand auto-expansion (`pls -> please`, `rn -> right now`)
/// from the bundled v3 English pack's `SHORTHAND` section.
pub fn lookup_english_shorthand(lower_word: &str) -> Option<&'static str> {
    bundled_en_lexicon()?.lookup_shorthand(lower_word)
}

pub const MAGIC_V1: &[u8; 8] = b"SKDICT01";
pub const MAGIC_V2: &[u8; 8] = b"SKDICT02";
pub const MAGIC_V3: &[u8; 8] = b"SKDICT03";

pub const TAG_WORDS_FST: u16 = 0x0001;
pub const TAG_KNOWN_TYPOS: u16 = 0x0002;
pub const TAG_VARIANT_REWRITES: u16 = 0x0003;
pub const TAG_BIGRAMS: u16 = 0x0004;
pub const TAG_SMASH_INDEX: u16 = 0x0005;
pub const TAG_PROPER_NOUNS: u16 = 0x0006;
pub const TAG_SHORTHAND: u16 = 0x0007;
pub const TAG_SLANG_SET: u16 = 0x0008;
pub const TAG_LANGUAGE_RULES: u16 = 0x0009;
pub const TAG_SYLLABLE_DICT: u16 = 0x000A;
pub const TAG_SOURCE_MANIFEST: u16 = 0x000B;

pub const FLAG_OFFENSIVE: u8 = 0x01;
pub const FLAG_SHIELD: u8 = 0x02;
pub const FLAG_TECH: u8 = FLAG_SHIELD;
pub const FLAG_AMBIGUOUS_PROPER: u8 = 0x04;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PackTier {
    Data = 0,
    ImeBridge = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub lang: String,
    pub pack_version: [u8; 3],
    pub tier: PackTier,
    pub variants: Vec<String>,
    pub freq_slots: Vec<u8>,
    pub default_variant_idx: u8,
    pub feature_flags: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceManifestEntry {
    pub name: String,
    pub sha256: [u8; 32],
    pub license: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementKind {
    Contraction,
    TransposedApostrophe,
    UnmergedCompound,
    TypoOverride,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementRule {
    pub typed: String,
    pub fix: String,
    pub kind: ReplacementKind,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CliticRules {
    pub suffixes: Vec<String>,
    pub prefixes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CasingRules {
    pub leading_apostrophes: Vec<String>,
    pub abbreviations: Vec<String>,
    pub exclusions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BigramBoost {
    pub prev: String,
    pub next: String,
    pub score: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LanguageRules {
    pub capitalize_standalone_i: bool,
    pub function_words: Vec<String>,
    pub clitics: CliticRules,
    pub casing: CasingRules,
    pub bigram_boosts: Vec<BigramBoost>,
    pub replacements: Vec<ReplacementRule>,
}

/// Unpacked metadata (`WordMetaV3`) stored in `words_fst` for every known lowercase word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordMeta {
    pub freq0: u8,
    pub freq1: u8,
    pub variant_mask: u8,
    pub flags: u8,
    pub word_id: u16,
}

impl WordMeta {
    #[inline]
    pub fn pack(self) -> u64 {
        (self.freq0 as u64)
            | ((self.freq1 as u64) << 8)
            | ((self.variant_mask as u64) << 16)
            | ((self.flags as u64) << 24)
            | ((self.word_id as u64) << 32)
    }

    #[inline]
    pub fn unpack(v: u64) -> Self {
        Self {
            freq0: (v & 0xFF) as u8,
            freq1: ((v >> 8) & 0xFF) as u8,
            variant_mask: ((v >> 16) & 0xFF) as u8,
            flags: ((v >> 24) & 0xFF) as u8,
            word_id: ((v >> 32) & 0xFFFF) as u16,
        }
    }

    #[inline]
    pub fn unpack_checked(v: u64) -> Result<Self, LexiconError> {
        if (v >> 48) != 0 {
            return Err(LexiconError::CorruptSection("WORDS_FST"));
        }
        Ok(Self::unpack(v))
    }

    #[inline]
    pub fn is_offensive(self) -> bool {
        (self.flags & FLAG_OFFENSIVE) != 0
    }

    #[inline]
    pub fn is_shield(self) -> bool {
        (self.flags & FLAG_SHIELD) != 0
    }

    #[inline]
    pub fn is_tech(self) -> bool {
        self.is_shield()
    }

    #[inline]
    pub fn is_ambiguous_proper(self) -> bool {
        (self.flags & FLAG_AMBIGUOUS_PROPER) != 0
    }

    #[inline]
    pub fn effective_freq(self, dialect: Dialect) -> u8 {
        match dialect {
            Dialect::EnUs => {
                if self.freq0 > 0 {
                    self.freq0
                } else {
                    self.freq1.saturating_sub(25)
                }
            }
            Dialect::EnGb => {
                if self.freq1 > 0 {
                    self.freq1
                } else {
                    self.freq0.saturating_sub(25)
                }
            }
            Dialect::Mixed => self.freq0.max(self.freq1),
        }
    }

    #[inline]
    pub fn effective_freq_for_pack(self, variant_count: usize, dialect: Dialect) -> u8 {
        if variant_count == 1 {
            self.freq0
        } else {
            self.effective_freq(dialect)
        }
    }

    #[inline]
    pub fn is_valid_for_dialect(self, dialect: Dialect) -> bool {
        if self.is_tech() || self.is_offensive() {
            return true;
        }
        match dialect {
            Dialect::Mixed => self.variant_mask != 0,
            Dialect::EnUs => (self.variant_mask & 0x01) != 0,
            Dialect::EnGb => (self.variant_mask & 0x02) != 0,
        }
    }

    #[inline]
    pub fn is_valid_for_pack(self, variant_count: usize, dialect: Dialect) -> bool {
        if variant_count == 1 {
            (self.variant_mask & 1) != 0 || self.is_tech() || self.is_offensive()
        } else {
            self.is_valid_for_dialect(dialect)
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
    LegacyFormat(u8),
    LanguageMismatch { expected: String, found: String },
    CorruptSection(&'static str),
    TruncatedData,
    Fst(fst::Error),
    Utf8(std::str::Utf8Error),
}

impl fmt::Display for LexiconError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid .skdict magic header"),
            Self::LegacyFormat(v) => write!(
                f,
                "legacy .skdict v{v} format is unsupported (run: smartkey dict update)"
            ),
            Self::LanguageMismatch { expected, found } => write!(
                f,
                "dictionary pack language mismatch: expected {expected:?}, found {found:?}"
            ),
            Self::CorruptSection(section) => write!(f, "corrupt .skdict section: {section}"),
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

fn empty_fst_bytes() -> &'static [u8] {
    static EMPTY_FST: OnceLock<Box<[u8]>> = OnceLock::new();
    EMPTY_FST.get_or_init(|| {
        MapBuilder::memory()
            .into_inner()
            .expect("empty fst map")
            .into_boxed_slice()
    })
}

struct PayloadReader<'a> {
    buf: &'a [u8],
    pos: usize,
    section: &'static str,
}

impl<'a> PayloadReader<'a> {
    fn new(buf: &'a [u8], section: &'static str) -> Self {
        Self {
            buf,
            pos: 0,
            section,
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    #[inline]
    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn read_u8(&mut self) -> Result<u8, LexiconError> {
        if self.pos >= self.buf.len() {
            return Err(LexiconError::CorruptSection(self.section));
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16(&mut self) -> Result<u16, LexiconError> {
        let end = self
            .pos
            .checked_add(2)
            .filter(|&e| e <= self.buf.len())
            .ok_or(LexiconError::CorruptSection(self.section))?;
        let v = u16::from_le_bytes(self.buf[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32, LexiconError> {
        let end = self
            .pos
            .checked_add(4)
            .filter(|&e| e <= self.buf.len())
            .ok_or(LexiconError::CorruptSection(self.section))?;
        let v = u32::from_le_bytes(self.buf[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], LexiconError> {
        let end = self
            .pos
            .checked_add(len)
            .filter(|&e| e <= self.buf.len())
            .ok_or(LexiconError::CorruptSection(self.section))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N], LexiconError> {
        let slice = self.read_bytes(N)?;
        Ok(slice.try_into().unwrap())
    }

    fn read_len_u32_str(&mut self) -> Result<Box<str>, LexiconError> {
        let len = self.read_u32()? as usize;
        let slice = self.read_bytes(len)?;
        let s = std::str::from_utf8(slice).map_err(LexiconError::Utf8)?;
        Ok(s.to_string().into_boxed_str())
    }

    fn finish(self) -> Result<(), LexiconError> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(LexiconError::CorruptSection(self.section))
        }
    }
}

fn peek_skdict_header_internal(bytes: &[u8]) -> Result<(PackHeader, usize), LexiconError> {
    if bytes.len() < 8 {
        return Err(LexiconError::TruncatedData);
    }
    match &bytes[..8] {
        b"SKDICT01" => return Err(LexiconError::LegacyFormat(1)),
        b"SKDICT02" => return Err(LexiconError::LegacyFormat(2)),
        b"SKDICT03" => {}
        _ => return Err(LexiconError::InvalidMagic),
    }
    if bytes.len() < 10 {
        return Err(LexiconError::TruncatedData);
    }
    let header_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
    let header_end = 10usize
        .checked_add(header_len)
        .ok_or(LexiconError::TruncatedData)?;
    if bytes.len() < header_end {
        return Err(LexiconError::TruncatedData);
    }

    let mut r = PayloadReader::new(&bytes[10..header_end], "header");
    let lang_len = r.read_u8()? as usize;
    if lang_len == 0 {
        return Err(LexiconError::CorruptSection("header"));
    }
    let lang_bytes = r.read_bytes(lang_len)?;
    let lang = std::str::from_utf8(lang_bytes)
        .map_err(LexiconError::Utf8)?
        .to_string();

    let pack_version = [r.read_u8()?, r.read_u8()?, r.read_u8()?];
    let tier = match r.read_u8()? {
        0 => PackTier::Data,
        1 => PackTier::ImeBridge,
        _ => return Err(LexiconError::CorruptSection("header")),
    };

    let variant_count = r.read_u8()? as usize;
    if !(1..=8).contains(&variant_count) {
        return Err(LexiconError::CorruptSection("header"));
    }
    let mut variants = Vec::with_capacity(variant_count);
    for _ in 0..variant_count {
        let vlen = r.read_u8()? as usize;
        if vlen == 0 {
            return Err(LexiconError::CorruptSection("header"));
        }
        let vbytes = r.read_bytes(vlen)?;
        let vstr = std::str::from_utf8(vbytes).map_err(LexiconError::Utf8)?;
        if !vstr.is_ascii() {
            return Err(LexiconError::CorruptSection("header"));
        }
        variants.push(vstr.to_string());
    }

    let freq_slot_count = r.read_u8()? as usize;
    if freq_slot_count > 2 {
        return Err(LexiconError::CorruptSection("header"));
    }
    let mut freq_slots = Vec::with_capacity(freq_slot_count);
    for _ in 0..freq_slot_count {
        freq_slots.push(r.read_u8()?);
    }

    let default_variant_idx = r.read_u8()?;
    if (default_variant_idx as usize) >= variant_count {
        return Err(LexiconError::CorruptSection("header"));
    }
    let feature_flags = r.read_u16()?;
    r.finish()?;

    Ok((
        PackHeader {
            lang,
            pack_version,
            tier,
            variants,
            freq_slots,
            default_variant_idx,
            feature_flags,
        },
        header_end,
    ))
}

/// Parses only the self-describing `.skdict` v3 header without loading FSTs or payload tables.
pub fn peek_skdict_header(bytes: &[u8]) -> Result<PackHeader, LexiconError> {
    peek_skdict_header_internal(bytes).map(|(h, _)| h)
}

fn parse_source_manifest_payload(payload: &[u8]) -> Result<Vec<SourceManifestEntry>, LexiconError> {
    let mut r = PayloadReader::new(payload, "SOURCE_MANIFEST");
    let count = r.read_u32()? as usize;
    let mut out = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let name = r.read_len_u32_str()?.into_string();
        let sha256 = r.read_fixed::<32>()?;
        let license = r.read_len_u32_str()?.into_string();
        out.push(SourceManifestEntry {
            name,
            sha256,
            license,
        });
    }
    r.finish()?;
    Ok(out)
}

/// Parses `.skdict` v3 header plus any `SOURCE_MANIFEST` TLV section without loading FSTs.
pub fn peek_skdict_header_and_manifest(
    bytes: &[u8],
) -> Result<(PackHeader, Vec<SourceManifestEntry>), LexiconError> {
    let (header, mut cursor) = peek_skdict_header_internal(bytes)?;
    let mut manifest = Vec::new();
    while cursor < bytes.len() {
        if cursor + 6 > bytes.len() {
            return Err(LexiconError::TruncatedData);
        }
        let tag = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
        let len = u32::from_le_bytes([
            bytes[cursor + 2],
            bytes[cursor + 3],
            bytes[cursor + 4],
            bytes[cursor + 5],
        ]) as usize;
        cursor += 6;
        let end = cursor
            .checked_add(len)
            .filter(|&e| e <= bytes.len())
            .ok_or(LexiconError::TruncatedData)?;
        if tag == TAG_SOURCE_MANIFEST {
            manifest = parse_source_manifest_payload(&bytes[cursor..end])?;
        }
        cursor = end;
    }
    Ok((header, manifest))
}

/// Loaded language pack lexicon, known-smash table, variant rewriter, and bigram scorer.
pub struct PackLexicon {
    header: PackHeader,
    words_fst: Map<Cow<'static, [u8]>>,
    typos_fst: Map<Cow<'static, [u8]>>,
    typo_pool: Cow<'static, [u8]>,
    variant_rewrites: Vec<Vec<(Box<str>, Box<str>)>>,
    proper_nouns: Vec<(Box<str>, Box<str>)>,
    bigram_keys: Vec<u32>,
    bigram_scores: Vec<u8>,
    smash_entries: Vec<SmashEntry>,
    shorthand: Vec<(Box<str>, Box<str>)>,
    slang_set: Vec<Box<str>>,
    rules: LanguageRules,
    syllable_dict: Vec<Box<str>>,
    source_manifest: Vec<SourceManifestEntry>,
    lev1: LevenshteinAutomatonBuilder,
    lev2: LevenshteinAutomatonBuilder,
}

/// Backwards-compatible alias for [`PackLexicon`].
pub type EnglishLexicon = PackLexicon;

struct ParsedSections {
    header: PackHeader,
    wf_range: Option<(usize, usize)>,
    tf_range: Option<(usize, usize)>,
    tp_range: Option<(usize, usize)>,
    variant_rewrites: Vec<Vec<(Box<str>, Box<str>)>>,
    proper_nouns: Vec<(Box<str>, Box<str>)>,
    bigram_keys: Vec<u32>,
    bigram_scores: Vec<u8>,
    smash_entries: Vec<SmashEntry>,
    shorthand: Vec<(Box<str>, Box<str>)>,
    slang_set: Vec<Box<str>>,
    rules: LanguageRules,
    syllable_dict: Vec<Box<str>>,
    source_manifest: Vec<SourceManifestEntry>,
}

impl PackLexicon {
    /// Loads the bundled English `.skdict` dictionary when compiled with `feature = "bundled-en"`.
    #[cfg(feature = "bundled-en")]
    pub fn from_bundled() -> Result<Self, LexiconError> {
        static BUNDLED: &[u8] = include_bytes!("../assets/en_lexicon.skdict");
        Self::from_static_bytes(BUNDLED)
    }

    /// Zero-copy load over a `'static` byte slice (`include_bytes!` or `mmap`).
    pub fn from_static_bytes(bytes: &'static [u8]) -> Result<Self, LexiconError> {
        let parsed = Self::parse_sections(bytes)?;
        let words_cow = match parsed.wf_range {
            Some((s, e)) => Cow::Borrowed(&bytes[s..e]),
            None => Cow::Borrowed(empty_fst_bytes()),
        };
        let typos_cow = match parsed.tf_range {
            Some((s, e)) => Cow::Borrowed(&bytes[s..e]),
            None => Cow::Borrowed(empty_fst_bytes()),
        };
        let pool_cow = match parsed.tp_range {
            Some((s, e)) => Cow::Borrowed(&bytes[s..e]),
            None => Cow::Borrowed(&[][..]),
        };
        Self::from_parsed(words_cow, typos_cow, pool_cow, parsed)
    }

    /// Loads from an owned byte buffer (e.g. Android asset stream or custom file).
    pub fn from_owned_bytes(bytes: Vec<u8>) -> Result<Self, LexiconError> {
        let parsed = Self::parse_sections(&bytes)?;
        let words_cow = match parsed.wf_range {
            Some((s, e)) => Cow::Owned(bytes[s..e].to_vec()),
            None => Cow::Borrowed(empty_fst_bytes()),
        };
        let typos_cow = match parsed.tf_range {
            Some((s, e)) => Cow::Owned(bytes[s..e].to_vec()),
            None => Cow::Borrowed(empty_fst_bytes()),
        };
        let pool_cow = match parsed.tp_range {
            Some((s, e)) => Cow::Owned(bytes[s..e].to_vec()),
            None => Cow::Borrowed(&[][..]),
        };
        Self::from_parsed(words_cow, typos_cow, pool_cow, parsed)
    }

    fn parse_sections(bytes: &[u8]) -> Result<ParsedSections, LexiconError> {
        let (header, mut cursor) = peek_skdict_header_internal(bytes)?;
        let variant_count = header.variants.len();

        let mut wf_range: Option<(usize, usize)> = None;
        let mut tf_range: Option<(usize, usize)> = None;
        let mut tp_range: Option<(usize, usize)> = None;
        let mut variant_rewrites = vec![Vec::new(); variant_count];
        let mut bigram_keys = Vec::new();
        let mut bigram_scores = Vec::new();
        let mut smash_entries = Vec::new();
        let mut proper_nouns = Vec::new();
        let mut shorthand = Vec::new();
        let mut slang_set = Vec::new();
        let mut rules = LanguageRules::default();
        let mut syllable_dict = Vec::new();
        let mut source_manifest = Vec::new();

        while cursor < bytes.len() {
            if cursor + 6 > bytes.len() {
                return Err(LexiconError::TruncatedData);
            }
            let tag = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
            let len = u32::from_le_bytes([
                bytes[cursor + 2],
                bytes[cursor + 3],
                bytes[cursor + 4],
                bytes[cursor + 5],
            ]) as usize;
            cursor += 6;
            let payload_start = cursor;
            let payload_end = payload_start
                .checked_add(len)
                .filter(|&e| e <= bytes.len())
                .ok_or(LexiconError::TruncatedData)?;
            let payload = &bytes[payload_start..payload_end];
            cursor = payload_end;

            match tag {
                TAG_WORDS_FST => {
                    wf_range = Some((payload_start, payload_end));
                }
                TAG_KNOWN_TYPOS => {
                    if payload.len() < 4 {
                        return Err(LexiconError::CorruptSection("KNOWN_TYPOS"));
                    }
                    let fst_len = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
                    let fst_end = 4usize
                        .checked_add(fst_len)
                        .filter(|&e| e <= payload.len())
                        .ok_or(LexiconError::CorruptSection("KNOWN_TYPOS"))?;
                    tf_range = Some((payload_start + 4, payload_start + fst_end));
                    tp_range = Some((payload_start + fst_end, payload_end));
                }
                TAG_VARIANT_REWRITES => {
                    let mut r = PayloadReader::new(payload, "VARIANT_REWRITES");
                    let group_count = r.read_u8()? as usize;
                    for _ in 0..group_count {
                        let target_idx = r.read_u8()? as usize;
                        if target_idx >= variant_count {
                            return Err(LexiconError::CorruptSection("VARIANT_REWRITES"));
                        }
                        let pair_count = r.read_u32()? as usize;
                        let mut pairs = Vec::with_capacity(pair_count.min(16_384));
                        for _ in 0..pair_count {
                            let from = r.read_len_u32_str()?;
                            let to = r.read_len_u32_str()?;
                            pairs.push((from, to));
                        }
                        pairs.sort_by(|a, b| a.0.cmp(&b.0));
                        pairs.dedup_by(|a, b| a.0 == b.0);
                        variant_rewrites[target_idx] = pairs;
                    }
                    r.finish()?;
                }
                TAG_BIGRAMS => {
                    let mut r = PayloadReader::new(payload, "BIGRAMS");
                    let count = r.read_u32()? as usize;
                    let expected_bytes = count
                        .checked_mul(5)
                        .ok_or(LexiconError::CorruptSection("BIGRAMS"))?;
                    if r.remaining() != expected_bytes {
                        return Err(LexiconError::CorruptSection("BIGRAMS"));
                    }
                    bigram_keys = Vec::with_capacity(count);
                    bigram_scores = Vec::with_capacity(count);
                    for _ in 0..count {
                        let key = r.read_u32()?;
                        let score = r.read_u8()?;
                        bigram_keys.push(key);
                        bigram_scores.push(score);
                    }
                    r.finish()?;
                }
                TAG_SMASH_INDEX => {
                    let mut r = PayloadReader::new(payload, "SMASH_INDEX");
                    let count = r.read_u32()? as usize;
                    smash_entries = Vec::with_capacity(count.min(65_536));
                    for _ in 0..count {
                        let mask = r.read_u32()?;
                        let len = r.read_u8()?;
                        let freq = r.read_u8()?;
                        let word = r.read_len_u32_str()?;
                        smash_entries.push(SmashEntry {
                            mask,
                            len,
                            freq,
                            word,
                        });
                    }
                    r.finish()?;
                }
                TAG_PROPER_NOUNS => {
                    let mut r = PayloadReader::new(payload, "PROPER_NOUNS");
                    let count = r.read_u32()? as usize;
                    proper_nouns = Vec::with_capacity(count.min(16_384));
                    for _ in 0..count {
                        let lower = r.read_len_u32_str()?;
                        let proper = r.read_len_u32_str()?;
                        proper_nouns.push((lower, proper));
                    }
                    r.finish()?;
                    proper_nouns.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    proper_nouns.dedup_by(|a, b| a.0 == b.0);
                }
                TAG_SHORTHAND => {
                    let mut r = PayloadReader::new(payload, "SHORTHAND");
                    let count = r.read_u32()? as usize;
                    shorthand = Vec::with_capacity(count.min(16_384));
                    for _ in 0..count {
                        let trig = r.read_len_u32_str()?;
                        let exp = r.read_len_u32_str()?;
                        shorthand.push((trig, exp));
                    }
                    r.finish()?;
                    shorthand.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                    shorthand.dedup_by(|a, b| a.0 == b.0);
                }
                TAG_SLANG_SET => {
                    let mut r = PayloadReader::new(payload, "SLANG_SET");
                    let count = r.read_u32()? as usize;
                    slang_set = Vec::with_capacity(count.min(16_384));
                    for _ in 0..count {
                        let w = r.read_len_u32_str()?;
                        slang_set.push(w);
                    }
                    r.finish()?;
                    slang_set.sort_unstable();
                    slang_set.dedup();
                }
                TAG_LANGUAGE_RULES => {
                    if payload.is_empty() || payload[0] != 1 {
                        return Err(LexiconError::CorruptSection("LANGUAGE_RULES"));
                    }
                    let json_str =
                        std::str::from_utf8(&payload[1..]).map_err(LexiconError::Utf8)?;
                    rules = serde_json::from_str(json_str)
                        .map_err(|_| LexiconError::CorruptSection("LANGUAGE_RULES"))?;
                }
                TAG_SYLLABLE_DICT => {
                    let mut r = PayloadReader::new(payload, "SYLLABLE_DICT");
                    let count = r.read_u32()? as usize;
                    syllable_dict = Vec::with_capacity(count.min(32_768));
                    for _ in 0..count {
                        let syl = r.read_len_u32_str()?;
                        syllable_dict.push(syl);
                    }
                    r.finish()?;
                    syllable_dict.sort_unstable();
                    syllable_dict.dedup();
                }
                TAG_SOURCE_MANIFEST => {
                    source_manifest = parse_source_manifest_payload(payload)?;
                }
                _ => {
                    // Forward compatibility: cleanly skip unknown TLV tags.
                }
            }
        }

        Ok(ParsedSections {
            header,
            wf_range,
            tf_range,
            tp_range,
            variant_rewrites,
            proper_nouns,
            bigram_keys,
            bigram_scores,
            smash_entries,
            shorthand,
            slang_set,
            rules,
            syllable_dict,
            source_manifest,
        })
    }

    fn from_parsed(
        words_cow: Cow<'static, [u8]>,
        typos_cow: Cow<'static, [u8]>,
        typo_pool: Cow<'static, [u8]>,
        parsed: ParsedSections,
    ) -> Result<Self, LexiconError> {
        let words_fst = Map::new(words_cow)?;
        let typos_fst = Map::new(typos_cow)?;

        {
            let mut stream = words_fst.values();
            while let Some(v) = stream.next() {
                WordMeta::unpack_checked(v)?;
            }
        }

        Ok(Self {
            header: parsed.header,
            words_fst,
            typos_fst,
            typo_pool,
            variant_rewrites: parsed.variant_rewrites,
            proper_nouns: parsed.proper_nouns,
            bigram_keys: parsed.bigram_keys,
            bigram_scores: parsed.bigram_scores,
            smash_entries: parsed.smash_entries,
            shorthand: parsed.shorthand,
            slang_set: parsed.slang_set,
            rules: parsed.rules,
            syllable_dict: parsed.syllable_dict,
            source_manifest: parsed.source_manifest,
            lev1: LevenshteinAutomatonBuilder::new(1, true),
            lev2: LevenshteinAutomatonBuilder::new(2, true),
        })
    }

    #[inline]
    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    #[inline]
    pub fn rules(&self) -> &LanguageRules {
        &self.rules
    }

    #[inline]
    pub fn source_manifest(&self) -> &[SourceManifestEntry] {
        &self.source_manifest
    }

    #[inline]
    pub fn slang_set(&self) -> &[Box<str>] {
        &self.slang_set
    }

    #[inline]
    pub fn syllable_dict(&self) -> &[Box<str>] {
        &self.syllable_dict
    }

    #[inline]
    pub fn lookup_shorthand(&self, lower_word: &str) -> Option<&str> {
        if lower_word.is_empty() {
            return None;
        }
        self.shorthand
            .binary_search_by(|(trig, _)| trig.as_ref().cmp(lower_word))
            .ok()
            .map(|idx| self.shorthand[idx].1.as_ref())
    }

    #[inline]
    pub fn is_slang_word(&self, lower_word: &str) -> bool {
        !lower_word.is_empty() && self.slang_set.binary_search_by(|w| w.as_ref().cmp(lower_word)).is_ok()
    }

    #[inline]
    pub fn is_contraction_pair(&self, typed_lower: &str, expanded_lower: &str) -> bool {
        self.rules.replacements.iter().any(|r| {
            r.kind == ReplacementKind::Contraction
                && r.typed == typed_lower
                && r.fix.to_lowercase() == expanded_lower.to_lowercase()
        })
    }

    #[inline]
    pub fn variant_rewrites_for(&self, variant_idx: u8) -> &[(Box<str>, Box<str>)] {
        self.variant_rewrites
            .get(variant_idx as usize)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn variant_index(&self, tag: &str) -> Option<usize> {
        self.header.variants.iter().position(|v| v == tag)
    }

    #[inline]
    pub fn is_valid_meta(&self, meta: WordMeta, dialect: Dialect) -> bool {
        meta.is_valid_for_pack(self.header.variants.len(), dialect)
    }

    #[inline]
    pub fn effective_freq_meta(&self, meta: WordMeta, dialect: Dialect) -> u8 {
        meta.effective_freq_for_pack(self.header.variants.len(), dialect)
    }

    #[inline]
    pub fn lookup_word(&self, lower_word: &str) -> Option<WordMeta> {
        self.words_fst.get(lower_word).map(WordMeta::unpack)
    }

    #[inline]
    pub fn contains_valid(&self, lower_word: &str, dialect: Dialect) -> bool {
        self.lookup_word(lower_word)
            .is_some_and(|m| self.is_valid_meta(m, dialect))
    }

    /// Returns true if `lower_word` matches `prefix'stem` (or `prefix’stem`) where `prefix` is in
    /// `rules.clitics.prefixes`, `stem` has at least 2 Unicode chars, and `stem` is valid in `dialect`.
    #[inline]
    pub fn is_valid_elided(&self, lower_word: &str, dialect: Dialect) -> bool {
        let prefixes = &self.rules.clitics.prefixes;
        if prefixes.is_empty() {
            return false;
        }
        let Some((prefix, stem)) = lower_word
            .split_once('\'')
            .or_else(|| lower_word.split_once('’'))
        else {
            return false;
        };
        prefixes.iter().any(|p| p == prefix)
            && stem.chars().count() >= 2
            && self.contains_valid(stem, dialect)
    }

    #[inline]
    pub fn contains_valid_or_elided(&self, lower_word: &str, dialect: Dialect) -> bool {
        self.contains_valid(lower_word, dialect) || self.is_valid_elided(lower_word, dialect)
    }

    #[inline]
    pub fn contains_any_variant(&self, lower_word: &str) -> bool {
        self.lookup_word(lower_word)
            .is_some_and(|m| m.is_valid_for_pack(self.header.variants.len(), Dialect::Mixed))
    }

    #[inline]
    pub fn contains_any_variant_or_elided(&self, lower_word: &str) -> bool {
        self.contains_any_variant(lower_word) || self.is_valid_elided(lower_word, Dialect::Mixed)
    }

    #[inline]
    pub fn contains_any_english(&self, lower_word: &str) -> bool {
        self.contains_any_variant(lower_word)
    }

    /// Maps a word to the preferred variant if a variant rewrite counterpart exists.
    pub fn map_dialect<'a>(&'a self, lower_word: &'a str, dialect: Dialect) -> &'a str {
        if self.header.variants.len() <= 1 {
            return lower_word;
        }
        let target_idx = match dialect {
            Dialect::Mixed => Some(self.header.default_variant_idx as usize),
            Dialect::EnUs => self.variant_index("en_US"),
            Dialect::EnGb => self.variant_index("en_GB"),
        };
        let Some(pairs) = target_idx.and_then(|idx| self.variant_rewrites.get(idx)) else {
            return lower_word;
        };
        if let Ok(idx) = pairs.binary_search_by(|(from, _)| from.as_ref().cmp(lower_word)) {
            &pairs[idx].1
        } else {
            lower_word
        }
    }

    /// Strictly checks if `lower_word` requires cross-dialect rewriting under `EnUs` or `EnGb`.
    pub fn strict_dialect_rewrite(&self, lower_word: &str, dialect: Dialect) -> Option<&str> {
        if self.header.variants.len() <= 1 {
            return None;
        }
        let target_idx = match dialect {
            Dialect::Mixed => return None,
            Dialect::EnUs => self.variant_index("en_US"),
            Dialect::EnGb => self.variant_index("en_GB"),
        }?;
        let pairs = self.variant_rewrites.get(target_idx)?;
        pairs
            .binary_search_by(|(from, _)| from.as_ref().cmp(lower_word))
            .ok()
            .map(|i| pairs[i].1.as_ref())
    }

    /// Looks up curated human keyboard smashes (`crate-ci/typos` `words.csv`).
    pub fn lookup_known_typos(&self, lower_word: &str) -> Option<impl Iterator<Item = &str>> {
        let packed = self.typos_fst.get(lower_word)?;
        let offset = (packed & 0xFFFF_FFFF) as usize;
        let len = (packed >> 32) as usize;
        let end = offset.checked_add(len)?;
        let slice = std::str::from_utf8(self.typo_pool.get(offset..end)?).ok()?;
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
            if meta.freq0 == 0 && meta.freq1 == 0 {
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

    /// Dumps all `(word, freq0, freq1, variant_mask, flags, word_id)` entries from `WORDS_FST`.
    pub fn dump_words(&self) -> Vec<(String, u8, u8, u8, u8, u16)> {
        let mut out = Vec::new();
        let mut stream = self.words_fst.stream();
        while let Some((k, v)) = stream.next() {
            let w = String::from_utf8_lossy(k).into_owned();
            let m = WordMeta::unpack(v);
            out.push((w, m.freq0, m.freq1, m.variant_mask, m.flags, m.word_id));
        }
        out
    }

    /// Dumps all `(typo, joined_targets)` entries from `KNOWN_TYPOS`.
    pub fn dump_typos(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stream = self.typos_fst.stream();
        while let Some((k, packed)) = stream.next() {
            let offset = (packed & 0xFFFF_FFFF) as usize;
            let len = (packed >> 32) as usize;
            let targets = self
                .typo_pool
                .get(offset..offset + len)
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or_default()
                .to_string();
            out.push((String::from_utf8_lossy(k).into_owned(), targets));
        }
        out
    }

    /// Dumps all `(key, score)` pairs from `BIGRAMS`.
    pub fn dump_bigrams(&self) -> Vec<(u32, u8)> {
        self.bigram_keys
            .iter()
            .copied()
            .zip(self.bigram_scores.iter().copied())
            .collect()
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
    let user_pack = default_user_dict_dir().join(format!("{lang}.skdict"));
    if user_pack.exists() {
        return Some(user_pack);
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
    if let Some(home) = crate::event_log::resolve_home_dir() {
        return home.join(".local/share/smartkey/dicts");
    }
    std::env::temp_dir().join("smartkey/dicts")
}

/// Minimum frequency margin (AOSP `f=` units) by which the capitalized form must beat an
/// attested lowercase form for a proper-noun entry to be accepted. Measured on the English pack:
/// real homographs (`polish` 34, `march` 26, `turkey` 20, `mars` 49) fall below it; legitimate
/// entries whose lowercase form is only noise (`dutch` 64, `august` 74, `england` 100) clear it.
pub const PROPER_NOUN_MIN_CASE_GAP: i32 = 60;

/// Result of [`lint_proper_nouns`]: `errors` must fail the build, `warnings` need human review.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProperNounLint {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Build-time homograph check for `proper_nouns.tsv`. AOSP word lists keep `Polish` and `polish`
/// as separate entries, so an entry whose lowercase form is attested with a frequency close to
/// (or above) the capitalized form is a different word sense (`polish` the verb), and capitalizing
/// it unconditionally is a false positive. The lowercase signal is language-independent: any pack
/// built from a case-sensitive AOSP/HeliBoard list can run the same check.
pub fn lint_proper_nouns(wordlists: &[&str], proper_nouns_tsv: &str) -> ProperNounLint {
    // raw (case-sensitive) form -> best frequency across lists
    let mut freq: BTreeMap<String, i32> = BTreeMap::new();
    for content in wordlists {
        for line in content.lines() {
            let Some(rest) = line.trim().strip_prefix("word=") else {
                continue;
            };
            let mut raw = "";
            let mut f: i32 = 0;
            let mut skip = false;
            for part in rest.split(',') {
                if raw.is_empty() && !part.contains('=') {
                    raw = part;
                } else if let Some(v) = part.strip_prefix("f=") {
                    f = v.parse().unwrap_or(0);
                } else if part == "not_a_word=true" || part == "possibly_offensive=true" {
                    skip = true;
                }
            }
            if raw.is_empty() || skip || f == 0 {
                continue;
            }
            let e = freq.entry(raw.to_string()).or_insert(0);
            *e = (*e).max(f);
        }
    }

    let mut out = ProperNounLint::default();
    for line in proper_nouns_tsv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((lower, proper)) = line.split_once('\t') else {
            continue;
        };
        let (lower, proper) = (lower.trim(), proper.trim());
        let Some(&lower_f) = freq.get(lower) else {
            continue;
        };
        let proper_f = freq.get(proper).copied().unwrap_or(0);
        let gap = proper_f - lower_f;
        let msg = format!(
            "{lower:?} -> {proper:?}: lowercase form is also a word (f={lower_f}) vs capitalized f={proper_f} (gap {gap})"
        );
        if gap < PROPER_NOUN_MIN_CASE_GAP {
            out.errors.push(format!(
                "{msg}; likely homograph, remove it (need gap >= {PROPER_NOUN_MIN_CASE_GAP})"
            ));
        } else {
            out.warnings.push(format!("{msg}; review manually"));
        }
    }
    out
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
            freq0: f,
            freq1: f,
            variant_mask: 0x03,
            flags: 0,
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

    let mut uk_vec: Vec<(String, String)> = uk_pairs
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();
    uk_vec.sort_unstable();
    uk_vec.dedup_by(|a, b| a.0 == b.0);
    let mut us_vec: Vec<(String, String)> = uk_vec
        .iter()
        .map(|(uk, us)| (us.clone(), uk.clone()))
        .collect();
    us_vec.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    us_vec.dedup_by(|a, b| a.0 == b.0);

    let mut bg_map: BTreeMap<u32, u8> = BTreeMap::new();
    for &(p, n, sc) in bigrams {
        if let (Some(&pid), Some(&nid)) = (
            word_ids.get(&p.to_lowercase()),
            word_ids.get(&n.to_lowercase()),
        ) {
            bg_map.insert(((pid as u32) << 16) | (nid as u32), sc);
        }
    }

    let mut pn_vec: Vec<(String, String)> = proper_nouns
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();
    pn_vec.sort_unstable();
    pn_vec.dedup_by(|a, b| a.0 == b.0);

    let header = PackHeader {
        lang: "en".to_string(),
        pack_version: [0, 3, 0],
        tier: PackTier::Data,
        variants: vec!["en_US".to_string(), "en_GB".to_string()],
        freq_slots: vec![0x01, 0x02],
        default_variant_idx: 0,
        feature_flags: 0,
    };
    let mut variant_rewrites = Vec::new();
    if !uk_vec.is_empty() {
        variant_rewrites.push((0u8, uk_vec));
    }
    if !us_vec.is_empty() {
        variant_rewrites.push((1u8, us_vec));
    }

    serialize_skdict_v3(&SkdictV3Payload {
        header: &header,
        words_fst: &wb.into_inner().unwrap(),
        typos_fst: &tb.into_inner().unwrap(),
        typo_pool: &pool,
        variant_rewrites: &variant_rewrites,
        bigrams: &bg_map,
        smash_entries: &smash_entries,
        proper_nouns: &pn_vec,
        shorthand: &[],
        slang_set: &[],
        rules: None,
        syllable_dict: &[],
        source_manifest: &[],
    })
}

pub fn build_words_fst_bytes(entries: &[(&str, WordMeta)]) -> Vec<u8> {
    let mut sorted = entries.to_vec();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    sorted.dedup_by(|a, b| a.0 == b.0);
    let mut wb = MapBuilder::memory();
    for (w, meta) in sorted {
        wb.insert(w.as_bytes(), meta.pack()).unwrap();
    }
    wb.into_inner().unwrap()
}

pub struct SkdictV3Payload<'a> {
    pub header: &'a PackHeader,
    pub words_fst: &'a [u8],
    pub typos_fst: &'a [u8],
    pub typo_pool: &'a [u8],
    pub variant_rewrites: &'a [(u8, Vec<(String, String)>)],
    pub bigrams: &'a BTreeMap<u32, u8>,
    pub smash_entries: &'a [(u32, u8, u8, String)],
    pub proper_nouns: &'a [(String, String)],
    pub shorthand: &'a [(String, String)],
    pub slang_set: &'a [String],
    pub rules: Option<&'a LanguageRules>,
    pub syllable_dict: &'a [String],
    pub source_manifest: &'a [SourceManifestEntry],
}

fn write_u32_slice(buf: &mut Vec<u8>, data: &[u8]) {
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
}

fn write_tlv_section(out: &mut Vec<u8>, tag: u16, payload: &[u8]) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
}

pub fn serialize_skdict_v3(p: &SkdictV3Payload<'_>) -> Vec<u8> {
    assert!(!p.header.lang.is_empty() && p.header.lang.len() <= 255);
    assert!((1..=8).contains(&p.header.variants.len()));
    assert!(p.header.freq_slots.len() <= 2);
    assert!((p.header.default_variant_idx as usize) < p.header.variants.len());

    let mut hdr = Vec::with_capacity(64);
    hdr.push(p.header.lang.len() as u8);
    hdr.extend_from_slice(p.header.lang.as_bytes());
    hdr.extend_from_slice(&p.header.pack_version);
    hdr.push(p.header.tier as u8);
    hdr.push(p.header.variants.len() as u8);
    for v in &p.header.variants {
        assert!(!v.is_empty() && v.len() <= 255 && v.is_ascii());
        hdr.push(v.len() as u8);
        hdr.extend_from_slice(v.as_bytes());
    }
    hdr.push(p.header.freq_slots.len() as u8);
    for &mask in &p.header.freq_slots {
        hdr.push(mask);
    }
    hdr.push(p.header.default_variant_idx);
    hdr.extend_from_slice(&p.header.feature_flags.to_le_bytes());
    assert!(hdr.len() <= u16::MAX as usize);

    let mut out = Vec::with_capacity(
        10 + hdr.len() + p.words_fst.len() + p.typos_fst.len() + p.typo_pool.len() + 65536,
    );
    out.extend_from_slice(MAGIC_V3);
    out.extend_from_slice(&(hdr.len() as u16).to_le_bytes());
    out.extend_from_slice(&hdr);

    if !p.words_fst.is_empty() {
        write_tlv_section(&mut out, TAG_WORDS_FST, p.words_fst);
    }

    if !p.typos_fst.is_empty() || !p.typo_pool.is_empty() {
        let mut sec = Vec::with_capacity(4 + p.typos_fst.len() + p.typo_pool.len());
        sec.extend_from_slice(&(p.typos_fst.len() as u32).to_le_bytes());
        sec.extend_from_slice(p.typos_fst);
        sec.extend_from_slice(p.typo_pool);
        write_tlv_section(&mut out, TAG_KNOWN_TYPOS, &sec);
    }

    if !p.variant_rewrites.is_empty() {
        let mut sec = Vec::new();
        sec.push(p.variant_rewrites.len() as u8);
        for (target_idx, pairs) in p.variant_rewrites {
            sec.push(*target_idx);
            sec.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
            for (from, to) in pairs {
                write_u32_slice(&mut sec, from.as_bytes());
                write_u32_slice(&mut sec, to.as_bytes());
            }
        }
        write_tlv_section(&mut out, TAG_VARIANT_REWRITES, &sec);
    }

    if !p.bigrams.is_empty() {
        let mut sec = Vec::with_capacity(4 + p.bigrams.len() * 5);
        sec.extend_from_slice(&(p.bigrams.len() as u32).to_le_bytes());
        for (&key, &score) in p.bigrams {
            sec.extend_from_slice(&key.to_le_bytes());
            sec.push(score);
        }
        write_tlv_section(&mut out, TAG_BIGRAMS, &sec);
    }

    if !p.smash_entries.is_empty() {
        let mut sec = Vec::with_capacity(4 + p.smash_entries.len() * 20);
        sec.extend_from_slice(&(p.smash_entries.len() as u32).to_le_bytes());
        for (mask, len, freq, word) in p.smash_entries {
            sec.extend_from_slice(&mask.to_le_bytes());
            sec.push(*len);
            sec.push(*freq);
            write_u32_slice(&mut sec, word.as_bytes());
        }
        write_tlv_section(&mut out, TAG_SMASH_INDEX, &sec);
    }

    if !p.proper_nouns.is_empty() {
        let mut sec = Vec::new();
        sec.extend_from_slice(&(p.proper_nouns.len() as u32).to_le_bytes());
        for (lower, proper) in p.proper_nouns {
            write_u32_slice(&mut sec, lower.as_bytes());
            write_u32_slice(&mut sec, proper.as_bytes());
        }
        write_tlv_section(&mut out, TAG_PROPER_NOUNS, &sec);
    }

    if !p.shorthand.is_empty() {
        let mut sec = Vec::new();
        sec.extend_from_slice(&(p.shorthand.len() as u32).to_le_bytes());
        for (trig, exp) in p.shorthand {
            write_u32_slice(&mut sec, trig.as_bytes());
            write_u32_slice(&mut sec, exp.as_bytes());
        }
        write_tlv_section(&mut out, TAG_SHORTHAND, &sec);
    }

    if !p.slang_set.is_empty() {
        let mut sec = Vec::new();
        sec.extend_from_slice(&(p.slang_set.len() as u32).to_le_bytes());
        for w in p.slang_set {
            write_u32_slice(&mut sec, w.as_bytes());
        }
        write_tlv_section(&mut out, TAG_SLANG_SET, &sec);
    }

    if let Some(rules) = p.rules {
        let json = serde_json::to_vec(rules).expect("serialize LanguageRules JSON");
        let mut sec = Vec::with_capacity(1 + json.len());
        sec.push(1u8);
        sec.extend_from_slice(&json);
        write_tlv_section(&mut out, TAG_LANGUAGE_RULES, &sec);
    }

    if !p.syllable_dict.is_empty() {
        let mut sec = Vec::new();
        sec.extend_from_slice(&(p.syllable_dict.len() as u32).to_le_bytes());
        for syl in p.syllable_dict {
            write_u32_slice(&mut sec, syl.as_bytes());
        }
        write_tlv_section(&mut out, TAG_SYLLABLE_DICT, &sec);
    }

    if !p.source_manifest.is_empty() {
        let mut sec = Vec::new();
        sec.extend_from_slice(&(p.source_manifest.len() as u32).to_le_bytes());
        for entry in p.source_manifest {
            write_u32_slice(&mut sec, entry.name.as_bytes());
            sec.extend_from_slice(&entry.sha256);
            write_u32_slice(&mut sec, entry.license.as_bytes());
        }
        write_tlv_section(&mut out, TAG_SOURCE_MANIFEST, &sec);
    }

    out
}

#[cfg(test)]
mod proper_noun_lint_tests {
    use super::*;

    const WORDLIST: &str = "\
dictionary=main:en_us,locale=en_US
 word=polish,f=92,flags=,originalFreq=92
 word=Polish,f=126,flags=,originalFreq=126
 word=august,f=71,flags=,originalFreq=71
 word=August,f=145,flags=,originalFreq=145
 word=English,f=144,flags=,originalFreq=144
 word=Monday,f=111,flags=,originalFreq=111
 word=junk,f=50,not_a_word=true
";

    #[test]
    fn homograph_is_an_error_clear_gap_is_a_warning_unattested_lowercase_passes() {
        let tsv = "# header\npolish\tPolish\naugust\tAugust\nenglish\tEnglish\nmonday\tMonday\n";
        let lint = lint_proper_nouns(&[WORDLIST], tsv);
        assert_eq!(lint.errors.len(), 1, "{lint:?}");
        assert!(lint.errors[0].contains("\"polish\""), "{lint:?}");
        assert_eq!(lint.warnings.len(), 1, "{lint:?}");
        assert!(lint.warnings[0].contains("\"august\""), "{lint:?}");
    }

    #[test]
    fn lowercase_form_more_frequent_than_capitalized_is_an_error() {
        let wl = " word=will,f=165\n word=Will,f=113\n";
        let lint = lint_proper_nouns(&[wl], "will\tWill\n");
        assert_eq!(lint.errors.len(), 1, "{lint:?}");
    }

    #[test]
    fn shipped_proper_nouns_pass_the_lint() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let (Ok(us), Ok(gb), Ok(tsv)) = (
            std::fs::read_to_string(root.join("data/sources/en_US.combined")),
            std::fs::read_to_string(root.join("data/sources/en_GB.combined")),
            std::fs::read_to_string(root.join("dicts/en/proper_nouns.tsv")),
        ) else {
            eprintln!("skipping: run smartkey-dev build-pack en --fetch for the AOSP word lists");
            return;
        };
        let lint = lint_proper_nouns(&[&us, &gb], &tsv);
        assert!(
            lint.errors.is_empty(),
            "gap < {PROPER_NOUN_MIN_CASE_GAP}: {:?}",
            lint.errors
        );

        for (lang, wl_rel) in [
            ("es", "data/sources/es/es_wordlist.combined"),
            ("fr", "data/sources/fr/fr_wordlist.combined"),
        ] {
            if let (Ok(wl), Ok(pn)) = (
                std::fs::read_to_string(root.join(wl_rel)),
                std::fs::read_to_string(root.join(format!("dicts/{lang}/proper_nouns.tsv"))),
            ) {
                let l = lint_proper_nouns(&[&wl], &pn);
                assert!(
                    l.errors.is_empty(),
                    "{lang} proper_nouns gap < {PROPER_NOUN_MIN_CASE_GAP}: {:?}",
                    l.errors
                );
            }
        }
    }

    #[cfg(feature = "bundled-en")]
    #[test]
    fn from_static_bytes_borrows_fst_and_typo_pool_without_copy() {
        use std::borrow::Cow;
        static RAW: &[u8] = include_bytes!("../assets/en_lexicon.skdict");
        let lex = super::PackLexicon::from_static_bytes(RAW).expect("bundled lexicon loads");
        let range = RAW.as_ptr_range();
        assert!(matches!(lex.typo_pool, Cow::Borrowed(_)));
        assert!(range.contains(&lex.typo_pool.as_ptr()));
        assert!(!lex.words_fst.as_fst().as_bytes().is_empty());
        assert!(range.contains(&lex.words_fst.as_fst().as_bytes().as_ptr()));
        assert!(!lex.typos_fst.as_fst().as_bytes().is_empty());
        assert!(range.contains(&lex.typos_fst.as_fst().as_bytes().as_ptr()));
        assert!(lex.lookup_word("the").is_some());
        assert!(lex.lookup_known_typos("teh").is_some());
    }

    #[test]
    fn skdict_v3_rejects_legacy_magics_skips_unknown_tlv_and_checks_wordmeta_reserved_bits() {
        assert!(matches!(
            peek_skdict_header(b"SKDICT01xxxx"),
            Err(LexiconError::LegacyFormat(1))
        ));
        assert!(matches!(
            peek_skdict_header(b"SKDICT02xxxx"),
            Err(LexiconError::LegacyFormat(2))
        ));
        assert!(matches!(
            PackLexicon::from_owned_bytes(b"SKDICT02xxxx".to_vec()),
            Err(LexiconError::LegacyFormat(2))
        ));

        // Reserved bits 48..64 must be rejected by unpack_checked and load
        let good_meta = WordMeta {
            freq0: 190,
            freq1: 185,
            variant_mask: 0x03,
            flags: FLAG_SHIELD | FLAG_AMBIGUOUS_PROPER,
            word_id: 42,
        };
        let packed = good_meta.pack();
        assert_eq!(WordMeta::unpack_checked(packed).unwrap(), good_meta);
        assert!(matches!(
            WordMeta::unpack_checked(packed | (1u64 << 48)),
            Err(LexiconError::CorruptSection("WORDS_FST"))
        ));

        let mut bad_wb = MapBuilder::memory();
        bad_wb.insert(b"hello", packed | (1u64 << 55)).unwrap();
        let bad_hdr = PackHeader {
            lang: "en".to_string(),
            pack_version: [0, 3, 0],
            tier: PackTier::Data,
            variants: vec!["en_US".to_string()],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };
        let bad_img = serialize_skdict_v3(&SkdictV3Payload {
            header: &bad_hdr,
            words_fst: &bad_wb.into_inner().unwrap(),
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &[],
            shorthand: &[],
            slang_set: &[],
            rules: None,
            syllable_dict: &[],
            source_manifest: &[],
        });
        assert!(matches!(
            PackLexicon::from_owned_bytes(bad_img),
            Err(LexiconError::CorruptSection("WORDS_FST"))
        ));

        // Single-variant pack (`variant_count == 1`), unknown TLV tag skip, and manifest/shorthand/slang/syllables roundtrip
        let mut wb = MapBuilder::memory();
        let es_meta = WordMeta {
            freq0: 200,
            freq1: 0,
            variant_mask: 0x01,
            flags: 0,
            word_id: 1,
        };
        wb.insert(b"hola", es_meta.pack()).unwrap();
        let es_hdr = PackHeader {
            lang: "es".to_string(),
            pack_version: [0, 3, 1],
            tier: PackTier::Data,
            variants: vec!["es_ES".to_string()],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };
        let manifest = vec![SourceManifestEntry {
            name: "es_wordlist".to_string(),
            sha256: [0xAB; 32],
            license: "Apache-2.0".to_string(),
        }];
        let mut img = serialize_skdict_v3(&SkdictV3Payload {
            header: &es_hdr,
            words_fst: &wb.into_inner().unwrap(),
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &[],
            shorthand: &[("xq".to_string(), "porque".to_string())],
            slang_set: &["holi".to_string()],
            rules: Some(&LanguageRules::default()),
            syllable_dict: &["syl".to_string()],
            source_manifest: &manifest,
        });
        // Append an unknown future TLV tag 0x9999 with 4 bytes payload
        img.extend_from_slice(&0x9999u16.to_le_bytes());
        img.extend_from_slice(&4u32.to_le_bytes());
        img.extend_from_slice(b"TEST");

        let (peeked_hdr, peeked_manifest) = peek_skdict_header_and_manifest(&img).unwrap();
        assert_eq!(peeked_hdr, es_hdr);
        assert_eq!(peeked_manifest, manifest);

        let lex = PackLexicon::from_owned_bytes(img).unwrap();
        assert_eq!(lex.header(), &es_hdr);
        assert!(lex.contains_valid("hola", Dialect::EnGb));
        assert_eq!(
            lex.effective_freq_meta(lex.lookup_word("hola").unwrap(), Dialect::EnGb),
            200
        );
        assert_eq!(lex.lookup_shorthand("xq"), Some("porque"));
        assert!(lex.is_slang_word("holi"));
        assert_eq!(lex.syllable_dict(), &["syl".to_string().into_boxed_str()]);
        assert_eq!(lex.source_manifest(), manifest.as_slice());
    }
}
