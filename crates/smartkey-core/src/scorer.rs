use std::collections::{BTreeSet, HashMap};

use crate::casing::{analyze_token, project_case, TokenAnalysis};
use crate::keyboard::{
    ascii_letter_mask, damerau_levenshtein_chars, multiset_overlap, weighted_keyboard_cost,
};
use crate::lexicon::EnglishLexicon;
use crate::types::{
    Aggressiveness, AppProfile, Candidate, EvalOutcome, Explanation, MatchKind, ScorerConfig,
};

/// Read-only view of user-learned rules (`rules.toml` + session shields) queried during Layer 1 evaluation.
pub trait UserRuleView {
    fn lookup_snippet(&self, trigger: &str) -> Option<&str> {
        let _ = trigger;
        None
    }
    fn lookup_context_rule(&self, prev_lower: &str, typed_lower: &str) -> Option<&str> {
        let _ = (prev_lower, typed_lower);
        None
    }
    fn lookup_correction(&self, typed_lower: &str) -> Option<&str> {
        let _ = typed_lower;
        None
    }
    fn is_personal_word(&self, lower_word: &str) -> bool {
        let _ = lower_word;
        false
    }
    fn is_undo_shielded(&self, lower_word: &str) -> bool {
        let _ = lower_word;
        false
    }
    fn is_pair_blocked(&self, typed_lower: &str, candidate_lower: &str) -> bool {
        let _ = (typed_lower, candidate_lower);
        false
    }
    fn trouble_word_boost(&self, candidate_lower: &str) -> f32 {
        let _ = candidate_lower;
        0.0
    }
    fn personal_words(&self) -> Option<&BTreeSet<String>> {
        None
    }
}

/// Default empty user-rule set for standalone Layer 1 evaluation and benchmarks.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyUserRules;
impl UserRuleView for EmptyUserRules {}

/// Context passed into `LanguageScorer::evaluate` at a word boundary.
#[derive(Debug, Clone, Default)]
pub struct EvalContext<'a> {
    pub prev_word: Option<&'a str>,
    pub boundary_punct: Option<char>,
    pub at_sentence_start: bool,
    pub suppress_sentence_cap: bool,
    pub in_markdown_code: bool,
    pub profile: AppProfile,
}

/// Pluggable language-specific scorer (`lang::en` in Phase 1; `lang::vi` alongside `fcitx5-lotus` in Phase 2).
pub trait LanguageScorer {
    fn evaluate(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
    ) -> EvalOutcome;

    fn explain(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
    ) -> Explanation;
}

/// Unconditional missing-apostrophe contractions (excludes valid homographs like `its`, `were`, `lets`, `ill`, `id`, `well`, `hell`, `shed`, `wed`).
const UNCONDITIONAL_CONTRACTIONS: &[(&str, &str)] = &[
    ("im", "I'm"),
    ("i'm", "I'm"),
    ("ive", "I've"),
    ("i've", "I've"),
    ("i'll", "I'll"),
    ("i'd", "I'd"),
    ("dont", "don't"),
    ("doesnt", "doesn't"),
    ("didnt", "didn't"),
    ("cant", "can't"),
    ("wont", "won't"),
    ("isnt", "isn't"),
    ("arent", "aren't"),
    ("wasnt", "wasn't"),
    ("werent", "weren't"),
    ("hasnt", "hasn't"),
    ("havent", "haven't"),
    ("hadnt", "hadn't"),
    ("wouldnt", "wouldn't"),
    ("couldnt", "couldn't"),
    ("shouldnt", "shouldn't"),
    ("mustnt", "mustn't"),
    ("neednt", "needn't"),
    ("aint", "ain't"),
    ("thats", "that's"),
    ("whats", "what's"),
    ("theres", "there's"),
    ("heres", "here's"),
    ("wheres", "where's"),
    ("whos", "who's"),
    ("hows", "how's"),
    ("youre", "you're"),
    ("theyre", "they're"),
    ("youve", "you've"),
    ("weve", "we've"),
    ("theyve", "they've"),
    ("youll", "you'll"),
    ("they'll", "they'll"),
    ("theyll", "they'll"),
    ("youd", "you'd"),
    ("theyd", "they'd"),
    ("couldve", "could've"),
    ("wouldve", "would've"),
    ("shouldve", "should've"),
    ("mightve", "might've"),
    ("mustve", "must've"),
];

/// Transposed apostrophe slips (`do'nt -> don't`, `does'nt -> doesn't`).
const TRANSPOSED_APOSTROPHES: &[(&str, &str)] = &[
    ("do'nt", "don't"),
    ("does'nt", "doesn't"),
    ("did'nt", "didn't"),
    ("ca'nt", "can't"),
    ("wo'nt", "won't"),
    ("is'nt", "isn't"),
    ("are'nt", "aren't"),
    ("was'nt", "wasn't"),
    ("were'nt", "weren't"),
    ("has'nt", "hasn't"),
    ("have'nt", "haven't"),
    ("hav'nt", "haven't"),
    ("had'nt", "hadn't"),
    ("would'nt", "wouldn't"),
    ("could'nt", "couldn't"),
    ("should'nt", "shouldn't"),
    ("must'nt", "mustn't"),
    ("ai'nt", "ain't"),
];

/// Unmerged two-word prose compounds (`alot -> a lot`, `aswell -> as well`).
const UNMERGED_COMPOUNDS: &[(&str, &str)] = &[
    ("alot", "a lot"),
    ("aswell", "as well"),
    ("atleast", "at least"),
    ("infront", "in front"),
    ("eachother", "each other"),
    ("infact", "in fact"),
    ("incase", "in case"),
    ("noone", "no one"),
    ("thankyou", "thank you"),
];

/// Closed-class function words allowed in automatic 2-word missed-space / thumb-clip splitting.
const FUNCTION_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "do", "for", "from", "had",
    "has", "have", "he", "her", "his", "if", "in", "into", "is", "it", "me", "my", "no", "not",
    "of", "on", "or", "our", "out", "she", "so", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "to", "up", "us", "was", "we", "were", "what", "when",
    "where", "which", "who", "will", "with", "would", "you", "your",
];

const CLITIC_SUFFIXES: &[&str] = &["t", "s", "m", "d", "ll", "ve", "re"];

/// Layer 1 PC-first English scorer backed by `EnglishLexicon`.
pub struct EnglishScorer {
    lexicon: EnglishLexicon,
}

impl EnglishScorer {
    pub fn new(lexicon: EnglishLexicon) -> Self {
        Self { lexicon }
    }

    #[cfg(feature = "bundled-en")]
    pub fn new_bundled() -> Result<Self, crate::lexicon::LexiconError> {
        Ok(Self::new(EnglishLexicon::from_bundled()?))
    }

    pub fn lexicon(&self) -> &EnglishLexicon {
        &self.lexicon
    }

    /// Evaluates whether a valid English word `token` committed after `prev` has a close
    /// keyboard/transposition neighbor (`known_typos` or `d <= 1`) that context strongly prefers.
    ///
    /// Returns `Some((candidate_lower, candidate_bigram, typed_bigram))` when Guard 3 asymmetry
    /// holds (`typed_bg < 0.25`, `cand_bg >= 0.50`, `cand_bg - typed_bg >= 0.35`).
    pub fn find_real_word_context_alternative(
        &self,
        token: &str,
        prev: &str,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
    ) -> Option<(String, f32, f32)> {
        let core_lower = token
            .trim_matches(|c: char| !c.is_alphabetic() && c != '\'' && c != '’')
            .to_lowercase();
        let prev_lower = prev
            .trim_matches(|c: char| !c.is_alphabetic() && c != '\'' && c != '’')
            .to_lowercase();
        if core_lower.chars().count() < 2 || prev_lower.is_empty() {
            return None;
        }
        let typed_bg = self.lexicon.bigram_score(&prev_lower, &core_lower);
        if typed_bg >= 0.25 {
            return None;
        }

        let mut best: Option<(String, f32)> = None;
        let mut consider = |cand_lower: &str| {
            if cand_lower == core_lower
                || cand_lower.contains(' ')
                || rules.is_pair_blocked(&core_lower, cand_lower)
            {
                return;
            }
            if weighted_keyboard_cost(&core_lower, cand_lower) > 1.25 {
                return;
            }
            let bg = self.lexicon.bigram_score(&prev_lower, cand_lower);
            if bg >= 0.50 && (bg - typed_bg) >= 0.35 {
                if best.as_ref().is_none_or(|(_, best_bg)| bg > *best_bg) {
                    best = Some((cand_lower.to_string(), bg));
                }
            }
        };

        if let Some(targets) = self.lexicon.lookup_known_typos(&core_lower) {
            for raw_t in targets {
                let mapped = self.lexicon.map_dialect(raw_t, config.dialect);
                consider(mapped);
            }
        }

        let mut fuzzy_hits = Vec::new();
        self.lexicon.search_fuzzy(&core_lower, 1, &mut fuzzy_hits);
        for (w, meta) in fuzzy_hits {
            if meta.is_offensive() || !meta.is_valid_for_dialect(config.dialect) {
                continue;
            }
            let mapped = self.lexicon.map_dialect(&w, config.dialect);
            consider(mapped);
        }

        best.map(|(cand, cand_bg)| (cand, cand_bg, typed_bg))
    }

    fn thresholds(aggr: Aggressiveness) -> (f32, f32) {
        match aggr {
            Aggressiveness::Conservative => (0.24, 0.09),
            Aggressiveness::Balanced => (0.14, 0.05),
            Aggressiveness::Aggressive => (0.06, 0.03),
        }
    }

    fn eval_internal(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
        collect_all: bool,
    ) -> (EvalOutcome, Vec<Candidate>) {
        if ctx.profile == AppProfile::Bypass || ctx.in_markdown_code {
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::Bypass,
                    do_not_learn: true,
                },
                Vec::new(),
            );
        }

        // Step 0: User snippets match raw trigger before casing/bypass rules if alphabetic
        if let Some(snip) = rules.lookup_snippet(token) {
            return (
                EvalOutcome::Autocorrect {
                    replacement: snip.to_string(),
                    kind: MatchKind::UserSnippet,
                    score: 10.0,
                    margin: 10.0,
                },
                vec![Candidate {
                    replacement: snip.to_string(),
                    score: 10.0,
                    edit_cost: 0.0,
                    freq: 255,
                    bigram_boost: 0.0,
                    kind: MatchKind::UserSnippet,
                }],
            );
        }

        let analysis = analyze_token(
            token,
            ctx.boundary_punct,
            ctx.at_sentence_start && !ctx.suppress_sentence_cap,
            config.fix_shift_slips,
            |w| self.lexicon.contains_any_english(w) || rules.is_personal_word(w),
        );

        let (leading_quotes, core, trailing_quotes, shift_slip_fixed, user_title_case) =
            match analysis {
                TokenAnalysis::Bypass => {
                    return (
                        EvalOutcome::Unchanged {
                            reason: MatchKind::Bypass,
                            do_not_learn: true,
                        },
                        Vec::new(),
                    )
                }
                TokenAnalysis::LeadingApostropheValid => {
                    return (
                        EvalOutcome::Unchanged {
                            reason: MatchKind::ValidWord,
                            do_not_learn: true,
                        },
                        Vec::new(),
                    )
                }
                TokenAnalysis::StretchedWord => {
                    return (
                        EvalOutcome::Unchanged {
                            reason: MatchKind::StretchedWord,
                            do_not_learn: true,
                        },
                        Vec::new(),
                    )
                }
                TokenAnalysis::Evaluate {
                    leading_quotes,
                    core,
                    trailing_quotes,
                    shift_slip_fixed,
                    user_title_case,
                } => (
                    leading_quotes,
                    core,
                    trailing_quotes,
                    shift_slip_fixed,
                    user_title_case,
                ),
            };

        let wrap = |body: &str| -> String {
            if leading_quotes.is_empty() && trailing_quotes.is_empty() {
                body.to_string()
            } else {
                format!("{leading_quotes}{body}{trailing_quotes}")
            }
        };

        let sentence_cap =
            ctx.at_sentence_start && config.auto_capitalize_sentence && !ctx.suppress_sentence_cap;
        let cap_initial = user_title_case || sentence_cap;
        let core_lower = core.to_lowercase();
        let prev_lower = ctx.prev_word.map(|p| p.to_lowercase());

        // Step 1: User contextual & unigram correction rules (`rules.toml`)
        if let Some(prev) = prev_lower.as_deref() {
            if let Some(target) = rules.lookup_context_rule(prev, &core_lower) {
                if !rules.is_pair_blocked(&core_lower, &target.to_lowercase()) {
                    let rep = wrap(&project_case(target, cap_initial));
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep.clone(),
                            kind: MatchKind::UserContextRule,
                            score: 9.0,
                            margin: 9.0,
                        },
                        vec![Candidate {
                            replacement: rep,
                            score: 9.0,
                            edit_cost: 0.0,
                            freq: 255,
                            bigram_boost: 1.0,
                            kind: MatchKind::UserContextRule,
                        }],
                    );
                }
            }
        }

        if let Some(target) = rules.lookup_correction(&core_lower) {
            if !rules.is_pair_blocked(&core_lower, &target.to_lowercase()) {
                let rep = wrap(&project_case(target, cap_initial));
                return (
                    EvalOutcome::Autocorrect {
                        replacement: rep.clone(),
                        kind: MatchKind::UserCorrectionRule,
                        score: 8.5,
                        margin: 8.5,
                    },
                    vec![Candidate {
                        replacement: rep,
                        score: 8.5,
                        edit_cost: 0.0,
                        freq: 255,
                        bigram_boost: 0.0,
                        kind: MatchKind::UserCorrectionRule,
                    }],
                );
            }
        }

        // Step 2: Pinky semicolon slip (`don;t -> don't`, `i;m -> I'm`)
        if core_lower.contains(';') {
            if let Some((stem, suffix)) = core_lower.split_once(';') {
                if !stem.is_empty() && CLITIC_SUFFIXES.contains(&suffix) {
                    let candidate_apos = format!("{stem}'{suffix}");
                    let resolved = if candidate_apos == "i'm" {
                        Some("I'm".to_string())
                    } else if candidate_apos == "i've" {
                        Some("I've".to_string())
                    } else if candidate_apos == "i'll" {
                        Some("I'll".to_string())
                    } else if candidate_apos == "i'd" {
                        Some("I'd".to_string())
                    } else if self.lexicon.contains_any_english(&candidate_apos) {
                        Some(candidate_apos)
                    } else {
                        None
                    };
                    if let Some(res) = resolved {
                        if !rules.is_pair_blocked(&core_lower, &res.to_lowercase()) {
                            let rep = wrap(&project_case(&res, cap_initial));
                            return (
                                EvalOutcome::Autocorrect {
                                    replacement: rep.clone(),
                                    kind: MatchKind::SemicolonContraction,
                                    score: 5.0,
                                    margin: 5.0,
                                },
                                vec![Candidate {
                                    replacement: rep,
                                    score: 5.0,
                                    edit_cost: 0.4,
                                    freq: 200,
                                    bigram_boost: 0.0,
                                    kind: MatchKind::SemicolonContraction,
                                }],
                            );
                        }
                    }
                }
            }
            // Any other `;` inside a token is not a clitic contraction -> bypass cleanly
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::Bypass,
                    do_not_learn: true,
                },
                Vec::new(),
            );
        }

        // Step 2b: Transposed apostrophes (`do'nt -> don't`, `does'nt -> doesn't`)
        if core_lower.contains('\'') {
            for &(bad, good) in TRANSPOSED_APOSTROPHES {
                if core_lower == bad && !rules.is_pair_blocked(&core_lower, &good.to_lowercase()) {
                    let rep = wrap(&project_case(good, cap_initial));
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep.clone(),
                            kind: MatchKind::TransposedApostrophe,
                            score: 5.0,
                            margin: 5.0,
                        },
                        vec![Candidate {
                            replacement: rep,
                            score: 5.0,
                            edit_cost: 0.45,
                            freq: 200,
                            bigram_boost: 0.0,
                            kind: MatchKind::TransposedApostrophe,
                        }],
                    );
                }
            }
        }

        // Step 3: Personal dictionary / undo shield check BEFORE contractions so user shields win!
        let in_personal = rules.is_personal_word(&core_lower);
        if in_personal || rules.is_undo_shielded(&core_lower) {
            if shift_slip_fixed {
                let rep = wrap(&core);
                return (
                    EvalOutcome::Autocorrect {
                        replacement: rep,
                        kind: MatchKind::ShiftSlip,
                        score: 4.0,
                        margin: 4.0,
                    },
                    Vec::new(),
                );
            }
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    // If `in_personal` is already true, no need to stage it again; if it is only
                    // `1x` undo-shielded (`in_personal == false`), set `do_not_learn: false` so its
                    // next untouched use graduates it into `personal_dict`!
                    do_not_learn: in_personal,
                },
                Vec::new(),
            );
        }

        // Step 3b: Standalone `i -> I`, missing-apostrophe contractions (`doesnt -> doesn't`, `im -> I'm`),
        // and unmerged compounds (`alot -> a lot`)
        if config.auto_capitalize_i && core == "i" && !rules.is_pair_blocked("i", "i") {
            let rep = wrap("I");
            return (
                EvalOutcome::Autocorrect {
                    replacement: rep.clone(),
                    kind: MatchKind::Contraction,
                    score: 4.5,
                    margin: 4.5,
                },
                vec![Candidate {
                    replacement: rep,
                    score: 4.5,
                    edit_cost: 0.2,
                    freq: 220,
                    bigram_boost: 0.0,
                    kind: MatchKind::Contraction,
                }],
            );
        }

        if config.enable_contractions {
            for &(raw_c, expanded) in UNCONDITIONAL_CONTRACTIONS {
                if core_lower == raw_c {
                    let exp_lower = expanded.to_lowercase();
                    if !rules.is_pair_blocked(&core_lower, &exp_lower) {
                        let projected = project_case(expanded, cap_initial);
                        let rep = wrap(&projected);
                        if rep != token {
                            return (
                                EvalOutcome::Autocorrect {
                                    replacement: rep.clone(),
                                    kind: MatchKind::Contraction,
                                    score: 4.5,
                                    margin: 4.5,
                                },
                                vec![Candidate {
                                    replacement: rep,
                                    score: 4.5,
                                    edit_cost: 0.3,
                                    freq: 210,
                                    bigram_boost: 0.0,
                                    kind: MatchKind::Contraction,
                                }],
                            );
                        }
                    }
                }
            }
        }

        if config.enable_missed_space_split {
            for &(merged, split_pair) in UNMERGED_COMPOUNDS {
                if core_lower == merged && !rules.is_pair_blocked(&core_lower, split_pair) {
                    let rep = wrap(&project_case(split_pair, cap_initial));
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep.clone(),
                            kind: MatchKind::UnmergedCompound,
                            score: 4.2,
                            margin: 4.2,
                        },
                        vec![Candidate {
                            replacement: rep,
                            score: 4.2,
                            edit_cost: 0.4,
                            freq: 200,
                            bigram_boost: 0.0,
                            kind: MatchKind::UnmergedCompound,
                        }],
                    );
                }
            }
        }

        // Step 3c: Built-in Shorthand Auto-Expansion (`pls -> please`, `rn -> right now`, `idk -> I don't know`)
        if config.expand_shorthand && !shift_slip_fixed {
            if let Some(expanded) = crate::lexicon::lookup_english_shorthand(&core_lower) {
                let exp_lower = expanded.to_lowercase();
                if !rules.is_pair_blocked(&core_lower, &exp_lower) {
                    let projected =
                        crate::vi_telex::project_shorthand_case(&core, expanded, cap_initial);
                    let rep = wrap(&projected);
                    if rep != token {
                        return (
                            EvalOutcome::Autocorrect {
                                replacement: rep.clone(),
                                kind: MatchKind::ShorthandExpansion,
                                score: 4.4,
                                margin: 4.4,
                            },
                            vec![Candidate {
                                replacement: rep,
                                score: 4.4,
                                edit_cost: 0.2,
                                freq: 215,
                                bigram_boost: 0.0,
                                kind: MatchKind::ShorthandExpansion,
                            }],
                        );
                    }
                }
            }
        }

        // Step 4: Strict dialect rewrite (`EnUs`: `colour -> color`; `EnGb`: `color -> colour`)
        if let Some(dialect_target) = self
            .lexicon
            .strict_dialect_rewrite(&core_lower, config.dialect)
        {
            if dialect_target != core_lower && !rules.is_pair_blocked(&core_lower, dialect_target) {
                let rep = wrap(&project_case(dialect_target, cap_initial));
                return (
                    EvalOutcome::Autocorrect {
                        replacement: rep.clone(),
                        kind: MatchKind::DialectNormalization,
                        score: 4.0,
                        margin: 4.0,
                    },
                    vec![Candidate {
                        replacement: rep,
                        score: 4.0,
                        edit_cost: 0.3,
                        freq: 180,
                        bigram_boost: 0.0,
                        kind: MatchKind::DialectNormalization,
                    }],
                );
            }
        }

        // Step 4b: Valid-Word Fast Path (`0%` false-positive shield on real US/UK/tech/offensive words)
        if self.lexicon.contains_valid(&core_lower, config.dialect) {
            if shift_slip_fixed {
                let rep = wrap(&core);
                if rep != token {
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep.clone(),
                            kind: MatchKind::ShiftSlip,
                            score: 4.0,
                            margin: 4.0,
                        },
                        vec![Candidate {
                            replacement: rep,
                            score: 4.0,
                            edit_cost: 0.2,
                            freq: 180,
                            bigram_boost: 0.0,
                            kind: MatchKind::ShiftSlip,
                        }],
                    );
                }
            }

            // Proper Noun auto-capitalization (english -> English, monday -> Monday)
            if config.fix_proper_nouns && !shift_slip_fixed {
                if let Some(proper) = self.lexicon.lookup_proper_noun(&core_lower) {
                    if core != proper && !rules.is_pair_blocked(&core_lower, &proper.to_lowercase()) {
                        let rep = wrap(proper);
                        if rep != token {
                            return (
                                EvalOutcome::Autocorrect {
                                    replacement: rep.clone(),
                                    kind: MatchKind::ProperNoun,
                                    score: 4.5,
                                    margin: 4.5,
                                },
                                vec![Candidate {
                                    replacement: rep,
                                    score: 4.5,
                                    edit_cost: 0.2,
                                    freq: 200,
                                    bigram_boost: 0.0,
                                    kind: MatchKind::ProperNoun,
                                }],
                            );
                        }
                    }
                }
            }
            if sentence_cap && !user_title_case {
                let rep = wrap(&project_case(&core, true));
                if rep != token {
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep,
                            kind: MatchKind::ShiftSlip,
                            score: 3.5,
                            margin: 3.5,
                        },
                        Vec::new(),
                    );
                }
            }
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    do_not_learn: true,
                },
                Vec::new(),
            );
        }

        // Check normalized apostrophe lookup (`that's` after `THat's` normalization)
        if core_lower.contains('’') {
            let ascii_apos = core_lower.replace('’', "'");
            if self.lexicon.contains_valid(&ascii_apos, config.dialect) {
                if shift_slip_fixed {
                    let rep = wrap(&core);
                    return (
                        EvalOutcome::Autocorrect {
                            replacement: rep,
                            kind: MatchKind::ShiftSlip,
                            score: 4.0,
                            margin: 4.0,
                        },
                        Vec::new(),
                    );
                }
                return (
                    EvalOutcome::Unchanged {
                        reason: MatchKind::ValidWord,
                        do_not_learn: true,
                    },
                    Vec::new(),
                );
            }
        }

        // Step 5: Non-Word Decoder
        let char_len = core_lower.chars().count();
        if char_len <= 1 {
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::Bypass,
                    do_not_learn: true,
                },
                Vec::new(),
            );
        }

        let length_damping = 1.0 / (1.0 + 0.10 * (char_len.saturating_sub(3).min(7) as f32));
        let mut pool: HashMap<String, Candidate> = HashMap::new();

        let push_candidate = |pool: &mut HashMap<String, Candidate>,
                              cand_lower: &str,
                              freq: u8,
                              bonus: f32,
                              kind: MatchKind,
                              custom_cost: Option<f32>| {
            if cand_lower == core_lower || rules.is_pair_blocked(&core_lower, cand_lower) {
                return;
            }
            let edit_cost =
                custom_cost.unwrap_or_else(|| weighted_keyboard_cost(&core_lower, cand_lower));
            let bg = match prev_lower.as_deref() {
                Some(p) => {
                    let first_word = cand_lower.split(' ').next().unwrap_or(cand_lower);
                    self.lexicon.bigram_score(p, first_word)
                }
                None => 0.0,
            };
            let trouble = rules.trouble_word_boost(cand_lower);
            let anagram_bonus = if cand_lower.len() == core_lower.len()
                && core_lower.is_ascii()
                && cand_lower.is_ascii()
                && multiset_overlap(&core_lower, cand_lower) >= 0.999
            {
                0.20
            } else {
                0.0
            };
            let freq_term = 1.25 * (freq as f32 / 255.0);
            let bg_term = 0.95 * bg;
            let score = freq_term + bg_term + bonus + anagram_bonus + trouble
                - (length_damping * edit_cost);

            let formatted = wrap(&project_case(cand_lower, cap_initial));
            match pool.get_mut(cand_lower) {
                Some(existing) => {
                    if score > existing.score {
                        *existing = Candidate {
                            replacement: formatted,
                            score,
                            edit_cost,
                            freq,
                            bigram_boost: bg,
                            kind,
                        };
                    }
                }
                None => {
                    pool.insert(
                        cand_lower.to_string(),
                        Candidate {
                            replacement: formatted,
                            score,
                            edit_cost,
                            freq,
                            bigram_boost: bg,
                            kind,
                        },
                    );
                }
            }
        };

        // 5a. Curated human keyboard smashes (`crate-ci/typos` `95,924` pairs)
        let mut single_curated_hit = false;
        if let Some(targets) = self.lexicon.lookup_known_typos(&core_lower) {
            let raw_targets: Vec<&str> = targets.collect();
            single_curated_hit = raw_targets.len() == 1;
            for (rank_idx, raw_t) in raw_targets.iter().copied().enumerate() {
                let rank_bonus = if single_curated_hit {
                    0.50
                } else {
                    match rank_idx {
                        0 => 0.52,
                        1 => 0.50,
                        _ => 0.10,
                    }
                };
                let mapped = self.lexicon.map_dialect(raw_t, config.dialect);
                let freq = self
                    .lexicon
                    .lookup_word(mapped)
                    .map(|m| m.effective_freq(config.dialect))
                    .unwrap_or(145);
                push_candidate(
                    &mut pool,
                    mapped,
                    freq,
                    rank_bonus,
                    MatchKind::KnownTypoDict,
                    None,
                );
            }
        }

        // Fast-path: an unambiguous curated typo entry (`chagnes -> changes`, `soemtimes -> sometimes`)
        // does not need expensive distance-2 DFA scanning over 160k lexicon words!
        let skip_heavy_dfa =
            single_curated_hit && !collect_all && pool.values().any(|c| c.score >= 0.35);

        // For 2-char unknown words (`uh`, `ko`, `iu`), never run statistical fuzzy guessing!
        if char_len == 2 && pool.is_empty() {
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::Bypass,
                    do_not_learn: false,
                },
                Vec::new(),
            );
        }

        // 5b. Mechanical switch chatter / key-bounce pre-filter (`thhe -> the`, `reallly -> really`, `wword -> word`)
        if char_len >= 3 && !skip_heavy_dfa {
            for collapsed in generate_chatter_collapses(&core_lower) {
                let mapped = self.lexicon.map_dialect(&collapsed, config.dialect);
                if let Some(meta) = self.lexicon.lookup_word(mapped) {
                    if !meta.is_offensive() && meta.is_valid_for_dialect(config.dialect) {
                        push_candidate(
                            &mut pool,
                            mapped,
                            meta.effective_freq(config.dialect),
                            0.24,
                            MatchKind::SwitchChatter,
                            Some(0.44),
                        );
                    }
                }
            }
        }

        // 5c. 2-word missed-space & thumb-spacebar bottom-row clips (`ofthe`, `inbthe -> in the`)
        if config.enable_missed_space_split
            && !skip_heavy_dfa
            && core_lower.is_ascii()
            && (4..=14).contains(&char_len)
        {
            self.collect_space_splits(&core_lower, config, &mut |c, f, b, k, cost| {
                push_candidate(&mut pool, c, f, b, k, cost);
            });
        }

        // 5d. Two-stage Damerau–Levenshtein FST search:
        // Stage 1: fast `d <= 1` (`< 15 µs`, catches 95% of unseen transpositions & adjacent slips like `woryr -> worry`).
        // Stage 2: `d <= 2` ONLY if Stage 1 produced no confident match!
        if char_len >= 3 && !skip_heavy_dfa {
            let run_stage = |dist: u8, pool: &mut HashMap<String, Candidate>| {
                let mut fuzzy_hits = Vec::new();
                self.lexicon
                    .search_fuzzy(&core_lower, dist, &mut fuzzy_hits);
                for (w, meta) in fuzzy_hits {
                    let mapped = self.lexicon.map_dialect(&w, config.dialect);
                    let freq = if mapped == w {
                        meta.effective_freq(config.dialect)
                    } else {
                        self.lexicon
                            .lookup_word(mapped)
                            .map(|m| m.effective_freq(config.dialect))
                            .unwrap_or_else(|| meta.effective_freq(config.dialect))
                    };
                    if char_len <= 4 {
                        let cost = weighted_keyboard_cost(&core_lower, mapped);
                        let bg = prev_lower
                            .as_deref()
                            .map(|p| self.lexicon.bigram_score(p, mapped))
                            .unwrap_or(0.0);
                        if char_len == 3 && cost > 0.65 && bg < 0.82 {
                            continue;
                        }
                        if char_len == 4 && cost > 1.05 && bg < 0.85 {
                            continue;
                        }
                    }
                    push_candidate(pool, mapped, freq, 0.0, MatchKind::FuzzyLexicon, None);
                }
            };

            run_stage(1, &mut pool);

            let best_d1 = pool
                .values()
                .map(|c| c.score)
                .fold(f32::NEG_INFINITY, f32::max);
            let allow_d2 = char_len >= 5 || prev_lower.is_some();
            if allow_d2 && (best_d1 < 0.25 || collect_all) {
                run_stage(2, &mut pool);
            }
        }

        // 5e. `POPCNT` Character-Multiset Smash Rescue for heavy scrambles (`hstiautoin -> situation`)
        let best_so_far = pool
            .values()
            .map(|c| c.score)
            .fold(f32::NEG_INFINITY, f32::max);
        if char_len >= 7 && core_lower.is_ascii() && (best_so_far < 0.45 || collect_all) {
            let q_mask = ascii_letter_mask(&core_lower);
            let q_len = char_len as i16;
            let q_bytes = core_lower.as_bytes();
            for entry in self.lexicon.smash_entries() {
                if ((entry.len as i16) - q_len).abs() > 2 {
                    continue;
                }
                if (q_mask ^ entry.mask).count_ones() > 2 {
                    continue;
                }
                let eb = entry.word.as_bytes();
                let start_ok = eb[0] == q_bytes[0]
                    || (q_bytes.len() > 1 && eb[0] == q_bytes[1])
                    || (eb.len() > 1 && eb[1] == q_bytes[0]);
                if !start_ok {
                    continue;
                }
                let overlap = multiset_overlap(&core_lower, &entry.word);
                if overlap < 0.80 {
                    continue;
                }
                let max_dl = if char_len >= 8 && overlap >= 0.88 {
                    4
                } else {
                    3
                };
                let dl = damerau_levenshtein_chars(&core_lower, &entry.word);
                if dl > max_dl {
                    continue;
                }
                let mapped = self.lexicon.map_dialect(&entry.word, config.dialect);
                // When >=88% of letters match in a long word (`hstiautoin -> situation`), or >=85%
                // on 12+ char words (`implementintion -> implementation`), cap effective scramble cost
                // so multiple local slips don't drown out an unmistakable character-bag match.
                let raw_cost = weighted_keyboard_cost(&core_lower, mapped);
                let high_overlap = overlap >= 0.88 || (char_len >= 12 && overlap >= 0.85);
                let damped_scramble_cost = if high_overlap {
                    raw_cost.min(1.15)
                } else {
                    raw_cost
                };
                let rescue_bonus = 0.30 * overlap;
                push_candidate(
                    &mut pool,
                    mapped,
                    entry.freq,
                    rescue_bonus,
                    MatchKind::SmashRescue,
                    Some(damped_scramble_cost),
                );
            }
        }

        // 5f. Personal dictionary words (`personal_dict` in `rules.toml`)
        if char_len >= 3 && !skip_heavy_dfa {
            if let Some(pwords) = rules.personal_words() {
                for pword in pwords {
                    let plen = pword.chars().count();
                    if plen < 3 || plen.abs_diff(char_len) > 2 {
                        continue;
                    }
                    let dl = damerau_levenshtein_chars(&core_lower, pword);
                    let overlap = if core_lower.is_ascii() && pword.is_ascii() {
                        multiset_overlap(&core_lower, pword)
                    } else {
                        0.0
                    };
                    let max_dl = if char_len >= 8 && overlap >= 0.85 {
                        3
                    } else if char_len >= 5 {
                        2
                    } else {
                        1
                    };
                    if dl > max_dl {
                        continue;
                    }
                    let cost = weighted_keyboard_cost(&core_lower, pword);
                    if char_len == 3 && cost > 0.65 {
                        continue;
                    }
                    if char_len == 4 && cost > 1.05 {
                        continue;
                    }
                    if cost > 1.45 {
                        continue;
                    }
                    push_candidate(
                        &mut pool,
                        pword,
                        165,
                        0.10,
                        MatchKind::PersonalDict,
                        Some(cost),
                    );
                }
            }
        }

        let mut ranked: Vec<Candidate> = pool.into_values().collect();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.freq.cmp(&a.freq))
        });

        let (t_auto, min_margin) = Self::thresholds(config.aggressiveness);
        if let Some(top) = ranked.first() {
            let second_score = ranked.get(1).map(|c| c.score).unwrap_or(0.0);
            let margin = top.score - second_score.max(0.0);
            // Curated `KnownTypoDict` single-target fixes only need positive score, whereas fuzzy guesses require margin
            let passes_margin = margin >= min_margin
                || (top.kind == MatchKind::KnownTypoDict && (top.score - second_score) >= 0.04);
            if top.score >= t_auto && passes_margin {
                let outcome = EvalOutcome::Autocorrect {
                    replacement: top.replacement.clone(),
                    kind: top.kind,
                    score: top.score,
                    margin,
                };
                if !collect_all {
                    ranked.truncate(5);
                }
                return (outcome, ranked);
            }
        }

        ranked.truncate(5);
        (
            EvalOutcome::Unchanged {
                reason: MatchKind::Bypass,
                do_not_learn: false,
            },
            ranked,
        )
    }

    fn collect_space_splits(
        &self,
        core_lower: &str,
        config: &ScorerConfig,
        push_candidate: &mut impl FnMut(&str, u8, f32, MatchKind, Option<f32>),
    ) {
        let n = core_lower.len();
        // 1. Direct missed space: `w1 + w2` (`ofthe -> of the`, `inthe -> in the`)
        for split in 1..n {
            let w1 = &core_lower[..split];
            let w2 = &core_lower[split..];
            if w1.len() < 2 && w1 != "a" && w1 != "i" {
                continue;
            }
            if w2.len() < 2 && w2 != "a" {
                continue;
            }
            if !FUNCTION_WORDS.contains(&w1) && !FUNCTION_WORDS.contains(&w2) {
                continue;
            }
            let (Some(m1), Some(m2)) = (self.lexicon.lookup_word(w1), self.lexicon.lookup_word(w2))
            else {
                continue;
            };
            if !m1.is_valid_for_dialect(config.dialect) || !m2.is_valid_for_dialect(config.dialect)
            {
                continue;
            }
            let bg = self.lexicon.bigram_score_ids(m1.word_id, m2.word_id);
            if bg < 0.72 {
                continue;
            }
            let min_f = m1
                .effective_freq(config.dialect)
                .min(m2.effective_freq(config.dialect));
            let w1_fmt = if w1 == "i" && config.auto_capitalize_i {
                "I"
            } else {
                w1
            };
            let joined = format!("{w1_fmt} {w2}");
            push_candidate(
                &joined,
                min_f,
                0.25 * bg,
                MatchKind::MissedSpaceSplit,
                Some(0.55),
            );
        }

        // 2. Thumb-spacebar bottom-row clip: `w1 + [b|n|v|m|c] + w2` (`inbthe -> in the`, `andnthe -> and the`)
        let bytes = core_lower.as_bytes();
        for mid in 1..n.saturating_sub(1) {
            if !matches!(bytes[mid], b'b' | b'n' | b'v' | b'm' | b'c') {
                continue;
            }
            let w1 = &core_lower[..mid];
            let w2 = &core_lower[mid + 1..];
            if !FUNCTION_WORDS.contains(&w1) || !FUNCTION_WORDS.contains(&w2) {
                continue;
            }
            let (Some(m1), Some(m2)) = (self.lexicon.lookup_word(w1), self.lexicon.lookup_word(w2))
            else {
                continue;
            };
            let bg = self.lexicon.bigram_score_ids(m1.word_id, m2.word_id);
            if bg < 0.75 {
                continue;
            }
            let min_f = m1
                .effective_freq(config.dialect)
                .min(m2.effective_freq(config.dialect));
            let joined = format!("{w1} {w2}");
            push_candidate(
                &joined,
                min_f,
                0.28 * bg,
                MatchKind::MissedSpaceSplit,
                Some(0.52),
            );
        }
    }
}

impl LanguageScorer for EnglishScorer {
    fn evaluate(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
    ) -> EvalOutcome {
        self.eval_internal(token, ctx, config, rules, false).0
    }

    fn explain(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        config: &ScorerConfig,
        rules: &dyn UserRuleView,
    ) -> Explanation {
        let (outcome, top_candidates) = self.eval_internal(token, ctx, config, rules, true);
        Explanation {
            input: token.to_string(),
            prev_word: ctx.prev_word.map(str::to_string),
            boundary_punct: ctx.boundary_punct,
            outcome,
            top_candidates,
        }
    }
}

/// Generates switch-chatter / key-bounce deduplicated variants (`wword -> word`, `thhe -> the`, `reallly -> really`).
fn generate_chatter_collapses(lower: &str) -> Vec<String> {
    let chars: Vec<char> = lower.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    if n < 3 {
        return out;
    }

    // 1. Collapse word-initial double consonant (`wword -> word`, `nnext -> next`, `tthe -> the`)
    if chars[0] == chars[1] && !matches!(chars[0], 'a' | 'e' | 'i' | 'o' | 'u') {
        out.push(chars[1..].iter().collect());
    }

    // 2. Collapse any `>= 3` run down to 2 chars (`reallly -> really`) and 1 char (`thhhe -> the`)
    let mut collapsed_to_2 = String::with_capacity(n);
    let mut collapsed_to_1 = String::with_capacity(n);
    let mut had_triple = false;
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        let mut run = 1;
        while i + run < n && chars[i + run] == ch {
            run += 1;
        }
        if run >= 3 {
            had_triple = true;
            collapsed_to_2.push(ch);
            collapsed_to_2.push(ch);
            collapsed_to_1.push(ch);
        } else {
            for _ in 0..run {
                collapsed_to_2.push(ch);
                collapsed_to_1.push(ch);
            }
        }
        i += run;
    }
    if had_triple {
        out.push(collapsed_to_2);
        out.push(collapsed_to_1);
    }

    // 3. Collapse each single adjacent double-char run one at a time (`thhe -> the`, `ttoally` no, `prooblem -> problem`)
    for idx in 0..(n - 1) {
        if chars[idx] == chars[idx + 1] {
            let mut s = String::with_capacity(n - 1);
            for (k, &c) in chars.iter().enumerate() {
                if k != idx {
                    s.push(c);
                }
            }
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }

    out
}
