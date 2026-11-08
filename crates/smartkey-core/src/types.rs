use serde::{Deserialize, Serialize};

/// English spelling dialect preference (`rules.toml`: `dialect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Dialect {
    /// Accept both valid US and UK spellings untouched; output US spelling when fixing typos.
    #[default]
    Mixed,
    /// Strict American English (also normalizes valid UK words like `colour -> color`).
    #[serde(alias = "en_US", alias = "us")]
    EnUs,
    /// Strict British English (also normalizes valid US words like `color -> colour`).
    #[serde(alias = "en_GB", alias = "gb", alias = "uk")]
    EnGb,
}

/// Confidence threshold profile (`rules.toml`: `aggressiveness`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Aggressiveness {
    Conservative,
    #[default]
    Balanced,
    Aggressive,
}

/// Active application context resolved by the host adapter (`fcitx5`, CLI, or HeliBoard).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AppProfile {
    /// Passwords, URLs, emails, terminals, and code editors: 100% disabled.
    Bypass,
    /// Web browsers (non-URL/non-password text fields).
    Browser,
    /// Messaging apps (`discord`, `vesktop`, `slack`, `telegram`, `signal`).
    Chat,
    /// Notes, documents, email clients, and general prose inputs.
    #[default]
    Prose,
}

/// Origin of a committed token, enabling zero-refactor bilingual integration with `fcitx5-lotus`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum InputOrigin {
    /// Standard raw character typing (Phase 1 default).
    #[default]
    PlainAscii,
    /// `fcitx5-lotus` `AutoNonVnRestore` restored non-Vietnamese ASCII keys.
    LotusRestoredAscii,
    /// `fcitx5-lotus` composed Vietnamese output from `raw_ascii` keystrokes.
    LotusComposed { raw_ascii: String, vi_out: String },
}

/// Opaque language identifier (`"en"` default; `"vi"` passed by `fcitx5-lotus` adapter).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LangId(pub String);

impl Default for LangId {
    fn default() -> Self {
        Self("en".to_string())
    }
}

impl LangId {
    pub fn en() -> Self {
        Self("en".to_string())
    }

    pub fn vi() -> Self {
        Self("vi".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Layer 1 scorer & normalization settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScorerConfig {
    pub dialect: Dialect,
    pub aggressiveness: Aggressiveness,
    pub auto_capitalize_i: bool,
    pub auto_capitalize_sentence: bool,
    pub fix_shift_slips: bool,
    pub fix_proper_nouns: bool,
    pub enable_contractions: bool,
    pub enable_missed_space_split: bool,
}

impl Default for ScorerConfig {
    fn default() -> Self {
        Self {
            dialect: Dialect::Mixed,
            aggressiveness: Aggressiveness::Balanced,
            auto_capitalize_i: true,
            auto_capitalize_sentence: false,
            fix_shift_slips: true,
            fix_proper_nouns: true,
            enable_contractions: true,
            enable_missed_space_split: true,
        }
    }
}

/// Provenance of a candidate replacement for debugging and `smartkey explain`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchKind {
    Bypass,
    ValidWord,
    StretchedWord,
    UserSnippet,
    UserContextRule,
    UserCorrectionRule,
    ShiftSlip,
    ProperNoun,
    DialectNormalization,
    Contraction,
    UnmergedCompound,
    SemicolonContraction,
    TransposedApostrophe,
    KnownTypoDict,
    SwitchChatter,
    MissedSpaceSplit,
    FuzzyLexicon,
    SmashRescue,
    PersonalDict,
    VietnameseTelexRescue,
}

/// Ranked candidate produced by a `LanguageScorer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub replacement: String,
    pub score: f32,
    pub edit_cost: f32,
    pub freq: u8,
    pub bigram_boost: f32,
    pub kind: MatchKind,
}

/// Outcome of evaluating a single token at a word boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvalOutcome {
    /// Leave the token untouched (`do_not_learn = true` for code/stretched/valid words).
    Unchanged {
        reason: MatchKind,
        do_not_learn: bool,
    },
    /// Confidently replace the token with `replacement`.
    Autocorrect {
        replacement: String,
        kind: MatchKind,
        score: f32,
        margin: f32,
    },
}

/// Detailed diagnostic report returned by `smartkey explain`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub input: String,
    pub prev_word: Option<String>,
    pub boundary_punct: Option<char>,
    pub outcome: EvalOutcome,
    pub top_candidates: Vec<Candidate>,
}
