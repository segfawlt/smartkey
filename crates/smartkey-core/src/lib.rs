pub mod casing;
pub mod event_log;
pub mod keyboard;
pub mod learner;
pub mod lexicon;
pub mod pack_builder;
pub mod scorer;
pub mod session;
pub mod store;
pub mod types;
pub mod vi_telex;

pub use event_log::{
    default_state_log_dir, resolve_home_dir, DiagnosticsConfig, EventLog, EventLogLevel,
    LoggedDiagnosticRecord,
};
pub use learner::{LearnEvent, SkipReason};
pub use lexicon::{
    default_user_dict_dir, lookup_english_shorthand, peek_skdict_header,
    peek_skdict_header_and_manifest, resolve_dict_path, BigramBoost, CasingRules, CliticRules,
    EnglishLexicon, LanguageRules, LexiconError, PackHeader, PackLexicon, PackTier,
    ReplacementKind, ReplacementRule, SourceManifestEntry, WordMeta,
};
pub use pack_builder::{
    build_pack_from_dir, sha256_bytes, sha256_hex, BuiltPack, PackBuildOptions, PackRecipe,
};
pub use scorer::{
    EmptyUserRules, EnglishScorer, EvalContext, LanguageScorer, LatinScorer, UserRuleView,
};
pub use session::{
    apply_key_to_buffer, KeyInput, LexiconRegistry, SessionAction, SessionOutput, SmartKeyEngine,
    SmartKeySession,
};
pub use store::{
    classify_builtin_program, normalize_program_id, BlockPair, CompiledRules, ConfidenceTier,
    DiagnosticEvent, LearnerState, Preset, PruneCandidate, PruneKinds, PruneOptions, PruneReport,
    RuleOrigin, RuleStat, RuleStore, SkippedEditEntry, UndoResolution,
};
pub use types::{
    Aggressiveness, AppProfile, Candidate, Dialect, EvalOutcome, Explanation, InputOrigin, LangId,
    MatchKind, ScorerConfig,
};
pub use vi_telex::{
    has_any_vietnamese_rune, is_vietnamese_dict_word, is_vietnamese_slang_word,
    lookup_vietnamese_shorthand, project_shorthand_case, rescue_telex_typo,
    rescue_telex_typo_with_options, resolve_lotus_dict_path, simulate_lotus_word,
    simulate_lotus_word_with_options, LotusOptions, LotusSimulation, TelexRescue, W2uMode,
};
