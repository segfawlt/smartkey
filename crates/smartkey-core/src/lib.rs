pub mod casing;
pub mod keyboard;
pub mod lexicon;
pub mod scorer;
pub mod types;

pub use lexicon::{compile_skdict_from_sources, EnglishLexicon, LexiconError, WordMeta};
pub use scorer::{EmptyUserRules, EnglishScorer, EvalContext, LanguageScorer, UserRuleView};
pub use types::{
    Aggressiveness, AppProfile, Candidate, Dialect, EvalOutcome, Explanation, InputOrigin, LangId,
    MatchKind, ScorerConfig,
};
