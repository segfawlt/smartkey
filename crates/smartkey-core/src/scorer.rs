use std::collections::{BTreeSet, HashMap};

use crate::casing::{analyze_token_with_rules, project_case, TokenAnalysis};
use crate::keyboard::{
    ascii_letter_mask, damerau_levenshtein_chars, multiset_overlap, weighted_keyboard_cost,
};
use crate::lexicon::{PackLexicon, ReplacementKind};
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
    fn lookup_personal_canonical(&self, lower_word: &str) -> Option<&str> {
        let _ = lower_word;
        None
    }
    fn is_undo_shielded(&self, lower_word: &str) -> bool {
        let _ = lower_word;
        false
    }
    fn is_pair_blocked(&self, typed_lower: &str, candidate_lower: &str) -> bool {
        let _ = (typed_lower, candidate_lower);
        false
    }
    /// True when the user pinned `lower_word` (`pinned = [...]` in `rules.toml`): its curated
    /// capitalization (`english -> English`) always applies, ignoring blocked pairs and undo shields.
    fn is_pinned(&self, lower_word: &str) -> bool {
        let _ = lower_word;
        false
    }
    fn trouble_word_boost(&self, candidate_lower: &str) -> f32 {
        let _ = candidate_lower;
        0.0
    }
    /// Bounded personal-dict usage frequency (`175..=225`) used by Step 5f fuzzy rescue.
    fn personal_word_freq(&self, lower_word: &str) -> u8 {
        let _ = lower_word;
        175
    }
    fn personal_words(&self) -> Option<&BTreeSet<String>> {
        None
    }
    fn lang_personal_words(&self) -> Option<&BTreeSet<String>> {
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

/// True when `(typed_lower, expanded_lower)` is one of the curated unconditional contraction
/// pairs in the bundled English pack (`dont -> don't`). These are near-100% corrections — the
/// typed form is never a valid word — so a single accidental rejection must not permanently block
/// them (see `learner::reject_pair_permanently` for the `2x` evidence requirement).
pub fn is_unconditional_contraction_pair(typed_lower: &str, expanded_lower: &str) -> bool {
    crate::lexicon::bundled_en_lexicon()
        .is_some_and(|lex| lex.is_contraction_pair(typed_lower, expanded_lower))
}

/// True when `candidate_cased` is eligible to hold canonical uppercase in `personal_dict`
/// (internal uppercase like `PyTorch`, non-ASCII initial uppercase like `Đắk`, novel OOV words
/// like `Domixi`, or lexicon `FLAG_TECH` words like `Hyprland`). Ordinary non-tech lexicon
/// prose words (`It`, `The`, `Hello`, `Polish`) return `false` so sentence-start TitleCase
/// never hijacks mid-sentence prose.
pub fn allows_personal_proper_case(
    lexicon: Option<&PackLexicon>,
    lower_word: &str,
    candidate_cased: &str,
) -> bool {
    let mut chars = candidate_cased.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !candidate_cased.chars().any(char::is_uppercase) {
        return false;
    }
    if chars.any(char::is_uppercase) || (!first.is_ascii() && first.is_uppercase()) {
        return true;
    }
    match lexicon {
        None => true,
        Some(lex) => {
            !lex.contains_any_variant_or_elided(lower_word)
                || lex.lookup_word(lower_word).is_some_and(|m| m.is_tech())
        }
    }
}

/// True when `(typed_lower, rejected_lower)` requires `2x` rejection evidence for the given pack:
/// curated contractions from `lexicon.rules()` (`dont -> don't`), standalone `i -> I`, and
/// curated proper nouns (`english -> English`) — NOT machine-learned `personal_dict` casing
/// (`domixi -> Domixi` or accidental `it -> It`), which must block at `1x`.
pub fn is_staged_rejection_pair_for(
    lexicon: Option<&PackLexicon>,
    lang: &str,
    typed_lower: &str,
    rejected_lower: &str,
) -> bool {
    let resolved_lex = match lexicon {
        Some(lex) => Some(lex),
        None if lang == "en" => crate::lexicon::bundled_en_lexicon(),
        None => None,
    };
    let Some(lex) = resolved_lex else {
        return false;
    };
    if typed_lower == rejected_lower {
        return (lex.rules().capitalize_standalone_i && typed_lower == "i")
            || lex.lookup_proper_noun(typed_lower).is_some();
    }
    lex.is_contraction_pair(typed_lower, rejected_lower)
}

/// Pairs whose permanent block needs `2x` rejection evidence instead of an immediate `1x` block:
/// curated contractions (`dont -> don't`) and curated case-only corrections (`english -> English`).
/// Both come from curated lists, so one accidental BackSpace must not permanently disable them
/// (see `learner::reject_pair_permanently`).
pub fn is_staged_rejection_pair(typed_lower: &str, rejected_lower: &str) -> bool {
    is_staged_rejection_pair_for(None, "en", typed_lower, rejected_lower)
}

/// Layer 1 PC-first scorer backed by `PackLexicon`.
pub struct LatinScorer {
    lexicon: PackLexicon,
}

/// Backwards-compatible alias for [`LatinScorer`].
pub type EnglishScorer = LatinScorer;

impl LatinScorer {
    pub fn new(lexicon: PackLexicon) -> Self {
        Self { lexicon }
    }

    #[cfg(feature = "bundled-en")]
    pub fn new_bundled() -> Result<Self, crate::lexicon::LexiconError> {
        Ok(Self::new(PackLexicon::from_bundled()?))
    }

    pub fn lexicon(&self) -> &PackLexicon {
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
            if bg >= 0.50
                && (bg - typed_bg) >= 0.35
                && best.as_ref().is_none_or(|(_, best_bg)| bg > *best_bg)
            {
                best = Some((cand_lower.to_string(), bg));
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
            if meta.is_offensive() || !self.lexicon.is_valid_meta(meta, config.dialect) {
                continue;
            }
            let mapped = self.lexicon.map_dialect(&w, config.dialect);
            consider(mapped);
        }

        best.map(|(cand, cand_bg)| (cand, cand_bg, typed_bg))
    }

    pub const fn thresholds(aggr: Aggressiveness) -> (f32, f32) {
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

        let lang_rules = self.lexicon.rules();
        let analysis = analyze_token_with_rules(
            token,
            ctx.boundary_punct,
            ctx.at_sentence_start && !ctx.suppress_sentence_cap,
            config.fix_shift_slips,
            &lang_rules.casing,
            |w| self.lexicon.contains_any_variant_or_elided(w) || rules.is_personal_word(w),
        );

        let (
            leading_quotes,
            core,
            trailing_quotes,
            shift_slip_fixed,
            user_title_case,
            is_mixed_case_word,
        ) = match analysis {
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
            TokenAnalysis::MixedCaseWord {
                leading_quotes,
                core,
                trailing_quotes,
            } => (leading_quotes, core, trailing_quotes, false, false, true),
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
                false,
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

        let format_rule_target = |target: &str| -> String {
            if target.chars().any(char::is_uppercase) {
                wrap(target)
            } else {
                wrap(&project_case(target, cap_initial))
            }
        };

        // Step 1: User contextual & unigram correction rules (`rules.toml`)
        if let Some(prev) = prev_lower.as_deref() {
            if let Some(target) = rules.lookup_context_rule(prev, &core_lower) {
                if !rules.is_pair_blocked(&core_lower, &target.to_lowercase()) {
                    let rep = format_rule_target(target);
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
                let rep = format_rule_target(target);
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
                if !stem.is_empty() && lang_rules.clitics.suffixes.iter().any(|s| s == suffix) {
                    let candidate_apos = format!("{stem}'{suffix}");
                    let resolved = if let Some(rule) = lang_rules.replacements.iter().find(|r| {
                        r.kind == ReplacementKind::Contraction && r.typed == candidate_apos
                    }) {
                        Some(rule.fix.clone())
                    } else if self.lexicon.contains_any_variant(&candidate_apos) {
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
        if core_lower.contains('\'') && !is_mixed_case_word {
            for r in lang_rules
                .replacements
                .iter()
                .filter(|r| r.kind == ReplacementKind::TransposedApostrophe)
            {
                if core_lower == r.typed
                    && !rules.is_pair_blocked(&core_lower, &r.fix.to_lowercase())
                {
                    let rep = wrap(&project_case(&r.fix, cap_initial));
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
                let canon_or_core = rules
                    .lookup_personal_canonical(&core_lower)
                    .filter(|c| c.chars().any(char::is_uppercase))
                    .unwrap_or(&core);
                let rep = wrap(canon_or_core);
                if rep != token {
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
            }
            if in_personal && !rules.is_undo_shielded(&core_lower) && config.fix_proper_nouns {
                if let Some(canon) = rules.lookup_personal_canonical(&core_lower) {
                    if canon.chars().any(char::is_uppercase)
                        && core != canon
                        && (rules.is_pinned(&core_lower)
                            || allows_personal_proper_case(Some(&self.lexicon), &core_lower, canon))
                        && !rules.is_pair_blocked(&core_lower, &canon.to_lowercase())
                    {
                        let rep = wrap(canon);
                        if rep != token {
                            return (
                                EvalOutcome::Autocorrect {
                                    replacement: rep.clone(),
                                    kind: MatchKind::PersonalDict,
                                    score: 4.5,
                                    margin: 4.5,
                                },
                                vec![Candidate {
                                    replacement: rep,
                                    score: 4.5,
                                    edit_cost: 0.2,
                                    freq: 210,
                                    bigram_boost: 0.0,
                                    kind: MatchKind::PersonalDict,
                                }],
                            );
                        }
                    }
                }
            }
            let allow_personal_case_upgrade = in_personal
                && !shift_slip_fixed
                && (is_mixed_case_word || (user_title_case && !ctx.at_sentence_start))
                && allows_personal_proper_case(Some(&self.lexicon), &core_lower, &core)
                && rules
                    .lookup_personal_canonical(&core_lower)
                    .is_some_and(|c| !c.chars().any(char::is_uppercase));
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    // If `in_personal` is already true, no need to stage it again unless upgrading
                    // a lowercase `personal_dict` entry to canonical case (`"domixi"` -> `"Domixi"`);
                    // if `1x` undo-shielded (`in_personal == false`), set `do_not_learn: false` so
                    // its next untouched use graduates it into `personal_dict`!
                    do_not_learn: in_personal && !allow_personal_case_upgrade,
                },
                Vec::new(),
            );
        }

        if is_mixed_case_word {
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    do_not_learn: false,
                },
                Vec::new(),
            );
        }

        // Step 3b: Standalone `i -> I`, missing-apostrophe contractions (`doesnt -> doesn't`, `im -> I'm`),
        // and unmerged compounds (`alot -> a lot`)
        if config.auto_capitalize_i
            && lang_rules.capitalize_standalone_i
            && core == "i"
            && !rules.is_pair_blocked("i", "i")
        {
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
            for r in lang_rules
                .replacements
                .iter()
                .filter(|r| r.kind == ReplacementKind::Contraction)
            {
                if core_lower == r.typed {
                    let exp_lower = r.fix.to_lowercase();
                    if !rules.is_pair_blocked(&core_lower, &exp_lower) {
                        let projected = project_case(&r.fix, cap_initial);
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
            for r in lang_rules
                .replacements
                .iter()
                .filter(|r| r.kind == ReplacementKind::UnmergedCompound)
            {
                if core_lower == r.typed && !rules.is_pair_blocked(&core_lower, &r.fix) {
                    let rep = wrap(&project_case(&r.fix, cap_initial));
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
            if let Some(expanded) = self.lexicon.lookup_shorthand(&core_lower) {
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

        // Step 4b: Valid-Word Fast Path (`0%` false-positive shield on real US/UK/tech/offensive words + clitic elisions)
        if self
            .lexicon
            .contains_valid_or_elided(&core_lower, config.dialect)
        {
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

            let word_meta = self.lexicon.lookup_word(&core_lower);
            let is_ambiguous_proper = word_meta.is_some_and(|m| m.is_ambiguous_proper());

            // Proper Noun auto-capitalization (english -> English, monday -> Monday)
            // Ambiguous lowercase words (`FLAG_AMBIGUOUS_PROPER`, e.g. `latin` or `march`) are skipped.
            if config.fix_proper_nouns && !shift_slip_fixed && !is_ambiguous_proper {
                if let Some(proper) = self.lexicon.lookup_proper_noun(&core_lower) {
                    if core != proper
                        && (rules.is_pinned(&core_lower)
                            || !rules.is_pair_blocked(&core_lower, &proper.to_lowercase()))
                    {
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
            let allow_title_case_learn = user_title_case
                && !shift_slip_fixed
                && !in_personal
                && (is_ambiguous_proper || self.lexicon.lookup_proper_noun(&core_lower).is_none())
                && word_meta.is_some_and(|m| m.is_tech());
            return (
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    do_not_learn: !allow_title_case_learn,
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
        struct PooledEntry {
            cand: Candidate,
            raw_edit_cost: f32,
            bg_term: f32,
            rescue_bonus: f32,
        }
        let mut pool: HashMap<String, PooledEntry> = HashMap::new();

        let push_candidate = |pool: &mut HashMap<String, PooledEntry>,
                              cand_lower: &str,
                              freq: u8,
                              bonus: f32,
                              kind: MatchKind,
                              custom_cost: Option<f32>| {
            if cand_lower == core_lower || rules.is_pair_blocked(&core_lower, cand_lower) {
                return;
            }
            // Keep true raw motor distance for SmashRescue even when `edit_cost` is scramble-clamped.
            let (edit_cost, raw_edit_cost) = match (kind, custom_cost) {
                (MatchKind::SmashRescue, Some(damped)) => {
                    (damped, weighted_keyboard_cost(&core_lower, cand_lower))
                }
                (_, Some(c)) => (c, c),
                (_, None) => {
                    let c = weighted_keyboard_cost(&core_lower, cand_lower);
                    (c, c)
                }
            };
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
            let rescue_bonus = if kind == MatchKind::SmashRescue {
                bonus
            } else {
                0.0
            };
            let score = freq_term + bg_term + bonus + anagram_bonus + trouble
                - (length_damping * edit_cost);

            let formatted = if let Some(canon) = rules.lookup_personal_canonical(cand_lower) {
                if canon.chars().any(char::is_uppercase) {
                    wrap(canon)
                } else {
                    wrap(&project_case(cand_lower, cap_initial))
                }
            } else {
                wrap(&project_case(cand_lower, cap_initial))
            };
            let make_entry = || PooledEntry {
                cand: Candidate {
                    replacement: formatted,
                    score,
                    edit_cost,
                    freq,
                    bigram_boost: bg,
                    kind,
                },
                raw_edit_cost,
                bg_term,
                rescue_bonus,
            };
            match pool.get_mut(cand_lower) {
                Some(existing) => {
                    if score > existing.cand.score {
                        *existing = make_entry();
                    }
                }
                None => {
                    pool.insert(cand_lower.to_string(), make_entry());
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
                let Some(meta) = self.lexicon.lookup_word(mapped) else {
                    continue;
                };
                if meta.is_offensive() || !self.lexicon.is_valid_meta(meta, config.dialect) {
                    continue;
                }
                let freq = self.lexicon.effective_freq_meta(meta, config.dialect);
                if freq == 0 {
                    continue;
                }
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
        // does not need expensive distance-2 DFA scanning over 160k lexicon words, provided the
        // hit itself is motor-close so a clean uninflected stem cannot be skipped.
        let skip_heavy_dfa = single_curated_hit
            && !collect_all
            && pool
                .values()
                .any(|e| e.cand.score >= 0.35 && e.raw_edit_cost <= 0.95);

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
                    if !meta.is_offensive() && self.lexicon.is_valid_meta(meta, config.dialect) {
                        push_candidate(
                            &mut pool,
                            mapped,
                            self.lexicon.effective_freq_meta(meta, config.dialect),
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
            let run_stage = |dist: u8, pool: &mut HashMap<String, PooledEntry>| {
                let mut fuzzy_hits = Vec::new();
                self.lexicon
                    .search_fuzzy(&core_lower, dist, &mut fuzzy_hits);
                for (w, meta) in fuzzy_hits {
                    let mapped = self.lexicon.map_dialect(&w, config.dialect);
                    let freq = if mapped == w {
                        self.lexicon.effective_freq_meta(meta, config.dialect)
                    } else {
                        self.lexicon
                            .lookup_word(mapped)
                            .map(|m| self.lexicon.effective_freq_meta(m, config.dialect))
                            .unwrap_or_else(|| {
                                self.lexicon.effective_freq_meta(meta, config.dialect)
                            })
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
                .map(|e| e.cand.score)
                .fold(f32::NEG_INFINITY, f32::max);
            let allow_d2 = char_len >= 5 || prev_lower.is_some();
            if allow_d2 && (best_d1 < 0.25 || collect_all) {
                run_stage(2, &mut pool);
            }
        }

        // 5e. `POPCNT` Character-Multiset Smash Rescue for heavy scrambles (`hstiautoin -> situation`)
        let best_so_far = pool
            .values()
            .map(|e| e.cand.score)
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
                // Restrict short (7–9 char) dl <= 2 candidates from SmashRescue so high-frequency
                // words (freq >= 105, e.g. inflected forms) do not gain a +0.26 cliff over stems.
                if dl > max_dl || (dl < 3 && char_len < 10) {
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

        // 5f. Personal dictionary words (`personal_dict` and `[lang.<id>].personal_dict` in `rules.toml`)
        if char_len >= 3 {
            let personal_iter = rules
                .personal_words()
                .into_iter()
                .flatten()
                .chain(rules.lang_personal_words().into_iter().flatten());
            for pword in personal_iter {
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
                    rules.personal_word_freq(pword),
                    0.14,
                    MatchKind::PersonalDict,
                    Some(cost),
                );
            }
        }

        let (t_auto, min_margin) = Self::thresholds(config.aggressiveness);

        // Damp bigram overrides when a candidate has large excess motor cost vs the tightest viable match.
        let min_raw_edit = pool
            .values()
            .filter(|e| e.cand.score >= t_auto)
            .map(|e| e.raw_edit_cost)
            .fold(f32::INFINITY, f32::min);
        if min_raw_edit.is_finite() {
            for entry in pool.values_mut() {
                if entry.bg_term > 0.0 {
                    let excess = entry.raw_edit_cost - min_raw_edit;
                    if excess > 0.45 {
                        let keep = (1.0 - (excess - 0.45) * 1.5).clamp(0.0, 1.0);
                        let new_bg = entry.bg_term * keep;
                        entry.cand.score -= entry.bg_term - new_bg;
                        entry.bg_term = new_bg;
                    }
                }
            }
        }

        // English Relational Stem-Dominance Guard: do not let a high-edit inflection (-ed/-d/-s/-es/-ies/-ied/-ing)
        // outrank or tie-block its own motor-clean viable base stem via bigram/SmashRescue boosts.
        if self.lexicon.header().lang == "en" {
            let viable_stems: Vec<(String, f32, f32)> = pool
                .iter()
                .filter(|(k, e)| k.len() >= 3 && e.cand.score >= t_auto)
                .map(|(k, e)| (k.clone(), e.raw_edit_cost, e.cand.score))
                .collect();
            if !viable_stems.is_empty() {
                for (cand_key, entry) in pool.iter_mut() {
                    let mut cap: Option<f32> = None;
                    for (stem_key, stem_raw_edit, stem_score) in &viable_stems {
                        if is_english_inflection_of(stem_key, cand_key)
                            && entry.raw_edit_cost >= *stem_raw_edit + 0.45
                        {
                            let c = *stem_score - min_margin - 0.01;
                            cap = Some(cap.map_or(c, |prev| prev.min(c)));
                        }
                    }
                    if let Some(max_score) = cap {
                        entry.cand.score -= entry.bg_term + entry.rescue_bonus;
                        entry.bg_term = 0.0;
                        entry.rescue_bonus = 0.0;
                        if entry.cand.score > max_score {
                            entry.cand.score = max_score;
                        }
                    }
                }
            }
        }

        let mut ranked: Vec<Candidate> = pool.into_values().map(|e| e.cand).collect();
        ranked.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| b.freq.cmp(&a.freq))
        });

        let mut ambiguous_high_score_tie = false;
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
            if top.score >= t_auto && !passes_margin {
                ambiguous_high_score_tie = true;
            }
        }

        ranked.truncate(5);
        (
            EvalOutcome::Unchanged {
                reason: MatchKind::Bypass,
                do_not_learn: ambiguous_high_score_tie,
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
        let lang_rules = self.lexicon.rules();
        let is_func = |w: &str| lang_rules.function_words.iter().any(|fw| fw == w);
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
            if !is_func(w1) && !is_func(w2) {
                continue;
            }
            let (Some(m1), Some(m2)) = (self.lexicon.lookup_word(w1), self.lexicon.lookup_word(w2))
            else {
                continue;
            };
            if !self.lexicon.is_valid_meta(m1, config.dialect)
                || !self.lexicon.is_valid_meta(m2, config.dialect)
            {
                continue;
            }
            let bg = self.lexicon.bigram_score_ids(m1.word_id, m2.word_id);
            if bg < 0.72 {
                continue;
            }
            let min_f = self
                .lexicon
                .effective_freq_meta(m1, config.dialect)
                .min(self.lexicon.effective_freq_meta(m2, config.dialect));
            let w1_fmt =
                if w1 == "i" && config.auto_capitalize_i && lang_rules.capitalize_standalone_i {
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
            if !is_func(w1) || !is_func(w2) {
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
            let min_f = self
                .lexicon
                .effective_freq_meta(m1, config.dialect)
                .min(self.lexicon.effective_freq_meta(m2, config.dialect));
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

impl LanguageScorer for LatinScorer {
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

/// Returns `true` when `cand` is a regular English inflection (`-s`, `-es`, `-ies`, `-d`, `-ed`,
/// `-ied`, `-ing`, or doubled final consonant + `-ed`/`-ing`) of ASCII base `stem` (`len >= 3`).
fn is_english_inflection_of(stem: &str, cand: &str) -> bool {
    if stem.len() < 3 || cand.len() <= stem.len() || !stem.is_ascii() || !cand.is_ascii() {
        return false;
    }
    if let Some(suf) = cand.strip_prefix(stem) {
        if matches!(suf, "s" | "es" | "ed" | "ing") || (suf == "d" && stem.ends_with('e')) {
            return true;
        }
        let last = stem.as_bytes()[stem.len() - 1];
        if last.is_ascii_alphabetic()
            && !matches!(last, b'a' | b'e' | b'i' | b'o' | b'u' | b'w' | b'x' | b'y')
        {
            let sb = suf.as_bytes();
            if sb.first() == Some(&last) && (&sb[1..] == b"ed" || &sb[1..] == b"ing") {
                return true;
            }
        }
    }
    if let Some(root) = stem.strip_suffix('y') {
        if let Some(suf) = cand.strip_prefix(root) {
            if matches!(suf, "ies" | "ied") {
                return true;
            }
        }
    }
    if stem.ends_with('e') && !stem.ends_with("ee") {
        let root = &stem[..stem.len() - 1];
        if cand.strip_prefix(root) == Some("ing") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use fst::MapBuilder;

    use super::*;
    use crate::lexicon::{
        serialize_skdict_v3, CliticRules, LanguageRules, PackHeader, PackTier, SkdictV3Payload,
        WordMeta, FLAG_AMBIGUOUS_PROPER,
    };

    fn synthetic_lexicon(
        lang: &str,
        words: &[(&str, WordMeta)],
        proper_nouns: &[(&str, &str)],
        shorthand: &[(&str, &str)],
        rules: LanguageRules,
    ) -> PackLexicon {
        let mut sorted_words = words.to_vec();
        sorted_words.sort_by(|a, b| a.0.cmp(b.0));
        let mut wb = MapBuilder::memory();
        for (w, meta) in sorted_words {
            wb.insert(w.as_bytes(), meta.pack()).unwrap();
        }
        let mut sorted_proper: Vec<(String, String)> = proper_nouns
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        sorted_proper.sort_by(|a, b| a.0.cmp(&b.0));

        let mut sorted_short: Vec<(String, String)> = shorthand
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        sorted_short.sort_by(|a, b| a.0.cmp(&b.0));

        let header = PackHeader {
            lang: lang.to_string(),
            pack_version: [0, 3, 0],
            tier: PackTier::Data,
            variants: vec![format!("{lang}_{}", lang.to_uppercase())],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };

        let bytes = serialize_skdict_v3(&SkdictV3Payload {
            header: &header,
            words_fst: &wb.into_inner().unwrap(),
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &sorted_proper,
            shorthand: &sorted_short,
            slang_set: &[],
            rules: Some(&rules),
            syllable_dict: &[],
            source_manifest: &[],
        });
        PackLexicon::from_owned_bytes(bytes).expect("valid synthetic v3 pack")
    }

    #[test]
    fn custom_shorthand_section_works_without_binary_consts() {
        let lex = synthetic_lexicon(
            "es",
            &[(
                "porque",
                WordMeta {
                    freq0: 210,
                    freq1: 0,
                    variant_mask: 0x01,
                    flags: 0,
                    word_id: 1,
                },
            )],
            &[],
            &[("xq", "porque")],
            LanguageRules::default(),
        );
        let scorer = LatinScorer::new(lex);
        let cfg = ScorerConfig::default();
        let ctx = EvalContext::default();

        let out = scorer.evaluate("xq", &ctx, &cfg, &EmptyUserRules);
        assert_eq!(
            out,
            EvalOutcome::Autocorrect {
                replacement: "porque".to_string(),
                kind: MatchKind::ShorthandExpansion,
                score: 4.4,
                margin: 4.4,
            }
        );

        // English binary shorthand ("pls") does NOT trigger on a synthetic Spanish pack.
        let out_pls = scorer.evaluate("pls", &ctx, &cfg, &EmptyUserRules);
        assert!(matches!(out_pls, EvalOutcome::Unchanged { .. }));
    }

    #[test]
    fn ambiguous_proper_flag_suppresses_step_4b_capitalization() {
        let lex = synthetic_lexicon(
            "en",
            &[
                (
                    "latin",
                    WordMeta {
                        freq0: 190,
                        freq1: 190,
                        variant_mask: 0x01,
                        flags: FLAG_AMBIGUOUS_PROPER,
                        word_id: 1,
                    },
                ),
                (
                    "monday",
                    WordMeta {
                        freq0: 210,
                        freq1: 210,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 2,
                    },
                ),
            ],
            &[("latin", "Latin"), ("monday", "Monday")],
            &[],
            LanguageRules::default(),
        );
        let scorer = LatinScorer::new(lex);
        let cfg = ScorerConfig::default();
        let ctx = EvalContext::default();

        assert_eq!(
            scorer.evaluate("latin", &ctx, &cfg, &EmptyUserRules),
            EvalOutcome::Unchanged {
                reason: MatchKind::ValidWord,
                do_not_learn: true,
            }
        );
        assert_eq!(
            scorer.evaluate("monday", &ctx, &cfg, &EmptyUserRules),
            EvalOutcome::Autocorrect {
                replacement: "Monday".to_string(),
                kind: MatchKind::ProperNoun,
                score: 4.5,
                margin: 4.5,
            }
        );
    }

    #[test]
    fn clitic_prefixes_elision_shields_valid_stems_and_rejects_one_char_stems() {
        let lex = synthetic_lexicon(
            "fr",
            &[
                (
                    "a",
                    WordMeta {
                        freq0: 240,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 1,
                    },
                ),
                (
                    "accord",
                    WordMeta {
                        freq0: 210,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 2,
                    },
                ),
                (
                    "homme",
                    WordMeta {
                        freq0: 220,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 3,
                    },
                ),
                (
                    "il",
                    WordMeta {
                        freq0: 245,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 4,
                    },
                ),
            ],
            &[],
            &[],
            LanguageRules {
                clitics: CliticRules {
                    suffixes: Vec::new(),
                    prefixes: vec!["l".to_string(), "d".to_string(), "qu".to_string()],
                },
                ..LanguageRules::default()
            },
        );
        let scorer = LatinScorer::new(lex);
        let cfg = ScorerConfig::default();
        let ctx = EvalContext::default();

        for valid_elided in ["l'homme", "qu'il", "d’accord", "L'homme"] {
            assert_eq!(
                scorer.evaluate(valid_elided, &ctx, &cfg, &EmptyUserRules),
                EvalOutcome::Unchanged {
                    reason: MatchKind::ValidWord,
                    do_not_learn: true,
                },
                "expected ValidWord shield for {valid_elided}"
            );
        }

        // 1-char stem "a" must NOT pass the elision ValidWord shield (min 2 chars).
        assert_ne!(
            scorer.evaluate("l'a", &ctx, &cfg, &EmptyUserRules),
            EvalOutcome::Unchanged {
                reason: MatchKind::ValidWord,
                do_not_learn: true,
            }
        );
    }

    #[test]
    fn capitalize_standalone_i_false_leaves_lowercase_i_untouched() {
        let lex = synthetic_lexicon(
            "es",
            &[(
                "y",
                WordMeta {
                    freq0: 250,
                    freq1: 0,
                    variant_mask: 0x01,
                    flags: 0,
                    word_id: 1,
                },
            )],
            &[],
            &[],
            LanguageRules {
                capitalize_standalone_i: false,
                ..LanguageRules::default()
            },
        );
        let scorer = LatinScorer::new(lex);
        let cfg = ScorerConfig {
            auto_capitalize_i: true,
            ..ScorerConfig::default()
        };
        let ctx = EvalContext::default();

        assert!(matches!(
            scorer.evaluate("i", &ctx, &cfg, &EmptyUserRules),
            EvalOutcome::Unchanged { .. }
        ));
    }

    #[test]
    fn english_inflection_helper_recognizes_regular_forms_and_rejects_unrelated_words() {
        for (stem, cand) in [
            ("disable", "disabled"),
            ("disable", "disables"),
            ("disable", "disabling"),
            ("observe", "observed"),
            ("compile", "compiled"),
            ("compile", "compiling"),
            ("require", "required"),
            ("require", "requires"),
            ("plan", "planned"),
            ("plan", "planning"),
            ("stop", "stopped"),
            ("apply", "applied"),
            ("apply", "applies"),
            ("process", "processes"),
            ("process", "processed"),
            ("process", "processing"),
            ("implementation", "implementations"),
        ] {
            assert!(
                is_english_inflection_of(stem, cand),
                "expected {cand:?} to be recognized as inflection of {stem:?}"
            );
        }

        for (stem, cand) in [
            ("go", "goes"),
            ("plan", "planner"),
            ("plane", "planned"),
            ("tree", "trings"),
            ("speed", "spede"),
            ("disable", "disable"),
        ] {
            assert!(
                !is_english_inflection_of(stem, cand),
                "did not expect {cand:?} to be recognized as inflection of {stem:?}"
            );
        }
    }
}
