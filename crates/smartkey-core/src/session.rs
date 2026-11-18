use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::casing::is_abbreviation_with_rules;
use crate::learner::{
    record_manual_edit, record_telex_passthrough_repair, record_undo_keep_verbatim,
    record_untouched_unknown, resolve_restored_edit, LearnEvent,
};
use crate::lexicon::{LexiconError, PackLexicon, PackTier};
use crate::scorer::{EvalContext, LanguageScorer, LatinScorer, UserRuleView};
use crate::store::{RuleStore, UndoResolution};
use crate::types::{
    AppProfile, Candidate, EvalOutcome, Explanation, InputOrigin, LangId, MatchKind, UndoMode,
};

enum LoadedPack {
    Data(LatinScorer),
    Bridge(PackLexicon),
}

impl LoadedPack {
    fn from_lexicon(lex: PackLexicon) -> Self {
        match lex.header().tier {
            PackTier::Data => Self::Data(LatinScorer::new(lex)),
            PackTier::ImeBridge => Self::Bridge(lex),
        }
    }

    fn lexicon(&self) -> &PackLexicon {
        match self {
            Self::Data(s) => s.lexicon(),
            Self::Bridge(l) => l,
        }
    }

    fn scorer(&self) -> Option<&LatinScorer> {
        match self {
            Self::Data(s) => Some(s),
            Self::Bridge(_) => None,
        }
    }
}

/// Process-wide registry of loaded `.skdict` v3 packs keyed by language code (`"en"`, `"vi"`, `"es"`, ...).
pub struct LexiconRegistry {
    packs: BTreeMap<String, LoadedPack>,
    disk_langs: BTreeSet<String>,
}

impl Default for LexiconRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LexiconRegistry {
    pub fn new() -> Self {
        Self {
            packs: BTreeMap::new(),
            disk_langs: BTreeSet::new(),
        }
    }

    /// Priority-ordered dictionary search directories:
    /// `SMARTKEY_DICT_DIR` > user data dir > `/usr/local/share/smartkey/dicts` > `/usr/share/smartkey/dicts`.
    pub fn default_search_dirs() -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        if let Ok(env_dir) = std::env::var("SMARTKEY_DICT_DIR") {
            let trimmed = env_dir.trim();
            if !trimmed.is_empty() {
                dirs.push(PathBuf::from(trimmed));
            }
        }
        let user_dir = crate::lexicon::default_user_dict_dir();
        if !dirs.contains(&user_dir) {
            dirs.push(user_dir);
        }
        for sys in [
            "/usr/local/share/smartkey/dicts",
            "/usr/share/smartkey/dicts",
        ] {
            let p = PathBuf::from(sys);
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
        dirs
    }

    /// Scans `dirs` in priority order (earliest directory wins on conflict for the same `lang`),
    /// collecting `"legacy_pack_ignored"` / `"invalid_pack_ignored"` notices for rejected `.skdict` files.
    /// Falls back to `bundled-en` when `"en"` was not loaded from disk and `bundled_en_fallback` is true.
    pub fn scan_dirs(dirs: &[PathBuf], bundled_fallback: bool) -> (Self, Vec<(String, String)>) {
        let mut reg = Self::new();
        let mut notices = Vec::new();

        for dir in dirs {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "skdict") && p.is_file())
                .collect();
            paths.sort();

            for path in paths {
                let bytes = match std::fs::read(&path) {
                    Ok(b) => b,
                    Err(e) => {
                        notices.push((
                            "invalid_pack_ignored".to_string(),
                            format!("{} ({e})", path.display()),
                        ));
                        continue;
                    }
                };
                match PackLexicon::from_owned_bytes(bytes) {
                    Ok(lex) => {
                        let lang = lex.header().lang.clone();
                        if !reg.packs.contains_key(&lang) {
                            reg.disk_langs.insert(lang.clone());
                            reg.packs.insert(lang, LoadedPack::from_lexicon(lex));
                        }
                    }
                    Err(LexiconError::LegacyFormat(ver)) => {
                        notices.push((
                            "legacy_pack_ignored".to_string(),
                            format!("{} (skdict v{ver})", path.display()),
                        ));
                    }
                    Err(e) => {
                        notices.push((
                            "invalid_pack_ignored".to_string(),
                            format!("{} ({e})", path.display()),
                        ));
                    }
                }
            }
        }

        if bundled_fallback && !reg.packs.contains_key("en") {
            #[cfg(feature = "bundled-en")]
            if let Ok(scorer) = LatinScorer::new_bundled() {
                reg.packs.insert("en".to_string(), LoadedPack::Data(scorer));
            }
        }
        if bundled_fallback && !reg.packs.contains_key("vi") {
            #[cfg(feature = "bundled-vi")]
            if let Ok(lex) = PackLexicon::from_bundled_vi() {
                reg.packs.insert("vi".to_string(), LoadedPack::Bridge(lex));
            }
        }

        let _ = bundled_fallback;
        (reg, notices)
    }

    pub fn load_default() -> (Self, Vec<(String, String)>) {
        Self::scan_dirs(
            &Self::default_search_dirs(),
            cfg!(any(feature = "bundled-en", feature = "bundled-vi")),
        )
    }

    pub fn insert(&mut self, lang: impl Into<String>, scorer: LatinScorer) {
        self.packs.insert(lang.into(), LoadedPack::Data(scorer));
    }

    pub fn insert_pack(&mut self, lang: impl Into<String>, lexicon: PackLexicon) {
        self.packs
            .insert(lang.into(), LoadedPack::from_lexicon(lexicon));
    }

    pub fn scorer(&self, lang: &str) -> Option<&LatinScorer> {
        self.packs.get(lang).and_then(LoadedPack::scorer)
    }

    pub fn lexicon(&self, lang: &str) -> Option<&PackLexicon> {
        self.packs.get(lang).map(LoadedPack::lexicon)
    }

    pub fn has_disk_pack(&self) -> bool {
        !self.disk_langs.is_empty()
    }

    pub fn languages(&self) -> impl Iterator<Item = &str> {
        self.packs.keys().map(String::as_str)
    }
}

/// Shared process-wide engine owning the zero-copy Layer 1 registry and comment-preserving `RuleStore`.
pub struct SmartKeyEngine {
    registry: LexiconRegistry,
    vi_runtime_syllables: Option<Vec<Box<str>>>,
    emitted_notices: BTreeSet<(String, String)>,
    store: RuleStore,
}

impl SmartKeyEngine {
    pub fn new(scorer: Option<LatinScorer>, store: RuleStore) -> Self {
        let mut registry = LexiconRegistry::new();
        if let Some(s) = scorer {
            let lang = s.lexicon().header().lang.clone();
            registry.insert(lang, s);
        }
        Self::from_registry(registry, store)
    }

    pub fn from_registry(registry: LexiconRegistry, store: RuleStore) -> Self {
        let mut engine = Self {
            registry,
            vi_runtime_syllables: None,
            emitted_notices: BTreeSet::new(),
            store,
        };
        engine.refresh_vi_authority();
        engine
    }

    pub fn new_from_dirs(dirs: &[PathBuf], bundled_fallback: bool, store: RuleStore) -> Self {
        let (registry, notices) = LexiconRegistry::scan_dirs(dirs, bundled_fallback);
        let mut engine = Self::from_registry(registry, store);
        engine.emit_notices(notices);
        engine
    }

    #[cfg(feature = "bundled-en")]
    pub fn new_bundled(store: RuleStore) -> Result<Self, crate::lexicon::LexiconError> {
        let mut registry = LexiconRegistry::new();
        let scorer = LatinScorer::new_bundled()?;
        registry.insert("en", scorer);
        #[cfg(feature = "bundled-vi")]
        if let Ok(vi_lex) = PackLexicon::from_bundled_vi() {
            registry.insert_pack("vi", vi_lex);
        }
        Ok(Self::from_registry(registry, store))
    }

    #[cfg(feature = "bundled-en")]
    pub fn in_memory_default() -> Result<Self, crate::lexicon::LexiconError> {
        Self::new_bundled(RuleStore::in_memory(crate::store::Preset::Balanced))
    }

    pub fn new_auto(store: RuleStore) -> Self {
        let (registry, notices) = LexiconRegistry::load_default();
        let mut engine = Self::from_registry(registry, store);
        engine.emit_notices(notices);
        engine
    }

    fn refresh_vi_authority(&mut self) {
        self.vi_runtime_syllables = None;
        let Some(path) = crate::vi_telex::resolve_lotus_dict_path() else {
            return;
        };
        let Some(lotus_syls) = crate::vi_telex::load_lotus_dict_from_path(&path) else {
            return;
        };
        if let Some(vi_pack) = self.registry.lexicon("vi") {
            let pack_syls = vi_pack.syllable_dict();
            if !pack_syls.is_empty() {
                let (added, removed) = crate::vi_telex::diff_syllable_sets(&lotus_syls, pack_syls);
                if added > 0 || removed > 0 {
                    self.emit_notices(vec![(
                        "lotus_dict_drift".to_string(),
                        format!(
                            "{} (+{added} -{removed} syllables vs vi pack)",
                            path.display()
                        ),
                    )]);
                }
            }
        }
        self.vi_runtime_syllables = Some(lotus_syls);
    }

    fn active_vi_syllables(&self) -> &[Box<str>] {
        if let Some(syls) = &self.vi_runtime_syllables {
            return syls;
        }
        if let Some(vi_lex) = self.registry.lexicon("vi") {
            let syls = vi_lex.syllable_dict();
            if !syls.is_empty() {
                return syls;
            }
        }
        crate::lexicon::bundled_vi_lexicon()
            .map(PackLexicon::syllable_dict)
            .unwrap_or(&[])
    }

    fn vi_dict_and_store_mut(&mut self) -> (&[Box<str>], &mut RuleStore) {
        let syls: &[Box<str>] = if let Some(syls) = &self.vi_runtime_syllables {
            syls
        } else if let Some(vi_lex) = self.registry.lexicon("vi") {
            let syls = vi_lex.syllable_dict();
            if !syls.is_empty() {
                syls
            } else {
                crate::lexicon::bundled_vi_lexicon()
                    .map(PackLexicon::syllable_dict)
                    .unwrap_or(&[])
            }
        } else {
            crate::lexicon::bundled_vi_lexicon()
                .map(PackLexicon::syllable_dict)
                .unwrap_or(&[])
        };
        (syls, &mut self.store)
    }

    fn active_vi_pack(&self) -> Option<&PackLexicon> {
        match self.registry.lexicon("vi") {
            Some(lex) => Some(lex),
            None => crate::lexicon::bundled_vi_lexicon(),
        }
    }

    pub fn is_vi_dict_word(&self, lower: &str) -> bool {
        crate::vi_telex::is_dict_word_in(self.active_vi_syllables(), lower)
    }

    pub fn is_vi_slang_word(&self, lower: &str) -> bool {
        !lower.is_empty()
            && self
                .active_vi_pack()
                .is_some_and(|l| l.is_slang_word(lower) || l.lookup_shorthand(lower).is_some())
    }

    pub fn lookup_vi_shorthand(&self, lower: &str) -> Option<&str> {
        if lower.is_empty() {
            return None;
        }
        self.active_vi_pack()?.lookup_shorthand(lower)
    }

    fn simulate_vi_lotus(
        &self,
        raw_keys: &str,
        opts: crate::vi_telex::LotusOptions,
    ) -> crate::vi_telex::LotusSimulation {
        crate::vi_telex::simulate_lotus_word_with_dict(raw_keys, opts, self.active_vi_syllables())
    }

    fn rescue_vi_telex(
        &self,
        raw_keys: &str,
        opts: crate::vi_telex::LotusOptions,
    ) -> Option<crate::vi_telex::TelexRescue> {
        crate::vi_telex::rescue_telex_typo_with_dict(raw_keys, opts, self.active_vi_syllables())
    }

    fn emit_notices(&mut self, notices: Vec<(String, String)>) {
        for (notice, detail) in notices {
            if self
                .emitted_notices
                .insert((notice.clone(), detail.clone()))
            {
                self.store.log_pack_notice(&notice, &detail);
            }
        }
    }

    pub fn reload_dicts(&mut self) -> bool {
        self.store.reload_if_modified();
        let (registry, notices) = LexiconRegistry::load_default();
        self.emit_notices(notices);
        let loaded_any = registry.has_disk_pack();
        self.registry = registry;
        self.refresh_vi_authority();
        loaded_any
    }

    pub fn registry(&self) -> &LexiconRegistry {
        &self.registry
    }

    pub fn scorer(&self) -> Option<&LatinScorer> {
        self.registry.scorer("en")
    }

    pub fn lexicon(&self) -> Option<&PackLexicon> {
        self.registry.lexicon("en")
    }

    pub fn scorer_for(&self, lang: &str) -> Option<&LatinScorer> {
        self.registry.scorer(lang)
    }

    pub fn lexicon_for(&self, lang: &str) -> Option<&PackLexicon> {
        self.registry.lexicon(lang)
    }

    pub fn store(&self) -> &RuleStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut RuleStore {
        &mut self.store
    }

    pub fn is_telex_passthrough(&self, raw_ascii: &str) -> bool {
        self.store
            .compiled()
            .telex_passthrough
            .contains(&raw_ascii.to_lowercase())
    }

    /// Returns true if a Lotus-composed Vietnamese syllable (`"ò"` from `"of"`, `"í"` from `"is"`,
    /// `"ảu"` from `"aur"`) should be restored to raw ASCII `token` via explicit `telex_passthrough`
    /// or the bilingual English-Sentence Momentum Shield.
    pub fn should_passthrough_telex(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        composed_lower: &str,
    ) -> bool {
        let lower = token.to_lowercase();
        if self.store.is_pair_blocked(&lower, &lower)
            || self.store.is_pair_blocked(composed_lower, &lower)
        {
            return false;
        }
        if self.is_telex_passthrough(token) {
            return true;
        }

        // Safe sentence-start TitleCase passthrough for English words whose Telex-composed
        // lowercase forms (`ò`, `ì`, `ỏ`) are not standalone Vietnamese sentence starters.
        // (`As` -> `Á`, `Is` -> `Í`, `Was` -> `Ứa`, `Has` -> `Há`, `His` -> `Hí` stay
        // Vietnamese at sentence start because those are real Vietnamese sentence starters.)
        if ctx.prev_word.is_none()
            && ctx.at_sentence_start
            && matches!(token, "If" | "Of" | "Or")
            && !self.store.is_lang_only_personal_word("vi", &lower)
        {
            return true;
        }

        // English-Sentence Momentum Shield: when the immediately preceding word was an unambiguous
        // English word (not in `vietnamese.cm.dict` or `slang_vi`), keep common short English words
        // (`of -> ò`, `is -> í`, `as -> á`, `or -> ỏ`, `if -> ì`, `was -> ứa`, `has -> há`, `for -> fỏ`)
        // in English instead of letting Telex hijack mid-sentence English prose.
        let Some(prev_raw) = ctx.prev_word else {
            return false;
        };
        let prev_lower = prev_raw.trim().to_lowercase();
        if prev_lower.is_empty()
            || !prev_lower.is_ascii()
            || self.is_vi_dict_word(&prev_lower)
            || self.is_vi_slang_word(&prev_lower)
        {
            return false;
        }
        if self.store.is_lang_only_personal_word("vi", &lower) {
            return false;
        }
        let dialect = self.store.scorer_config().dialect;
        let Some(lex) = self.lexicon() else {
            return false;
        };
        let prev_is_english = self.store.compiled().personal_dict.contains(&prev_lower)
            || lex
                .lookup_word(&prev_lower)
                .is_some_and(|m| m.is_valid_for_dialect(dialect));
        if !prev_is_english {
            return false;
        }

        if !token.is_ascii() || !(2..=6).contains(&lower.len()) {
            return false;
        }
        let Some(cur_meta) = lex.lookup_word(&lower) else {
            return false;
        };
        if !cur_meta.is_valid_for_dialect(dialect) || cur_meta.effective_freq(dialect) < 155 {
            return false;
        }

        let is_core_telex_collision = matches!(
            lower.as_str(),
            "of" | "is" | "as" | "or" | "if" | "was" | "has" | "for" | "her" | "his" | "our"
        );
        is_core_telex_collision || lex.bigram_score(&prev_lower, &lower) >= 0.05
    }

    pub fn evaluate_token(&self, token: &str, ctx: &EvalContext<'_>) -> EvalOutcome {
        self.evaluate_token_internal(token, ctx, true)
    }

    /// Evaluates `token` knowing whether the host observed `fcitx5-lotus` actually mutating/composing
    /// keys during this word (`lotus_active_on_word == false` when Lotus is in `Off`/`Default` mode
    /// and forwarded raw ASCII keys without composing).
    fn evaluate_token_internal(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        lotus_active_on_word: bool,
    ) -> EvalOutcome {
        if !self.store.compiled().enabled
            || ctx.profile == AppProfile::Bypass
            || ctx.in_markdown_code
        {
            return EvalOutcome::Unchanged {
                reason: MatchKind::Bypass,
                do_not_learn: true,
            };
        }

        if self.store.active_lang().as_str() == "vi" && self.store.compiled().vi_lotus_enabled {
            return self.evaluate_vi_lotus_token(token, ctx, lotus_active_on_word);
        }

        if let Some(scorer) = self.scorer_for(self.store.active_lang().as_str()) {
            return scorer.evaluate(token, ctx, self.store.scorer_config(), &self.store);
        }

        self.evaluate_user_rules_only(token, ctx)
    }

    fn evaluate_user_rules_only(&self, token: &str, ctx: &EvalContext<'_>) -> EvalOutcome {
        if let Some(snip) = self.store.lookup_snippet(token) {
            return EvalOutcome::Autocorrect {
                replacement: snip.to_string(),
                kind: MatchKind::UserSnippet,
                score: 10.0,
                margin: 10.0,
            };
        }
        let lower = token.to_lowercase();
        if let Some(prev) = ctx.prev_word {
            let prev_lower = prev.to_lowercase();
            if let Some(target) = self.store.lookup_context_rule(&prev_lower, &lower) {
                if !self.store.is_pair_blocked(&lower, &target.to_lowercase()) {
                    return EvalOutcome::Autocorrect {
                        replacement: target.to_string(),
                        kind: MatchKind::UserContextRule,
                        score: 9.0,
                        margin: 9.0,
                    };
                }
            }
        }
        if let Some(target) = self.store.lookup_correction(&lower) {
            if !self.store.is_pair_blocked(&lower, &target.to_lowercase()) {
                return EvalOutcome::Autocorrect {
                    replacement: target.to_string(),
                    kind: MatchKind::UserCorrectionRule,
                    score: 8.5,
                    margin: 8.5,
                };
            }
        }
        let known = self.store.is_personal_word(&lower);
        EvalOutcome::Unchanged {
            reason: if known {
                MatchKind::ValidWord
            } else {
                MatchKind::Bypass
            },
            do_not_learn: known,
        }
    }

    fn evaluate_vi_lotus_token(
        &self,
        token: &str,
        ctx: &EvalContext<'_>,
        lotus_composing: bool,
    ) -> EvalOutcome {
        // 1. User explicit rules (`[snippets]`, `[context_corrections]`, `[corrections]`, `[lang.vi.corrections]`)
        if let EvalOutcome::Autocorrect {
            replacement,
            kind,
            score,
            margin,
        } = self.evaluate_user_rules_only(token, ctx)
        {
            return EvalOutcome::Autocorrect {
                replacement,
                kind,
                score,
                margin,
            };
        }

        let lower = token.to_lowercase();
        let lotus_opts = self.store.lotus_options();
        let sim = self.simulate_vi_lotus(token, lotus_opts);

        // 1b. Built-in Vietnamese Shorthand Auto-Expansion (`ko -> không`, `dc -> được`, `ntn -> như thế nào`)
        //     Enabled when `expand_shorthand = true` (default). `1x BackSpace` reverts the entire expanded
        //     phrase back to the raw shorthand trigger (`is_snippet = true` when expansion contains spaces).
        if self.store.scorer_config().expand_shorthand {
            let char_len = token.chars().count();
            let is_all_caps_acronym = char_len >= 2
                && token
                    .chars()
                    .all(|c| !c.is_alphabetic() || c.is_uppercase());
            // For 1-char Vietnamese shorthand (`j -> gì`, `k -> không`, `h -> giờ`, `r -> rồi`, `uh / u -> ừ`),
            // skip uppercase single letters mid-sentence (`J`, `S`, `H`) so middle initials are untouched.
            let is_single_upper_initial = char_len == 1
                && token.chars().next().is_some_and(char::is_uppercase)
                && !ctx.at_sentence_start;

            if !is_all_caps_acronym && !is_single_upper_initial {
                let committed_lower = sim.committed_at_break.to_lowercase();
                let exp = self.lookup_vi_shorthand(&lower).or_else(|| {
                    if lotus_composing && committed_lower != lower {
                        self.lookup_vi_shorthand(&committed_lower)
                    } else {
                        None
                    }
                });
                if let Some(expansion) = exp {
                    let exp_lower = expansion.to_lowercase();
                    if !self.store.is_pair_blocked(&lower, &exp_lower)
                        && !self.store.is_pair_blocked(&committed_lower, &exp_lower)
                    {
                        let cap_initial = ctx.at_sentence_start
                            && !ctx.suppress_sentence_cap
                            && self.store.scorer_config().auto_capitalize_sentence;
                        return EvalOutcome::Autocorrect {
                            replacement: crate::vi_telex::project_shorthand_case(
                                token,
                                expansion,
                                cap_initial,
                            ),
                            kind: MatchKind::ShorthandExpansion,
                            score: 9.4,
                            margin: 9.4,
                        };
                    }
                }
            }
        }

        // 2. If Lotus composed this into Vietnamese, or if it is a valid Vietnamese syllable
        //    (including toneless Vietnamese words like `anh`, `em`, `nha`, `con`, `cho`) or
        //    conversational Vietnamese slang (`ib`, `rep`, `onl`, `uk`, `uhm`, `okela`, `bt`, `nc`):
        if (lotus_composing && sim.is_valid_vietnamese)
            || self.is_vi_dict_word(&lower)
            || self.is_vi_slang_word(&lower)
        {
            let composed_lower = sim.committed_at_break.to_lowercase();
            if lotus_composing
                && sim.is_composed_vietnamese
                && self.should_passthrough_telex(token, ctx, &composed_lower)
            {
                return EvalOutcome::Autocorrect {
                    replacement: token.to_string(),
                    kind: MatchKind::UserCorrectionRule,
                    score: 9.5,
                    margin: 9.5,
                };
            }
            return EvalOutcome::Unchanged {
                reason: MatchKind::ValidWord,
                do_not_learn: (lotus_composing
                    && (sim.is_valid_vietnamese || sim.is_composed_vietnamese))
                    || self.is_vi_dict_word(&lower)
                    || self.is_vi_slang_word(&lower)
                    || self.store.is_personal_word(&lower),
            };
        }

        // 3. Valid English word shield (`personal_dict` + `EnglishLexicon` on both raw keys and
        //    Lotus's on-screen preedit after 3-key Telex escape like `seee -> see`, `tooo -> too`):
        let dialect = self.store.scorer_config().dialect;
        let screen_lower = sim.preedit_before_break.to_lowercase();
        let is_lexicon_english_raw = self
            .lexicon()
            .and_then(|lex| lex.lookup_word(&lower))
            .is_some_and(|meta| meta.is_valid_for_dialect(dialect));
        let is_lexicon_english_screen = lotus_composing
            && self
                .lexicon()
                .and_then(|lex| lex.lookup_word(&screen_lower))
                .is_some_and(|meta| meta.is_valid_for_dialect(dialect));
        let is_known_english_raw = self.store.is_personal_word(&lower) || is_lexicon_english_raw;
        let is_known_english_screen = lotus_composing
            && (self.store.is_personal_word(&screen_lower) || is_lexicon_english_screen);
        let is_known_english = is_known_english_raw || is_known_english_screen;

        // If the user typed a valid English word containing doubled Telex keys (`issue`, `off`,
        // `boss`, `less`, `process`, `success`) where Lotus's tone-undo ate one `s`/`f` leaving
        // a truncated non-Vietnamese string (`isue`, `of`, `bos`, `les`, `proces`) on screen,
        // automatically restore the full English word (only when Lotus actually composed!).
        if lotus_composing && is_known_english_raw && sim.preedit_before_break != token {
            return EvalOutcome::Autocorrect {
                replacement: token.to_string(),
                kind: MatchKind::ValidWord,
                score: 9.5,
                margin: 9.5,
            };
        }

        // 4. Vietnamese Telex Typo Rescue (prioritized over fuzzy English matching;
        //    only `[lang.vi].personal_dict` + real English lexicon words block rescue, so global
        //    English `personal_dict` entries cannot accidentally silence Vietnamese Telex rescue).
        //    Note: pure-ASCII fallback typos like `"ttieengs"` forward every key without a mid-word
        //    Lotus commit even in active Uinput mode (`sim.preedit_before_break == token && !sim.is_composed_vietnamese`),
        //    so rescue remains eligible whenever either `lotus_composing` is true OR `sim` itself expected
        //    zero mid-word mutations!
        let can_rescue_vi =
            lotus_composing || (!sim.is_composed_vietnamese && sim.preedit_before_break == token);
        let shield_from_vi_rescue = is_lexicon_english_raw
            || is_lexicon_english_screen
            || self.store.is_lang_only_personal_word("vi", &lower)
            || (lotus_composing && self.store.is_lang_only_personal_word("vi", &screen_lower));
        if can_rescue_vi && !shield_from_vi_rescue {
            if let Some(rescue) = self.rescue_vi_telex(token, lotus_opts) {
                if !self
                    .store
                    .is_pair_blocked(&lower, &rescue.replacement.to_lowercase())
                {
                    return EvalOutcome::Autocorrect {
                        replacement: rescue.replacement,
                        kind: MatchKind::VietnameseTelexRescue,
                        score: 9.2,
                        margin: 9.2,
                    };
                }
            }
        }

        // If Lotus's 3x mark undo changed the on-screen character count (`tieeeng` or `feeel -> feel`),
        // do not run fuzzy English guesses on the raw keystrokes. But when the token is not a known
        // English word, still let it be learned/logged (`do_not_learn: false`) so uinput-restore
        // words that no rescue candidate matched (e.g. `duonxg`) feed `log_unfixed_unknown` and
        // `3x -> personal_dict` staging exactly like the forwarded-gibberish path does.
        if lotus_composing && sim.screen_chars != token.chars().count() {
            return EvalOutcome::Unchanged {
                reason: if is_known_english {
                    MatchKind::ValidWord
                } else {
                    MatchKind::Bypass
                },
                do_not_learn: is_known_english,
            };
        }

        // 5. Fallback to Layer 1 English scorer (`dont -> don't`, `teh -> the`, `chagnes -> changes`):
        if let Some(scorer) = self.scorer() {
            return scorer.evaluate(token, ctx, self.store.scorer_config(), &self.store);
        }

        EvalOutcome::Unchanged {
            reason: if is_known_english {
                MatchKind::ValidWord
            } else {
                MatchKind::Bypass
            },
            do_not_learn: is_known_english,
        }
    }

    pub fn explain_token(&self, token: &str, ctx: &EvalContext<'_>) -> Explanation {
        if self.store.active_lang().as_str() == "vi" && self.store.compiled().vi_lotus_enabled {
            let outcome = self.evaluate_token(token, ctx);
            let mut top_candidates = Vec::new();
            if let EvalOutcome::Autocorrect {
                ref replacement,
                kind: kind @ (MatchKind::VietnameseTelexRescue | MatchKind::ShorthandExpansion),
                score,
                ..
            } = outcome
            {
                top_candidates.push(Candidate {
                    replacement: replacement.clone(),
                    score,
                    edit_cost: 1.0,
                    freq: 250,
                    bigram_boost: 0.0,
                    kind,
                });
            }
            if let Some(scorer) = self.scorer() {
                let en_exp = scorer.explain(token, ctx, self.store.scorer_config(), &self.store);
                top_candidates.extend(en_exp.top_candidates);
            }
            return Explanation {
                input: token.to_string(),
                prev_word: ctx.prev_word.map(str::to_string),
                boundary_punct: ctx.boundary_punct,
                outcome,
                top_candidates,
            };
        }

        if let Some(scorer) = self.scorer_for(self.store.active_lang().as_str()) {
            return scorer.explain(token, ctx, self.store.scorer_config(), &self.store);
        }
        Explanation {
            input: token.to_string(),
            prev_word: ctx.prev_word.map(str::to_string),
            boundary_punct: ctx.boundary_punct,
            outcome: self.evaluate_token(token, ctx),
            top_candidates: Vec::new(),
        }
    }

    /// Records an untouched unknown word into Layer 2 (`3x -> personal_dict`), skipping raw
    /// unswitched Vietnamese Telex syllables (`>= 4` ASCII chars that compose to a valid accented
    /// Vietnamese word) when typing in English mode without prior undo confirmation.
    pub fn record_untouched_unknown_token(&mut self, lang: &LangId, word: &str) -> LearnEvent {
        if lang.as_str() == "en" {
            let trimmed = word.trim();
            let lower = trimmed.to_lowercase();
            if trimmed.len() >= 4
                && trimmed.is_ascii()
                && !self.store.is_personal_word(&lower)
                && !self
                    .registry
                    .lexicon("en")
                    .is_some_and(|l| l.contains_any_variant_or_elided(&lower))
            {
                let undo_prefix = format!("en:{lower}->");
                let has_prior_undo = self
                    .store
                    .state()
                    .staged_undo_keep
                    .iter()
                    .any(|(k, &v)| v >= 1 && k.starts_with(&undo_prefix));
                if !has_prior_undo {
                    let sim = self.simulate_vi_lotus(&lower, self.store.lotus_options());
                    if sim.is_valid_vietnamese && sim.is_composed_vietnamese {
                        return LearnEvent::None;
                    }
                }
            }
        }
        let lex = self.registry.lexicon(lang.as_str());
        record_untouched_unknown(&mut self.store, lex, lang, word)
    }
}

/// Normalized input event delivered by the host adapter (`fcitx5`, CLI REPL, or test harness)
/// BEFORE the key reaches the client application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyInput {
    Char(char),
    BackSpace,
    CtrlBackSpace,
    Enter,
    Tab,
    NavOrClick,
    FocusLost,
}

/// Imperative host action returned by `SmartKeySession::process_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAction {
    /// Do not filter the key event; let the client application handle it normally.
    PassThrough,
    /// Consume/filter the current key event, delete `delete_chars` Unicode scalar characters
    /// before the cursor, and insert `insert`.
    Replace { delete_chars: usize, insert: String },
}

/// Result of processing a single keystroke or mobile word hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOutput {
    pub action: SessionAction,
    pub matched_kind: Option<MatchKind>,
    pub learn_events: Vec<LearnEvent>,
}

impl SessionOutput {
    pub fn passthrough() -> Self {
        Self {
            action: SessionAction::PassThrough,
            matched_kind: None,
            learn_events: Vec::new(),
        }
    }

    fn with_events(learn_events: Vec<LearnEvent>) -> Self {
        Self {
            action: SessionAction::PassThrough,
            matched_kind: None,
            learn_events: learn_events
                .into_iter()
                .filter(|e| !matches!(e, LearnEvent::None))
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
struct PostAutocorrectOrigin {
    raw_typed: String,
    prev_word_before: Option<String>,
    is_snippet: bool,
}

enum Mode {
    Idle,
    Composing {
        buf: String,
        peak_before_bs: Option<String>,
        origin: InputOrigin,
    },
    /// Saw `.` or `:` right after `word`; waiting 1 key to distinguish prose (`"chagnes. "`)
    /// from URLs/filenames/ports (`"teh.com"`, `"config.json."`, `"localhost:8080"`).
    PendingTrailingDot {
        word: String,
        peak_before_bs: Option<String>,
        dot_char: char,
        on_screen_word_chars: Option<usize>,
        post_autocorrect: Option<PostAutocorrectOrigin>,
    },
    /// Saw `;` right after `word`; waiting 1 key to distinguish pinky contraction slip (`"don;t"`)
    /// from prose semicolon (`"chagnes; "`).
    PendingSemicolon {
        word: String,
        peak_before_bs: Option<String>,
        on_screen_word_chars: Option<usize>,
        post_autocorrect: Option<PostAutocorrectOrigin>,
    },
    /// Word was just committed untouched followed by `trailing_spaces` spaces (`' '`),
    /// allowing post-Space `BackSpace` or `Ctrl+BackSpace` manual typo learning.
    AfterWord {
        word: String,
        prev_word_before: Option<String>,
        trailing_spaces: usize,
    },
    /// Word was just autocorrected (`raw_typed -> applied + trailing_boundaries`);
    /// post-correction `BackSpace` follows `settings.undo_mode` (`"step"`, `"immediate"`, `"off"`)
    /// within `undo_window_ms`.
    JustAutocorrected {
        raw_typed: String,
        applied: String,
        prev_word_before: Option<String>,
        trailing_boundaries: String,
        is_snippet: bool,
        corrected_at_ms: u64,
    },
    /// User pressed `1x BackSpace` on `JustAutocorrected`, restoring `buf = raw_typed`.
    EditingRestored {
        raw_typed: String,
        rejected: String,
        prev_word_before: Option<String>,
        buf: String,
        is_snippet: bool,
    },
    /// User backspaced across `Space` into the previously committed untouched word.
    EditingPrevious {
        orig_word: String,
        prev_word_before: Option<String>,
        buf: String,
    },
}

/// Zero-allocation borrowed snapshot of `SmartKeySession`'s live shadow buffer and mode
/// for the flight-recorder diagnostic trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionDebugSnapshot<'a> {
    pub mode: &'static str,
    pub active_buf: &'a str,
    pub aux_text: &'a str,
    pub prev_word: &'a str,
    pub trailing_count: usize,
    pub lotus_composing: bool,
}

/// Per-window (`InputContext`) shadow-buffer state machine.
pub struct SmartKeySession {
    profile: AppProfile,
    lang: LangId,
    app: Option<String>,
    mode: Mode,
    prev_word: Option<String>,
    at_sentence_start: bool,
    suppress_sentence_cap: bool,
    in_markdown_code: bool,
    /// True when `fcitx5-lotus` is actively composing (false when Lotus is in `Off`/`Default` mode
    /// and forwarding raw ASCII keys without committing runes).
    lotus_composing: bool,
    /// Timestamp of the most recent key event processed on this session (guards `SurroundingText`
    /// reconciliation against asynchronous toolkit key-echo lag).
    last_key_ms: u64,
    /// Unknown non-word (`do_not_learn == false`) waiting for confirmation that the user moved on
    /// without backspacing it (`3x -> personal_dict`).
    pending_unknown: Option<(LangId, String)>,
    /// Tracks pending mobile `revertCommit` for HeliBoard word-level hooks.
    mobile_pending_undo: Option<(String, String, Option<String>)>,
}

impl Default for SmartKeySession {
    fn default() -> Self {
        Self::new()
    }
}

impl SmartKeySession {
    pub fn new() -> Self {
        Self {
            profile: AppProfile::Prose,
            lang: LangId::en(),
            app: None,
            mode: Mode::Idle,
            prev_word: None,
            at_sentence_start: true,
            suppress_sentence_cap: false,
            in_markdown_code: false,
            lotus_composing: true,
            last_key_ms: 0,
            pending_unknown: None,
            mobile_pending_undo: None,
        }
    }

    pub fn set_lotus_composing(&mut self, composing: bool) {
        self.lotus_composing = composing;
    }

    pub fn lotus_composing(&self) -> bool {
        self.lotus_composing
    }

    pub fn set_profile(&mut self, profile: AppProfile) {
        if profile == AppProfile::Bypass {
            self.reset_state_only();
        }
        self.profile = profile;
    }

    pub fn profile(&self) -> AppProfile {
        self.profile
    }

    pub fn set_lang(&mut self, lang: LangId) {
        self.lang = lang;
    }

    pub fn lang(&self) -> &LangId {
        &self.lang
    }

    pub fn set_app(&mut self, app: Option<&str>) {
        self.app = app.map(str::to_string);
    }

    pub fn app(&self) -> Option<&str> {
        self.app.as_deref()
    }

    pub fn in_markdown_code(&self) -> bool {
        self.in_markdown_code
    }

    /// Returns a zero-allocation borrowed snapshot of the session's current state machine mode
    /// and shadow buffer for flight-recorder tracing.
    pub fn debug_snapshot(&self) -> SessionDebugSnapshot<'_> {
        let (mode, active_buf, aux_text, trailing_count) = match &self.mode {
            Mode::Idle => ("Idle", "", "", 0),
            Mode::Composing {
                buf,
                peak_before_bs,
                ..
            } => (
                "Composing",
                buf.as_str(),
                peak_before_bs.as_deref().unwrap_or(""),
                0,
            ),
            Mode::PendingTrailingDot {
                word,
                peak_before_bs,
                ..
            } => (
                "PendingDot",
                word.as_str(),
                peak_before_bs.as_deref().unwrap_or(""),
                1,
            ),
            Mode::PendingSemicolon {
                word,
                peak_before_bs,
                ..
            } => (
                "PendingSemi",
                word.as_str(),
                peak_before_bs.as_deref().unwrap_or(""),
                1,
            ),
            Mode::AfterWord {
                word,
                trailing_spaces,
                ..
            } => ("AfterWord", word.as_str(), "", *trailing_spaces),
            Mode::JustAutocorrected {
                raw_typed,
                applied,
                trailing_boundaries,
                ..
            } => (
                "JustAutocorrected",
                applied.as_str(),
                raw_typed.as_str(),
                trailing_boundaries.chars().count(),
            ),
            Mode::EditingRestored { raw_typed, buf, .. } => {
                ("EditingRestored", buf.as_str(), raw_typed.as_str(), 0)
            }
            Mode::EditingPrevious { orig_word, buf, .. } => {
                ("EditingPrevious", buf.as_str(), orig_word.as_str(), 0)
            }
        };
        SessionDebugSnapshot {
            mode,
            active_buf,
            aux_text,
            prev_word: self.prev_word.as_deref().unwrap_or(""),
            trailing_count,
            lotus_composing: self.lotus_composing,
        }
    }

    fn reset_state_only(&mut self) {
        self.mode = Mode::Idle;
        self.prev_word = None;
        self.at_sentence_start = true;
        self.suppress_sentence_cap = false;
        self.in_markdown_code = false;
        self.lotus_composing = true;
        self.last_key_ms = 0;
        self.pending_unknown = None;
        self.mobile_pending_undo = None;
    }

    /// Reconciles the shadow buffer when the client notifies `InputContextSurroundingTextUpdated`
    /// (e.g. user clicked the mouse elsewhere mid-word or post-correction without pressing a
    /// cursor-movement key). Ignores updates within `80 ms` (or `400 ms` during active `vi` Lotus
    /// uinput bursts) of a keystroke so async Wayland/GTK key-echo lag never resets active typing.
    pub fn reconcile_surrounding(
        &mut self,
        engine: &mut SmartKeyEngine,
        surrounding_text: &str,
        cursor_chars: usize,
        now_ms: u64,
    ) {
        let echo_guard_ms = if self.lang.as_str() == "vi" && self.lotus_composing {
            400
        } else if matches!(self.mode, Mode::EditingRestored { .. }) {
            180
        } else {
            80
        };
        if now_ms.saturating_sub(self.last_key_ms) <= echo_guard_ms {
            return;
        }

        let mut total = 0usize;
        let mut byte_end = surrounding_text.len();
        for (b_idx, _) in surrounding_text.char_indices() {
            if total == cursor_chars {
                byte_end = b_idx;
                break;
            }
            total += 1;
        }
        if total < cursor_chars {
            return;
        }
        let before = &surrounding_text[..byte_end];

        fn ends_with_or_window_suffix(before: &str, expected: &str) -> bool {
            if expected.is_empty() {
                return true;
            }
            if before.is_empty() {
                return false;
            }
            before.ends_with(expected)
                || (before.len() < expected.len() && expected.ends_with(before))
        }

        fn tail_token_matches(tail_token: &str, buf: &str) -> bool {
            let t = tail_token.to_lowercase();
            let b = buf.to_lowercase();
            t == b || (!t.is_empty() && b.starts_with(&t))
        }

        let should_reset = match &self.mode {
            Mode::JustAutocorrected {
                raw_typed,
                applied,
                trailing_boundaries,
                ..
            } => {
                let expected = format!("{applied}{trailing_boundaries}");
                let dropped_dup = format!("{raw_typed}{applied}{trailing_boundaries}");
                !ends_with_or_window_suffix(before, &expected)
                    || (!raw_typed.is_empty() && before.ends_with(&dropped_dup))
            }
            Mode::AfterWord {
                word,
                trailing_spaces,
                ..
            } if !(self.lang.as_str() == "vi" && self.lotus_composing) => {
                let expected = format!("{word}{}", " ".repeat(*trailing_spaces));
                !ends_with_or_window_suffix(before, &expected)
            }
            Mode::Composing { buf, .. }
            | Mode::EditingRestored { buf, .. }
            | Mode::EditingPrevious { buf, .. }
                if !buf.is_empty() =>
            {
                let tail_token = before
                    .rsplit(|c: char| {
                        Self::is_immediate_boundary(c)
                            || Self::is_opening_delimiter_when_idle(c)
                            || c == '\n'
                            || c == '\r'
                    })
                    .next()
                    .unwrap_or("");
                if self.lang.as_str() == "vi" && self.lotus_composing {
                    let preedit = engine
                        .simulate_vi_lotus(buf, engine.store.lotus_options())
                        .preedit_before_break;
                    !tail_token_matches(tail_token, buf)
                        && !tail_token_matches(tail_token, &preedit)
                } else {
                    !tail_token_matches(tail_token, buf)
                }
            }
            _ => false,
        };
        if should_reset {
            let _ = self.process_key(engine, KeyInput::NavOrClick, now_ms);
        }
    }

    /// Flushes any confirmed untouched unknown word before resetting session state,
    /// after hot-reloading `rules.toml` / `learner_state.json` if modified externally
    /// so external CLI edits (e.g. `smartkey log --clear`, `smartkey toggle`) are never clobbered.
    pub fn reset(&mut self, engine: &mut SmartKeyEngine) -> Vec<LearnEvent> {
        engine.store.set_active_lang(self.lang.clone());
        engine.store.set_current_app(self.app.as_deref());
        engine.store.reload_if_modified();
        let mut events = Vec::new();
        if let Mode::EditingRestored {
            ref raw_typed,
            ref buf,
            ..
        } = self.mode
        {
            if buf.eq_ignore_ascii_case(raw_typed) {
                self.commit_active_without_autocorrect(engine, &mut events);
            }
        }
        self.flush_pending_unknown(engine, &mut events);
        let _ = engine.store.flush_dirty_state();
        self.reset_state_only();
        events
    }

    fn flush_pending_unknown(&mut self, engine: &mut SmartKeyEngine, events: &mut Vec<LearnEvent>) {
        if let Some((lang, word)) = self.pending_unknown.take() {
            let ev = engine.record_untouched_unknown_token(&lang, &word);
            if !matches!(ev, LearnEvent::None) {
                events.push(ev);
            }
        }
    }

    #[inline]
    fn is_immediate_boundary(ch: char) -> bool {
        matches!(
            ch,
            ' ' | '\t' | ',' | '!' | '?' | ')' | ']' | '}' | '"' | '”'
        )
    }

    #[inline]
    fn is_opening_delimiter_when_idle(ch: char) -> bool {
        matches!(ch, '(' | '[' | '{' | '"' | '“')
    }

    #[inline]
    fn ends_sentence(ch: char) -> bool {
        matches!(ch, '.' | '!' | '?')
    }

    fn update_context_after_commit(
        &mut self,
        engine: &SmartKeyEngine,
        final_text: &str,
        punct: Option<char>,
        has_space: bool,
    ) {
        let last_word = final_text
            .split_whitespace()
            .last()
            .unwrap_or(final_text)
            .trim_matches(|c: char| !c.is_alphabetic() && c != '\'' && c != '’');

        if let Some(p) = punct {
            if Self::ends_sentence(p) {
                self.prev_word = None;
                self.at_sentence_start = true;
                let abbrs = engine
                    .lexicon_for(self.lang.as_str())
                    .map(|l| l.rules().casing.abbreviations.as_slice())
                    .unwrap_or(&[]);
                self.suppress_sentence_cap =
                    p == '.' && is_abbreviation_with_rules(last_word, abbrs);
                return;
            }
            // Non-sentence punctuation (`,`, `;`, `:`, `)`, `"`) breaks strict bigram adjacency
            self.prev_word = None;
            self.at_sentence_start = false;
            self.suppress_sentence_cap = false;
            return;
        }

        if has_space && !last_word.is_empty() {
            self.prev_word = Some(last_word.to_string());
            self.at_sentence_start = false;
            self.suppress_sentence_cap = false;
        } else {
            self.prev_word = None;
            self.at_sentence_start = false;
            self.suppress_sentence_cap = false;
        }
    }

    /// Processes a single key event on desktop (`fcitx5-lotus` / CLI REPL) BEFORE the key is
    /// forwarded to the client application.
    pub fn process_key(
        &mut self,
        engine: &mut SmartKeyEngine,
        key: KeyInput,
        now_ms: u64,
    ) -> SessionOutput {
        if !engine.store.compiled().enabled || self.profile == AppProfile::Bypass {
            self.reset_state_only();
            return SessionOutput::passthrough();
        }
        self.last_key_ms = now_ms;
        engine.store.set_active_lang(self.lang.clone());
        engine.store.set_current_app(self.app.as_deref());

        match key {
            KeyInput::NavOrClick | KeyInput::Tab => {
                let mut events = Vec::new();
                if let Mode::EditingRestored {
                    ref raw_typed,
                    ref buf,
                    ..
                } = self.mode
                {
                    if buf.eq_ignore_ascii_case(raw_typed) {
                        self.commit_active_without_autocorrect(engine, &mut events);
                    }
                }
                self.flush_pending_unknown(engine, &mut events);
                self.mode = Mode::Idle;
                self.prev_word = None;
                self.suppress_sentence_cap = false;
                SessionOutput::with_events(events)
            }
            KeyInput::FocusLost => {
                let events = self.reset(engine);
                SessionOutput::with_events(events)
            }
            KeyInput::Enter => {
                // Never mutate text on `Enter` so chat messages (Discord/Telegram/Slack) are never
                // unexpectedly altered at submit time without an undo opportunity.
                let mut events = Vec::new();
                self.commit_active_without_autocorrect(engine, &mut events);
                self.flush_pending_unknown(engine, &mut events);
                self.mode = Mode::Idle;
                self.prev_word = None;
                self.at_sentence_start = true;
                self.suppress_sentence_cap = false;
                self.in_markdown_code = false;
                SessionOutput::with_events(events)
            }
            KeyInput::BackSpace => self.handle_backspace(engine, now_ms),
            KeyInput::CtrlBackSpace => self.handle_ctrl_backspace(engine),
            KeyInput::Char(ch) => self.handle_char(engine, ch, now_ms),
        }
    }

    fn commit_active_without_autocorrect(
        &mut self,
        engine: &mut SmartKeyEngine,
        events: &mut Vec<LearnEvent>,
    ) {
        let mode = std::mem::replace(&mut self.mode, Mode::Idle);
        match mode {
            Mode::EditingRestored {
                raw_typed,
                rejected,
                prev_word_before,
                buf,
                is_snippet,
            } => {
                if !is_snippet && !buf.is_empty() {
                    let lex = engine.registry.lexicon(self.lang.as_str());
                    let evs = resolve_restored_edit(
                        &mut engine.store,
                        lex,
                        &self.lang,
                        prev_word_before.as_deref(),
                        &raw_typed,
                        &rejected,
                        &buf,
                    );
                    events.extend(evs.into_iter().filter(|e| !matches!(e, LearnEvent::None)));
                } else if is_snippet && !buf.is_empty() {
                    let res = if buf.eq_ignore_ascii_case(&raw_typed) {
                        UndoResolution::KeptVerbatim
                    } else if buf.eq_ignore_ascii_case(&rejected) {
                        UndoResolution::SelfConfirmCancelled
                    } else {
                        UndoResolution::EditedToThird { final_word: buf }
                    };
                    engine
                        .store
                        .resolve_pending_undo_log(&raw_typed, &rejected, res);
                }
            }
            Mode::EditingPrevious {
                orig_word,
                prev_word_before,
                buf,
            } => {
                if !buf.is_empty() {
                    {
                        let (vi_dict, store) = engine.vi_dict_and_store_mut();
                        self.maybe_record_vi_passthrough_repair(
                            vi_dict, store, &orig_word, &buf, events,
                        );
                    }
                    let lex = engine.registry.lexicon(self.lang.as_str());
                    let ev = record_manual_edit(
                        &mut engine.store,
                        lex,
                        &self.lang,
                        prev_word_before.as_deref(),
                        &orig_word,
                        &buf,
                    );
                    let check_unknown = matches!(
                        ev,
                        LearnEvent::Ignored(
                            crate::learner::SkipReason::TargetNotValidWord
                                | crate::learner::SkipReason::SameWord
                        )
                    );
                    if !matches!(ev, LearnEvent::None) {
                        events.push(ev);
                    }
                    if check_unknown && !self.in_markdown_code {
                        let ctx = EvalContext {
                            prev_word: prev_word_before.as_deref(),
                            boundary_punct: None,
                            at_sentence_start: self.at_sentence_start,
                            suppress_sentence_cap: self.suppress_sentence_cap,
                            in_markdown_code: self.in_markdown_code,
                            profile: self.profile,
                        };
                        if let EvalOutcome::Unchanged {
                            do_not_learn: false,
                            ..
                        } = engine.evaluate_token_internal(&buf, &ctx, self.lotus_composing)
                        {
                            self.flush_pending_unknown(engine, events);
                            self.pending_unknown = Some((self.lang.clone(), buf));
                        }
                    }
                }
            }
            Mode::PendingTrailingDot {
                word: buf,
                dot_char,
                post_autocorrect: Some(_),
                ..
            } => {
                if !buf.is_empty() {
                    self.update_context_after_commit(engine, &buf, Some(dot_char), false);
                }
            }
            Mode::PendingSemicolon {
                word: buf,
                post_autocorrect: Some(_),
                ..
            } => {
                if !buf.is_empty() {
                    self.update_context_after_commit(engine, &buf, Some(';'), false);
                }
            }
            Mode::PendingTrailingDot {
                word: buf,
                peak_before_bs,
                post_autocorrect: None,
                ..
            }
            | Mode::PendingSemicolon {
                word: buf,
                peak_before_bs,
                post_autocorrect: None,
                ..
            }
            | Mode::Composing {
                buf,
                peak_before_bs,
                ..
            } => {
                if let Some(peak) = peak_before_bs {
                    if !buf.is_empty() {
                        let lex = engine.registry.lexicon(self.lang.as_str());
                        let ev = record_manual_edit(
                            &mut engine.store,
                            lex,
                            &self.lang,
                            self.prev_word.as_deref(),
                            &peak,
                            &buf,
                        );
                        let check_unknown = matches!(
                            ev,
                            LearnEvent::Ignored(
                                crate::learner::SkipReason::TargetNotValidWord
                                    | crate::learner::SkipReason::SameWord
                            )
                        );
                        if !matches!(ev, LearnEvent::None) {
                            events.push(ev);
                        }
                        if check_unknown && !self.in_markdown_code {
                            let ctx = EvalContext {
                                prev_word: self.prev_word.as_deref(),
                                boundary_punct: None,
                                at_sentence_start: self.at_sentence_start,
                                suppress_sentence_cap: self.suppress_sentence_cap,
                                in_markdown_code: self.in_markdown_code,
                                profile: self.profile,
                            };
                            if let EvalOutcome::Unchanged {
                                do_not_learn: false,
                                ..
                            } = engine.evaluate_token(&buf, &ctx)
                            {
                                self.flush_pending_unknown(engine, events);
                                self.pending_unknown = Some((self.lang.clone(), buf));
                            }
                        }
                    }
                } else if !buf.is_empty() && !self.in_markdown_code {
                    let ctx = EvalContext {
                        prev_word: self.prev_word.as_deref(),
                        boundary_punct: None,
                        at_sentence_start: self.at_sentence_start,
                        suppress_sentence_cap: self.suppress_sentence_cap,
                        in_markdown_code: self.in_markdown_code,
                        profile: self.profile,
                    };
                    if let EvalOutcome::Unchanged {
                        do_not_learn: false,
                        ..
                    } = engine.evaluate_token(&buf, &ctx)
                    {
                        self.flush_pending_unknown(engine, events);
                        self.pending_unknown = Some((self.lang.clone(), buf));
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_backspace(&mut self, engine: &mut SmartKeyEngine, now_ms: u64) -> SessionOutput {
        let mode = std::mem::replace(&mut self.mode, Mode::Idle);
        match mode {
            Mode::Idle => {
                if let Some((_, pending_w)) = self.pending_unknown.take() {
                    engine.store.cancel_pending_unfixed_unknown(&pending_w);
                }
                self.prev_word = None;
                SessionOutput::passthrough()
            }
            Mode::JustAutocorrected {
                raw_typed,
                applied,
                prev_word_before,
                mut trailing_boundaries,
                is_snippet,
                corrected_at_ms,
            } => {
                let (undo_window, undo_mode) = {
                    let c = engine.store.compiled();
                    (c.undo_window_ms, c.undo_mode)
                };
                let in_window = now_ms.saturating_sub(corrected_at_ms) <= undo_window;
                match undo_mode {
                    UndoMode::Immediate => {
                        if in_window {
                            engine.store.log_backspace_undo(
                                &raw_typed,
                                &applied,
                                UndoResolution::Triggered,
                            );
                            let delete_chars =
                                applied.chars().count() + trailing_boundaries.chars().count();
                            let restored = raw_typed.clone();
                            self.prev_word = prev_word_before.clone();
                            self.mode = Mode::EditingRestored {
                                raw_typed,
                                rejected: applied,
                                prev_word_before,
                                buf: restored.clone(),
                                is_snippet,
                            };
                            SessionOutput {
                                action: SessionAction::Replace {
                                    delete_chars,
                                    insert: restored,
                                },
                                matched_kind: None,
                                learn_events: Vec::new(),
                            }
                        } else {
                            self.prev_word = None;
                            SessionOutput::passthrough()
                        }
                    }
                    UndoMode::Step => {
                        if !in_window {
                            self.prev_word = None;
                            SessionOutput::passthrough()
                        } else if !trailing_boundaries.is_empty() {
                            trailing_boundaries.pop();
                            if trailing_boundaries.is_empty() {
                                self.at_sentence_start = false;
                                self.suppress_sentence_cap = false;
                            }
                            self.mode = Mode::JustAutocorrected {
                                raw_typed,
                                applied,
                                prev_word_before,
                                trailing_boundaries,
                                is_snippet,
                                corrected_at_ms: now_ms,
                            };
                            SessionOutput::passthrough()
                        } else {
                            engine.store.log_backspace_undo(
                                &raw_typed,
                                &applied,
                                UndoResolution::Triggered,
                            );
                            let delete_chars = applied.chars().count();
                            let restored = raw_typed.clone();
                            self.prev_word = prev_word_before.clone();
                            self.mode = Mode::EditingRestored {
                                raw_typed,
                                rejected: applied,
                                prev_word_before,
                                buf: restored.clone(),
                                is_snippet,
                            };
                            SessionOutput {
                                action: SessionAction::Replace {
                                    delete_chars,
                                    insert: restored,
                                },
                                matched_kind: None,
                                learn_events: Vec::new(),
                            }
                        }
                    }
                    UndoMode::Off => {
                        if in_window && !trailing_boundaries.is_empty() {
                            trailing_boundaries.pop();
                            if trailing_boundaries.is_empty() {
                                self.at_sentence_start = false;
                                self.suppress_sentence_cap = false;
                            }
                            self.mode = Mode::JustAutocorrected {
                                raw_typed,
                                applied,
                                prev_word_before,
                                trailing_boundaries,
                                is_snippet,
                                corrected_at_ms: now_ms,
                            };
                        } else if trailing_boundaries.is_empty() {
                            let mut buf = applied;
                            buf.pop();
                            if buf.is_empty() {
                                self.prev_word = None;
                            } else {
                                self.prev_word = prev_word_before;
                                self.mode = Mode::Composing {
                                    buf,
                                    peak_before_bs: None,
                                    origin: InputOrigin::PlainAscii,
                                };
                            }
                        } else {
                            self.prev_word = None;
                        }
                        SessionOutput::passthrough()
                    }
                }
            }
            Mode::EditingRestored {
                raw_typed,
                rejected,
                prev_word_before,
                mut buf,
                is_snippet,
            } => {
                if buf.is_empty() {
                    // Clause-Erase Cancel: user held BackSpace past index 0 into previous word!
                    engine.store.resolve_pending_undo_log(
                        &raw_typed,
                        &rejected,
                        UndoResolution::ClauseErased,
                    );
                    self.prev_word = None;
                } else {
                    buf.pop();
                    self.mode = Mode::EditingRestored {
                        raw_typed,
                        rejected,
                        prev_word_before,
                        buf,
                        is_snippet,
                    };
                }
                SessionOutput::passthrough()
            }
            Mode::AfterWord {
                word,
                prev_word_before,
                trailing_spaces,
            } => {
                // Cancel untouched-unknown staging since user immediately backspaced into this word!
                if let Some((_, pending_w)) = self.pending_unknown.take() {
                    engine.store.cancel_pending_unfixed_unknown(&pending_w);
                }
                if trailing_spaces > 1 {
                    self.mode = Mode::AfterWord {
                        word,
                        prev_word_before,
                        trailing_spaces: trailing_spaces - 1,
                    };
                } else {
                    self.prev_word = prev_word_before.clone();
                    self.mode = Mode::EditingPrevious {
                        orig_word: word.clone(),
                        prev_word_before,
                        buf: word,
                    };
                }
                SessionOutput::passthrough()
            }
            Mode::EditingPrevious {
                orig_word,
                prev_word_before,
                mut buf,
            } => {
                if buf.is_empty() {
                    // Row 6: Backspaced past index 0 into a 2nd word -> ignore (`>1` word erase)!
                    self.prev_word = None;
                } else {
                    buf.pop();
                    self.mode = Mode::EditingPrevious {
                        orig_word,
                        prev_word_before,
                        buf,
                    };
                }
                SessionOutput::passthrough()
            }
            Mode::PendingTrailingDot {
                word,
                peak_before_bs,
                post_autocorrect,
                ..
            }
            | Mode::PendingSemicolon {
                word,
                peak_before_bs,
                post_autocorrect,
                ..
            } => {
                // Backspace deletes the provisional `.`, `:`, or `;`
                if let Some(origin) = post_autocorrect {
                    self.at_sentence_start = false;
                    self.suppress_sentence_cap = false;
                    self.mode = Mode::JustAutocorrected {
                        raw_typed: origin.raw_typed,
                        applied: word,
                        prev_word_before: origin.prev_word_before,
                        trailing_boundaries: String::new(),
                        is_snippet: origin.is_snippet,
                        corrected_at_ms: now_ms,
                    };
                } else {
                    self.mode = Mode::Composing {
                        buf: word,
                        peak_before_bs,
                        origin: InputOrigin::PlainAscii,
                    };
                }
                SessionOutput::passthrough()
            }
            Mode::Composing {
                mut buf,
                mut peak_before_bs,
                origin,
            } => {
                if buf.is_empty() {
                    self.prev_word = None;
                } else {
                    let cur_chars = buf.chars().count();
                    if cur_chars >= 3 || self.lang.as_str() == "vi" {
                        let replace_peak = match &peak_before_bs {
                            None => true,
                            Some(prev_peak) => cur_chars > prev_peak.chars().count(),
                        };
                        if replace_peak {
                            peak_before_bs = Some(buf.clone());
                        }
                    }
                    buf.pop();
                    self.mode = Mode::Composing {
                        buf,
                        peak_before_bs,
                        origin,
                    };
                }
                SessionOutput::passthrough()
            }
        }
    }

    fn handle_ctrl_backspace(&mut self, engine: &mut SmartKeyEngine) -> SessionOutput {
        let mode = std::mem::replace(&mut self.mode, Mode::Idle);
        match mode {
            Mode::JustAutocorrected {
                raw_typed,
                applied,
                prev_word_before,
                trailing_boundaries,
                is_snippet,
                corrected_at_ms,
            } => {
                let (undo_window, undo_mode) = {
                    let c = engine.store.compiled();
                    (c.undo_window_ms, c.undo_mode)
                };
                let single_word_space = !applied.chars().any(char::is_whitespace)
                    && trailing_boundaries.chars().all(char::is_whitespace);
                if undo_mode == UndoMode::Off {
                    self.prev_word = if single_word_space {
                        prev_word_before
                    } else {
                        None
                    };
                } else if self.last_key_ms.saturating_sub(corrected_at_ms) <= undo_window
                    && single_word_space
                {
                    engine.store.log_backspace_undo(
                        &raw_typed,
                        &applied,
                        UndoResolution::Triggered,
                    );
                    self.prev_word = prev_word_before.clone();
                    self.mode = Mode::EditingRestored {
                        raw_typed,
                        rejected: applied,
                        prev_word_before,
                        buf: String::new(),
                        is_snippet,
                    };
                } else {
                    self.prev_word = None;
                }
            }
            Mode::AfterWord {
                word,
                prev_word_before,
                ..
            } => {
                if let Some((_, pending_w)) = self.pending_unknown.take() {
                    engine.store.cancel_pending_unfixed_unknown(&pending_w);
                }
                self.prev_word = prev_word_before.clone();
                self.mode = Mode::EditingPrevious {
                    orig_word: word,
                    prev_word_before,
                    buf: String::new(),
                };
            }
            Mode::EditingPrevious {
                orig_word,
                prev_word_before,
                buf,
            } if !buf.is_empty() => {
                self.mode = Mode::EditingPrevious {
                    orig_word,
                    prev_word_before,
                    buf: String::new(),
                };
            }
            Mode::EditingRestored {
                raw_typed,
                rejected,
                prev_word_before,
                buf,
                is_snippet,
            } => {
                if buf.is_empty() {
                    engine.store.resolve_pending_undo_log(
                        &raw_typed,
                        &rejected,
                        UndoResolution::ClauseErased,
                    );
                    self.prev_word = None;
                } else {
                    self.mode = Mode::EditingRestored {
                        raw_typed,
                        rejected,
                        prev_word_before,
                        buf: String::new(),
                        is_snippet,
                    };
                }
            }
            Mode::Composing {
                buf,
                peak_before_bs,
                origin,
            } if !buf.is_empty() => {
                let peak = if buf.chars().count() >= 3 {
                    Some(buf)
                } else {
                    peak_before_bs
                };
                self.mode = Mode::Composing {
                    buf: String::new(),
                    peak_before_bs: peak,
                    origin,
                };
            }
            _ => {
                // Deleting past index 0 into previous word -> reset cleanly to Idle
                if let Some((_, pending_w)) = self.pending_unknown.take() {
                    engine.store.cancel_pending_unfixed_unknown(&pending_w);
                }
                self.prev_word = None;
            }
        }
        SessionOutput::passthrough()
    }

    fn finish_post_autocorrect_pending_punct(
        &mut self,
        engine: &SmartKeyEngine,
        word: String,
        punct_char: char,
        origin: PostAutocorrectOrigin,
        next_ch: char,
        now_ms: u64,
    ) -> SessionOutput {
        if Self::is_immediate_boundary(next_ch) {
            self.update_context_after_commit(engine, &word, Some(punct_char), false);
            let mut trailing_boundaries = String::with_capacity(4);
            trailing_boundaries.push(punct_char);
            trailing_boundaries.push(next_ch);
            self.mode = Mode::JustAutocorrected {
                raw_typed: origin.raw_typed,
                applied: word,
                prev_word_before: origin.prev_word_before,
                trailing_boundaries,
                is_snippet: origin.is_snippet,
                corrected_at_ms: now_ms,
            };
        } else {
            self.prev_word = origin.prev_word_before;
            let mut merged = word;
            merged.push(punct_char);
            merged.push(next_ch);
            self.mode = Mode::Composing {
                buf: merged,
                peak_before_bs: None,
                origin: InputOrigin::PlainAscii,
            };
        }
        SessionOutput::passthrough()
    }

    fn handle_char(&mut self, engine: &mut SmartKeyEngine, ch: char, now_ms: u64) -> SessionOutput {
        // Backtick toggles inline Markdown code immunity (`` `code` ``)
        if ch == '`' {
            let mut learn_events = Vec::new();
            self.commit_active_without_autocorrect(engine, &mut learn_events);
            self.flush_pending_unknown(engine, &mut learn_events);
            self.mode = Mode::Idle;
            self.in_markdown_code = !self.in_markdown_code;
            return SessionOutput::with_events(learn_events);
        }

        let mut learn_events = Vec::new();
        let mode = std::mem::replace(&mut self.mode, Mode::Idle);

        match mode {
            // 1. Resolve provisional `.` or `:` lookahead (`"chagnes. "` vs `"teh.com"` / `"localhost:8080"`)
            Mode::PendingTrailingDot {
                word,
                peak_before_bs,
                dot_char,
                on_screen_word_chars,
                post_autocorrect,
            } => {
                if let Some(origin) = post_autocorrect {
                    return self.finish_post_autocorrect_pending_punct(
                        engine, word, dot_char, origin, ch, now_ms,
                    );
                }
                if Self::is_immediate_boundary(ch) {
                    return self.finish_token_at_boundary_inner(
                        engine,
                        word,
                        peak_before_bs,
                        Some(dot_char),
                        ch,
                        1, // `dot_char` is already in client buffer
                        on_screen_word_chars,
                        false,
                        now_ms,
                        learn_events,
                    );
                }
                // Followed by non-boundary (`teh.com`, `e.g`, `localhost:8080`, `...`): merge into token!
                let mut merged = word;
                merged.push(dot_char);
                merged.push(ch);
                self.mode = Mode::Composing {
                    buf: merged,
                    peak_before_bs,
                    origin: InputOrigin::PlainAscii,
                };
                return SessionOutput::passthrough();
            }

            // 2. Resolve provisional `;` lookahead (`"don;t "` vs `"chagnes; "`)
            Mode::PendingSemicolon {
                word,
                peak_before_bs,
                on_screen_word_chars,
                post_autocorrect,
            } => {
                if let Some(origin) = post_autocorrect {
                    return self.finish_post_autocorrect_pending_punct(
                        engine, word, ';', origin, ch, now_ms,
                    );
                }
                if Self::is_immediate_boundary(ch) {
                    return self.finish_token_at_boundary_inner(
                        engine,
                        word,
                        peak_before_bs,
                        Some(';'),
                        ch,
                        1, // `';'` is already in client buffer
                        on_screen_word_chars,
                        false,
                        now_ms,
                        learn_events,
                    );
                }
                let mut merged = word;
                merged.push(';');
                merged.push(ch);
                self.mode = Mode::Composing {
                    buf: merged,
                    peak_before_bs,
                    origin: InputOrigin::PlainAscii,
                };
                return SessionOutput::passthrough();
            }

            // 3. User is editing a restored word right after `1x BackSpace` undo
            Mode::EditingRestored {
                raw_typed,
                rejected,
                prev_word_before,
                mut buf,
                is_snippet,
            } => {
                if Self::is_immediate_boundary(ch)
                    || (!buf.is_empty() && matches!(ch, '.' | ':' | ';'))
                {
                    if !is_snippet && !buf.is_empty() {
                        let lex = engine.registry.lexicon(self.lang.as_str());
                        let evs = resolve_restored_edit(
                            &mut engine.store,
                            lex,
                            &self.lang,
                            prev_word_before.as_deref(),
                            &raw_typed,
                            &rejected,
                            &buf,
                        );
                        learn_events
                            .extend(evs.into_iter().filter(|e| !matches!(e, LearnEvent::None)));
                    } else if is_snippet && !buf.is_empty() {
                        let res = if buf.eq_ignore_ascii_case(&raw_typed) {
                            UndoResolution::KeptVerbatim
                        } else if buf.eq_ignore_ascii_case(&rejected) {
                            UndoResolution::SelfConfirmCancelled
                        } else {
                            UndoResolution::EditedToThird {
                                final_word: buf.clone(),
                            }
                        };
                        engine
                            .store
                            .resolve_pending_undo_log(&raw_typed, &rejected, res);
                    }
                    let is_space = ch == ' ' || ch == '\t';
                    let punct = if is_space { None } else { Some(ch) };
                    self.update_context_after_commit(engine, &buf, punct, is_space);
                    if is_space && !buf.is_empty() {
                        self.mode = Mode::AfterWord {
                            word: buf,
                            prev_word_before,
                            trailing_spaces: 1,
                        };
                    }
                    return SessionOutput::with_events(learn_events);
                }
                buf.push(ch);
                self.mode = Mode::EditingRestored {
                    raw_typed,
                    rejected,
                    prev_word_before,
                    buf,
                    is_snippet,
                };
                return SessionOutput::passthrough();
            }

            // 4. User backspaced across `Space` into the previous untouched word
            Mode::EditingPrevious {
                orig_word,
                prev_word_before,
                mut buf,
            } => {
                if Self::is_immediate_boundary(ch)
                    || (!buf.is_empty() && matches!(ch, '.' | ':' | ';'))
                {
                    let is_space = ch == ' ' || ch == '\t';
                    let punct = if is_space { None } else { Some(ch) };
                    if !buf.is_empty() {
                        {
                            let (vi_dict, store) = engine.vi_dict_and_store_mut();
                            self.maybe_record_vi_passthrough_repair(
                                vi_dict,
                                store,
                                &orig_word,
                                &buf,
                                &mut learn_events,
                            );
                        }
                        let lex = engine.registry.lexicon(self.lang.as_str());
                        let ev = record_manual_edit(
                            &mut engine.store,
                            lex,
                            &self.lang,
                            prev_word_before.as_deref(),
                            &orig_word,
                            &buf,
                        );
                        let check_unknown = matches!(
                            ev,
                            LearnEvent::Ignored(
                                crate::learner::SkipReason::TargetNotValidWord
                                    | crate::learner::SkipReason::SameWord
                            )
                        );
                        if !matches!(ev, LearnEvent::None) {
                            learn_events.push(ev);
                        }
                        if check_unknown && !self.in_markdown_code {
                            let ctx = EvalContext {
                                prev_word: prev_word_before.as_deref(),
                                boundary_punct: punct,
                                at_sentence_start: self.at_sentence_start,
                                suppress_sentence_cap: self.suppress_sentence_cap,
                                in_markdown_code: self.in_markdown_code,
                                profile: self.profile,
                            };
                            if let EvalOutcome::Unchanged {
                                do_not_learn: false,
                                ..
                            } = engine.evaluate_token_internal(&buf, &ctx, self.lotus_composing)
                            {
                                self.flush_pending_unknown(engine, &mut learn_events);
                                self.pending_unknown = Some((self.lang.clone(), buf.clone()));
                            }
                        }
                    }
                    self.update_context_after_commit(engine, &buf, punct, is_space);
                    if is_space && !buf.is_empty() {
                        self.mode = Mode::AfterWord {
                            word: buf,
                            prev_word_before,
                            trailing_spaces: 1,
                        };
                    }
                    return SessionOutput::with_events(learn_events);
                }
                buf.push(ch);
                self.mode = Mode::EditingPrevious {
                    orig_word,
                    prev_word_before,
                    buf,
                };
                return SessionOutput::passthrough();
            }

            // 5. Normal composing word
            Mode::Composing {
                mut buf,
                peak_before_bs,
                origin,
            } => {
                if ch == '.' || ch == ':' {
                    self.mode = Mode::PendingTrailingDot {
                        word: buf,
                        peak_before_bs,
                        dot_char: ch,
                        on_screen_word_chars: None,
                        post_autocorrect: None,
                    };
                    return SessionOutput::passthrough();
                }
                if ch == ';' {
                    self.mode = Mode::PendingSemicolon {
                        word: buf,
                        peak_before_bs,
                        on_screen_word_chars: None,
                        post_autocorrect: None,
                    };
                    return SessionOutput::passthrough();
                }
                if Self::is_immediate_boundary(ch) {
                    return self.finish_token_at_boundary_inner(
                        engine,
                        buf,
                        peak_before_bs,
                        None,
                        ch,
                        0,
                        None,
                        false,
                        now_ms,
                        learn_events,
                    );
                }
                buf.push(ch);
                self.mode = Mode::Composing {
                    buf,
                    peak_before_bs,
                    origin,
                };
                return SessionOutput::passthrough();
            }

            // 6. Immediately after an autocorrect: allow trailing boundaries (e.g. `"fomr, "`) or
            //    peeled post-backspace punctuation/suffix edits (`"reosurce <BS>. "`, `"reosurce <BS>s "`)!
            Mode::JustAutocorrected {
                raw_typed,
                applied,
                prev_word_before,
                mut trailing_boundaries,
                is_snippet,
                corrected_at_ms,
            } => {
                if (ch == ' ' || ch == '\t')
                    && !trailing_boundaries.ends_with(' ')
                    && !trailing_boundaries.ends_with('\t')
                {
                    let was_empty = trailing_boundaries.is_empty();
                    trailing_boundaries.push(ch);
                    if was_empty {
                        self.update_context_after_commit(engine, &applied, None, true);
                    }
                    self.mode = Mode::JustAutocorrected {
                        raw_typed,
                        applied,
                        prev_word_before,
                        trailing_boundaries,
                        is_snippet,
                        corrected_at_ms,
                    };
                    return SessionOutput::passthrough();
                }
                if trailing_boundaries.is_empty() {
                    if ch == '.' || ch == ':' {
                        let word_chars = applied.chars().count();
                        self.mode = Mode::PendingTrailingDot {
                            word: applied,
                            peak_before_bs: None,
                            dot_char: ch,
                            on_screen_word_chars: Some(word_chars),
                            post_autocorrect: Some(PostAutocorrectOrigin {
                                raw_typed,
                                prev_word_before,
                                is_snippet,
                            }),
                        };
                        return SessionOutput::passthrough();
                    }
                    if ch == ';' {
                        let word_chars = applied.chars().count();
                        self.mode = Mode::PendingSemicolon {
                            word: applied,
                            peak_before_bs: None,
                            on_screen_word_chars: Some(word_chars),
                            post_autocorrect: Some(PostAutocorrectOrigin {
                                raw_typed,
                                prev_word_before,
                                is_snippet,
                            }),
                        };
                        return SessionOutput::passthrough();
                    }
                    if Self::is_immediate_boundary(ch) {
                        trailing_boundaries.push(ch);
                        self.update_context_after_commit(engine, &applied, Some(ch), false);
                        self.mode = Mode::JustAutocorrected {
                            raw_typed,
                            applied,
                            prev_word_before,
                            trailing_boundaries,
                            is_snippet,
                            corrected_at_ms,
                        };
                        return SessionOutput::passthrough();
                    }
                    if !Self::is_opening_delimiter_when_idle(ch) {
                        self.prev_word = prev_word_before;
                        let mut buf = applied;
                        buf.push(ch);
                        self.mode = Mode::Composing {
                            buf,
                            peak_before_bs: None,
                            origin: InputOrigin::PlainAscii,
                        };
                        return SessionOutput::passthrough();
                    }
                }
                // User moved on to new characters -> start normal handling from Idle
            }

            // 7. Immediately after an untouched word + `' '`
            Mode::AfterWord {
                word,
                prev_word_before,
                trailing_spaces,
            } => {
                if ch == ' ' || ch == '\t' {
                    self.mode = Mode::AfterWord {
                        word,
                        prev_word_before,
                        trailing_spaces: trailing_spaces + 1,
                    };
                    return SessionOutput::passthrough();
                }
                // User started typing the next word without backspacing `word` -> confirm untouched unknown!
                self.flush_pending_unknown(engine, &mut learn_events);
            }

            Mode::Idle => {}
        }

        // Starting from `Idle` (or transitioning out of `JustAutocorrected` / `AfterWord`):
        // Leading `;` and `:` are allowed to start `Composing` so `;addr` / `:sig` snippets work
        // while non-snippets (`:wq`, `;)`) safely hit `TokenAnalysis::Bypass`.
        if Self::is_immediate_boundary(ch) || Self::is_opening_delimiter_when_idle(ch) || ch == '.'
        {
            if Self::ends_sentence(ch) {
                self.prev_word = None;
                self.at_sentence_start = true;
                self.suppress_sentence_cap = false;
            } else if ch != ' ' && ch != '\t' && !Self::is_opening_delimiter_when_idle(ch) {
                self.prev_word = None;
            }
            return SessionOutput::with_events(learn_events);
        }

        // User began typing a new word without backspacing any prior punctuation-terminated unknown word
        self.flush_pending_unknown(engine, &mut learn_events);

        let mut buf = String::with_capacity(8);
        buf.push(ch);
        self.mode = Mode::Composing {
            buf,
            peak_before_bs: None,
            origin: InputOrigin::PlainAscii,
        };
        SessionOutput::with_events(learn_events)
    }

    fn maybe_record_vi_passthrough_repair(
        &mut self,
        vi_dict: &[Box<str>],
        store: &mut RuleStore,
        orig_word: &str,
        new_buf: &str,
        events: &mut Vec<LearnEvent>,
    ) {
        if self.lang.as_str() != "vi" || !crate::vi_telex::has_any_vietnamese_rune(orig_word) {
            return;
        }
        let opts = store.lotus_options();
        let buf_sim = crate::vi_telex::simulate_lotus_word_with_dict(new_buf, opts, vi_dict);
        let cand_ascii =
            if !buf_sim.is_composed_vietnamese && buf_sim.preedit_before_break.is_ascii() {
                buf_sim.preedit_before_break.as_str()
            } else if new_buf.is_ascii() {
                new_buf
            } else {
                return;
            };
        if cand_ascii.len() < 2 {
            return;
        }
        let re_sim = crate::vi_telex::simulate_lotus_word_with_dict(cand_ascii, opts, vi_dict);
        if re_sim.committed_at_break.eq_ignore_ascii_case(orig_word) {
            let ev = record_telex_passthrough_repair(store, cand_ascii);
            if !matches!(ev, LearnEvent::None) {
                events.push(ev);
            }
        }
    }

    /// Evaluates a buffered word immediately after `fcitx5-lotus` filters a boundary key
    /// (`Space`, `,`, `.`, `!`, `?`, `;`, `:`) and commits the restored or composed word via
    /// `EventType::InputContextCommitString`.
    ///
    /// Verifies `committed_text` is a suffix of `sim.committed_at_break + boundary_ch` so Lotus
    /// macro expansions (`ko -> "không "`) and fast buffered-key batches (`"uowcss a"`) are never
    /// touched.
    pub fn on_lotus_boundary_commit(
        &mut self,
        engine: &mut SmartKeyEngine,
        boundary_ch: char,
        committed_text: &str,
        now_ms: u64,
    ) -> SessionOutput {
        if !engine.store.compiled().enabled
            || self.profile == AppProfile::Bypass
            || !engine.store.compiled().vi_lotus_enabled
        {
            self.reset_state_only();
            return SessionOutput::passthrough();
        }
        self.last_key_ms = now_ms;
        self.lotus_composing = true;
        engine.store.set_active_lang(self.lang.clone());
        engine.store.set_current_app(self.app.as_deref());

        if committed_text.is_empty() || !committed_text.ends_with(boundary_ch) {
            return self.process_key(engine, KeyInput::NavOrClick, now_ms);
        }

        let mut learn_events = Vec::new();
        let mode = std::mem::replace(&mut self.mode, Mode::Idle);
        let (word, peak_before_bs, prior_punct, extra_buffered) = match mode {
            Mode::Composing {
                buf,
                peak_before_bs,
                ..
            } => (buf, peak_before_bs, None, 0usize),
            Mode::PendingTrailingDot {
                word,
                dot_char,
                post_autocorrect: Some(origin),
                ..
            } if Self::is_immediate_boundary(boundary_ch) => {
                return self.finish_post_autocorrect_pending_punct(
                    engine, word, dot_char, origin, boundary_ch, now_ms,
                );
            }
            Mode::PendingSemicolon {
                word,
                post_autocorrect: Some(origin),
                ..
            } if Self::is_immediate_boundary(boundary_ch) => {
                return self.finish_post_autocorrect_pending_punct(
                    engine, word, ';', origin, boundary_ch, now_ms,
                );
            }
            Mode::PendingTrailingDot {
                word,
                peak_before_bs,
                dot_char,
                post_autocorrect: None,
                ..
            } if Self::is_immediate_boundary(boundary_ch) => {
                (word, peak_before_bs, Some(dot_char), 1usize)
            }
            Mode::PendingSemicolon {
                word,
                peak_before_bs,
                post_autocorrect: None,
                ..
            } if Self::is_immediate_boundary(boundary_ch) => {
                (word, peak_before_bs, Some(';'), 1usize)
            }
            other => {
                self.mode = other;
                return self.process_key(engine, KeyInput::NavOrClick, now_ms);
            }
        };

        let opts = engine.store.lotus_options();
        let sim = engine.simulate_vi_lotus(&word, opts);
        let mut expected_commit = String::with_capacity(sim.committed_at_break.len() + 4);
        expected_commit.push_str(&sim.committed_at_break);
        expected_commit.push(boundary_ch);

        // If `committed_text` is not a suffix of `sim.committed_at_break + boundary_ch`, Lotus
        // expanded a user macro (`ko -> "không "`) or custom rule: update context and step aside!
        if !expected_commit.ends_with(committed_text) {
            self.flush_pending_unknown(engine, &mut learn_events);
            let is_space = boundary_ch == ' ' || boundary_ch == '\t';
            let punct = prior_punct.or(if is_space { None } else { Some(boundary_ch) });
            let macro_body = committed_text
                .strip_suffix(boundary_ch)
                .unwrap_or(committed_text);
            self.update_context_after_commit(
                engine,
                macro_body,
                punct,
                is_space && prior_punct.is_none(),
            );
            return SessionOutput::with_events(learn_events);
        }

        let committed_word_chars = sim.committed_at_break.chars().count();
        if prior_punct.is_none() && (boundary_ch == '.' || boundary_ch == ':') {
            self.mode = Mode::PendingTrailingDot {
                word,
                peak_before_bs,
                dot_char: boundary_ch,
                on_screen_word_chars: Some(committed_word_chars),
                post_autocorrect: None,
            };
            return SessionOutput::passthrough();
        }
        if prior_punct.is_none() && boundary_ch == ';' {
            self.mode = Mode::PendingSemicolon {
                word,
                peak_before_bs,
                on_screen_word_chars: Some(committed_word_chars),
                post_autocorrect: None,
            };
            return SessionOutput::passthrough();
        }
        if !Self::is_immediate_boundary(boundary_ch) {
            return SessionOutput::with_events(learn_events);
        }

        self.finish_token_at_boundary_inner(
            engine,
            word,
            peak_before_bs,
            prior_punct,
            boundary_ch,
            extra_buffered,
            Some(committed_word_chars),
            true,
            now_ms,
            learn_events,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_token_at_boundary_inner(
        &mut self,
        engine: &mut SmartKeyEngine,
        word: String,
        peak_before_bs: Option<String>,
        prior_punct: Option<char>,
        trigger_ch: char,
        extra_buffered_chars: usize,
        on_screen_word_override: Option<usize>,
        boundary_already_committed: bool,
        now_ms: u64,
        mut learn_events: Vec<LearnEvent>,
    ) -> SessionOutput {
        self.flush_pending_unknown(engine, &mut learn_events);
        let is_trigger_space = trigger_ch == ' ' || trigger_ch == '\t';
        let effective_punct = prior_punct.or(if is_trigger_space {
            None
        } else {
            Some(trigger_ch)
        });
        let prev_before = self.prev_word.clone();

        // Pre-Space mid-word manual fix (`"cna" -> BS -> "can "`):
        // The user ALREADY manually edited this word mid-flight; learn from `(peak -> word)` and
        // do not overwrite their fresh manual edit with statistical guesses!
        if let Some(peak) = peak_before_bs {
            let check_unknown = if self.lang.as_str() != "vi" {
                let lex = engine.registry.lexicon(self.lang.as_str());
                let ev = record_manual_edit(
                    &mut engine.store,
                    lex,
                    &self.lang,
                    prev_before.as_deref(),
                    &peak,
                    &word,
                );
                let is_unk = matches!(
                    ev,
                    LearnEvent::Ignored(
                        crate::learner::SkipReason::TargetNotValidWord
                            | crate::learner::SkipReason::SameWord
                    )
                );
                if !matches!(ev, LearnEvent::None) {
                    learn_events.push(ev);
                }
                is_unk
            } else {
                true
            };
            if check_unknown && !self.in_markdown_code {
                let ctx = EvalContext {
                    prev_word: prev_before.as_deref(),
                    boundary_punct: effective_punct,
                    at_sentence_start: self.at_sentence_start,
                    suppress_sentence_cap: self.suppress_sentence_cap,
                    in_markdown_code: self.in_markdown_code,
                    profile: self.profile,
                };
                if let EvalOutcome::Unchanged {
                    do_not_learn: false,
                    ..
                } = engine.evaluate_token_internal(&word, &ctx, self.lotus_composing)
                {
                    self.pending_unknown = Some((self.lang.clone(), word.clone()));
                }
            }
            self.update_context_after_commit(
                engine,
                &word,
                effective_punct,
                is_trigger_space && prior_punct.is_none(),
            );
            if is_trigger_space && prior_punct.is_none() {
                self.mode = Mode::AfterWord {
                    word,
                    prev_word_before: prev_before,
                    trailing_spaces: 1,
                };
            }
            return SessionOutput::with_events(learn_events);
        }

        let ctx = EvalContext {
            prev_word: prev_before.as_deref(),
            boundary_punct: effective_punct,
            at_sentence_start: self.at_sentence_start,
            suppress_sentence_cap: self.suppress_sentence_cap,
            in_markdown_code: self.in_markdown_code,
            profile: self.profile,
        };

        let is_vi_lotus = self.lang.as_str() == "vi" && engine.store.compiled().vi_lotus_enabled;
        let lotus_opts = engine.store.lotus_options();
        let vi_sim = if is_vi_lotus && self.lotus_composing {
            let sim = engine.simulate_vi_lotus(&word, lotus_opts);
            // If Lotus itself is performing a uinput fallback burst on an unfiltered test path,
            // step aside until `on_lotus_boundary_commit` delivers the completed commit.
            if sim.needs_uinput_restore_on_break && !boundary_already_committed {
                self.update_context_after_commit(
                    engine,
                    &sim.committed_at_break,
                    effective_punct,
                    is_trigger_space && prior_punct.is_none(),
                );
                return SessionOutput::with_events(learn_events);
            }
            let composed_lower = sim.committed_at_break.to_lowercase();
            // If Lotus already composed a valid Vietnamese word with Vietnamese runes (e.g. `"tiếng"`),
            // leave it untouched unless `telex_passthrough` or the English-Sentence Momentum Shield applies.
            if sim.is_valid_vietnamese
                && sim.is_composed_vietnamese
                && !engine.should_passthrough_telex(&word, &ctx, &composed_lower)
            {
                self.update_context_after_commit(
                    engine,
                    &sim.committed_at_break,
                    effective_punct,
                    is_trigger_space && prior_punct.is_none(),
                );
                if is_trigger_space && prior_punct.is_none() {
                    self.mode = Mode::AfterWord {
                        word: sim.committed_at_break,
                        prev_word_before: prev_before,
                        trailing_spaces: 1,
                    };
                }
                return SessionOutput::with_events(learn_events);
            }
            Some(sim)
        } else {
            None
        };

        let outcome = engine.evaluate_token_internal(&word, &ctx, self.lotus_composing);
        match outcome {
            EvalOutcome::Autocorrect {
                replacement, kind, ..
            } => {
                let mut trailing_boundaries = String::with_capacity(2);
                if let Some(p) = prior_punct {
                    trailing_boundaries.push(p);
                }
                trailing_boundaries.push(trigger_ch);

                let mut insert =
                    String::with_capacity(replacement.len() + trailing_boundaries.len());
                insert.push_str(&replacement);
                insert.push_str(&trailing_boundaries);

                if boundary_already_committed {
                    let on_screen_word = vi_sim
                        .as_ref()
                        .map(|s| s.committed_at_break.as_str())
                        .unwrap_or(word.as_str());
                    if replacement == on_screen_word {
                        self.update_context_after_commit(
                            engine,
                            &replacement,
                            effective_punct,
                            is_trigger_space && prior_punct.is_none(),
                        );
                        return SessionOutput::with_events(learn_events);
                    }
                }

                engine.store.log_autocorrect_applied_with_context(
                    prev_before.as_deref(),
                    &word,
                    &replacement,
                    kind,
                );

                let base_chars = on_screen_word_override.unwrap_or_else(|| {
                    vi_sim
                        .as_ref()
                        .map(|s| s.screen_chars)
                        .unwrap_or_else(|| word.chars().count())
                });
                let delete_chars =
                    base_chars + extra_buffered_chars + usize::from(boundary_already_committed);

                // For a `telex_passthrough` / Momentum-Shield restore (`"of"` replacing `"ò"`),
                // `1x BackSpace` should revert back to Lotus's Vietnamese rune (`"ò"`).
                let undo_raw_typed = match &vi_sim {
                    Some(s) if s.is_composed_vietnamese && replacement == word => {
                        s.committed_at_break.clone()
                    }
                    _ => word,
                };

                self.update_context_after_commit(
                    engine,
                    &replacement,
                    effective_punct,
                    is_trigger_space && prior_punct.is_none(),
                );
                self.mode = Mode::JustAutocorrected {
                    raw_typed: undo_raw_typed,
                    applied: replacement,
                    prev_word_before: prev_before,
                    trailing_boundaries,
                    is_snippet: kind == MatchKind::UserSnippet,
                    corrected_at_ms: now_ms,
                };

                SessionOutput {
                    action: SessionAction::Replace {
                        delete_chars,
                        insert,
                    },
                    matched_kind: Some(kind),
                    learn_events,
                }
            }
            EvalOutcome::Unchanged {
                reason,
                do_not_learn,
            } => {
                // Detect deliberate Telex double-key undo escape (`"aurr "` -> `"aur"` on screen
                // where `"aur"` alone composes `"ảu"`): 2x graduates `"aur"` to `telex_passthrough`!
                if let Some(sim) = &vi_sim {
                    if word.len() >= 3
                        && word.is_ascii()
                        && !sim.is_composed_vietnamese
                        && sim.preedit_before_break.is_ascii()
                        && sim.preedit_before_break.len() + 1 == word.len()
                    {
                        let base = &sim.preedit_before_break;
                        let base_sim = engine.simulate_vi_lotus(base, lotus_opts);
                        if base_sim.is_valid_vietnamese && base_sim.is_composed_vietnamese {
                            let pt_ev = record_telex_passthrough_repair(&mut engine.store, base);
                            if !matches!(pt_ev, LearnEvent::None) {
                                learn_events.push(pt_ev);
                            }
                        }
                    }
                }

                if reason == MatchKind::ValidWord {
                    let core_lower = word
                        .trim_matches(|c: char| !c.is_alphabetic() && c != '\'' && c != '’')
                        .to_lowercase();
                    if !core_lower.is_empty() {
                        engine
                            .store
                            .record_personal_word_use(&self.lang, &core_lower);
                    }
                }

                if !do_not_learn {
                    self.pending_unknown = Some((self.lang.clone(), word.clone()));
                    if reason != MatchKind::ValidWord {
                        let (best_candidate, best_score, threshold) =
                            if let Some(scorer) = engine.scorer_for(self.lang.as_str()) {
                                let explanation = scorer.explain(
                                    &word,
                                    &ctx,
                                    engine.store.scorer_config(),
                                    &engine.store,
                                );
                                let (thresh, _) = crate::scorer::LatinScorer::thresholds(
                                    engine.store.scorer_config().aggressiveness,
                                );
                                let (cand, score) = match explanation.top_candidates.first() {
                                    Some(c) => (Some(c.replacement.clone()), c.score),
                                    None => (None, 0.0),
                                };
                                (cand, score, thresh)
                            } else {
                                (None, 0.0, 0.14)
                            };
                        engine.store.log_unfixed_unknown(
                            &word,
                            best_candidate.as_deref(),
                            best_score,
                            threshold,
                        );
                    }
                } else {
                    if reason == MatchKind::ValidWord && engine.store.diagnostics().event_log {
                        if let (Some(prev), Some(scorer)) = (
                            prev_before.as_deref(),
                            engine.scorer_for(self.lang.as_str()),
                        ) {
                            if scorer.lexicon().header().tier == PackTier::Data {
                                let alt = scorer.find_real_word_context_alternative(
                                    &word,
                                    prev,
                                    engine.store.scorer_config(),
                                    &engine.store,
                                );
                                if let Some((cand, cand_bg, typed_bg)) = alt {
                                    engine.store.log_real_word_declined(
                                        &word, prev, &cand, cand_bg, typed_bg,
                                    );
                                }
                            }
                        }
                    }
                    if engine.store.diagnostics().event_log
                        && engine.store.diagnostics().log_level
                            == crate::event_log::EventLogLevel::All
                    {
                        engine.store.log_word_unchanged(&word, reason);
                    }
                }
                let on_screen_final = vi_sim
                    .as_ref()
                    .map(|s| s.committed_at_break.clone())
                    .unwrap_or(word);
                self.update_context_after_commit(
                    engine,
                    &on_screen_final,
                    effective_punct,
                    is_trigger_space && prior_punct.is_none(),
                );
                if is_trigger_space && prior_punct.is_none() {
                    self.mode = Mode::AfterWord {
                        word: on_screen_final,
                        prev_word_before: prev_before,
                        trailing_spaces: 1,
                    };
                }
                SessionOutput::with_events(learn_events)
            }
        }
    }

    // =========================================================================
    // Portable Word-Level Learner API (for Android HeliBoard JNI & `fcitx5-lotus` Phase 2)
    // =========================================================================

    /// Filters blocked pairs, shields `personal_dict` words, and applies user rule / `trouble_words`
    /// boosts onto an external candidate list (such as HeliBoard's `SuggestedWords`).
    pub fn filter_and_rerank_candidates(
        &self,
        engine: &SmartKeyEngine,
        typed: &str,
        candidates: &mut Vec<Candidate>,
    ) {
        let lower = typed.to_lowercase();
        candidates.retain(|c| {
            !engine
                .store
                .is_pair_blocked(&lower, &c.replacement.to_lowercase())
        });

        if let Some(prev) = self.prev_word.as_deref() {
            if let Some(ctx_target) = engine
                .store
                .lookup_context_rule(&prev.to_lowercase(), &lower)
            {
                candidates.insert(
                    0,
                    Candidate {
                        replacement: ctx_target.to_string(),
                        score: 9.0,
                        edit_cost: 0.0,
                        freq: 255,
                        bigram_boost: 1.0,
                        kind: MatchKind::UserContextRule,
                    },
                );
            }
        }

        if let Some(corr) = engine.store.lookup_correction(&lower) {
            candidates.insert(
                0,
                Candidate {
                    replacement: corr.to_string(),
                    score: 8.5,
                    edit_cost: 0.0,
                    freq: 255,
                    bigram_boost: 0.0,
                    kind: MatchKind::UserCorrectionRule,
                },
            );
        }

        for cand in candidates.iter_mut() {
            cand.score += engine
                .store
                .trouble_word_boost(&cand.replacement.to_lowercase());
        }

        if engine.store.is_personal_word(&lower) {
            candidates.insert(
                0,
                Candidate {
                    replacement: typed.to_string(),
                    score: 100.0,
                    edit_cost: 0.0,
                    freq: 255,
                    bigram_boost: 0.0,
                    kind: MatchKind::ValidWord,
                },
            );
        }
    }

    /// HeliBoard hook: called when the mobile keyboard auto-commits `applied` in place of `raw_typed`.
    pub fn on_mobile_auto_commit(&mut self, raw_typed: &str, applied: &str) {
        let prev = self.prev_word.clone();
        self.mobile_pending_undo = Some((raw_typed.to_string(), applied.to_string(), prev));
        self.prev_word = Some(applied.to_string());
    }

    /// HeliBoard hook: called when `revertCommit` fires on `1x BackSpace`.
    /// Immediately applies the `1x` session shield if the user keeps the reverted word, or awaits edit resolution.
    pub fn on_mobile_undo_keep(
        &mut self,
        engine: &mut SmartKeyEngine,
        raw_typed: &str,
        rejected: &str,
    ) -> LearnEvent {
        engine.store.set_active_lang(self.lang.clone());
        self.mobile_pending_undo = None;
        self.prev_word = Some(raw_typed.to_string());
        engine
            .store
            .log_backspace_undo(raw_typed, rejected, UndoResolution::KeptVerbatim);
        let lex = engine.registry.lexicon(self.lang.as_str());
        record_undo_keep_verbatim(&mut engine.store, lex, &self.lang, raw_typed, rejected)
    }

    /// HeliBoard hook: called when a user reverts an auto-commit and edits/commits `final_word`,
    /// OR manually edits `orig -> fixed` / taps a suggestion chip.
    pub fn on_mobile_manual_fix(
        &mut self,
        engine: &mut SmartKeyEngine,
        orig: &str,
        fixed: &str,
    ) -> Vec<LearnEvent> {
        engine.store.set_active_lang(self.lang.clone());
        let lex = engine.registry.lexicon(self.lang.as_str());
        if let Some((raw_typed, rejected, prev_before)) = self.mobile_pending_undo.take() {
            if raw_typed.eq_ignore_ascii_case(orig) {
                self.prev_word = Some(fixed.to_string());
                return resolve_restored_edit(
                    &mut engine.store,
                    lex,
                    &self.lang,
                    prev_before.as_deref(),
                    &raw_typed,
                    &rejected,
                    fixed,
                );
            }
        }
        let prev = self.prev_word.clone();
        self.prev_word = Some(fixed.to_string());
        vec![record_manual_edit(
            &mut engine.store,
            lex,
            &self.lang,
            prev.as_deref(),
            orig,
            fixed,
        )]
    }

    /// HeliBoard hook: called when a word is committed untouched (stages `3x -> personal_dict` if unknown).
    pub fn on_mobile_untouched_commit(
        &mut self,
        engine: &mut SmartKeyEngine,
        word: &str,
    ) -> LearnEvent {
        engine.store.set_active_lang(self.lang.clone());
        self.mobile_pending_undo = None;
        let ev = engine.record_untouched_unknown_token(&self.lang, word);
        self.prev_word = Some(word.to_string());
        ev
    }

    /// Phase 2 (`fcitx5-lotus`) hook: records a Telex-to-ASCII repair (`2x -> telex_passthrough`).
    pub fn on_lotus_passthrough_repair(
        &mut self,
        engine: &mut SmartKeyEngine,
        raw_ascii: &str,
    ) -> LearnEvent {
        record_telex_passthrough_repair(&mut engine.store, raw_ascii)
    }
}

/// Pure helper that simulates client application buffer updates for REPLs and test harnesses.
/// Applies `PassThrough` and `Replace { delete_chars, insert }` onto `client_buf` using Unicode scalar chars.
pub fn apply_key_to_buffer(client_buf: &mut String, key: KeyInput, action: &SessionAction) {
    match action {
        SessionAction::Replace {
            delete_chars,
            insert,
        } => {
            for _ in 0..*delete_chars {
                client_buf.pop();
            }
            client_buf.push_str(insert);
        }
        SessionAction::PassThrough => match key {
            KeyInput::Char(ch) => client_buf.push(ch),
            KeyInput::BackSpace => {
                client_buf.pop();
            }
            KeyInput::CtrlBackSpace => {
                // Standard editor word-delete backward: trim trailing spaces, then non-spaces
                while client_buf.ends_with(' ') || client_buf.ends_with('\t') {
                    client_buf.pop();
                }
                while let Some(last) = client_buf.chars().next_back() {
                    if last == ' ' || last == '\t' || last == '\n' {
                        break;
                    }
                    client_buf.pop();
                }
            }
            KeyInput::Enter => client_buf.push('\n'),
            KeyInput::Tab => client_buf.push('\t'),
            KeyInput::NavOrClick | KeyInput::FocusLost => {}
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use fst::MapBuilder;

    use super::*;
    use crate::event_log::EventLog;
    use crate::lexicon::{
        serialize_skdict_v3, CliticRules, LanguageRules, PackHeader, PackTier, SkdictV3Payload,
        WordMeta,
    };
    use crate::store::{DiagnosticEvent, Preset};

    fn unique_tmp_dir(tag: &str) -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "smartkey_session_test_{tag}_{}_{}",
            std::process::id(),
            id
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn build_synthetic_v3_bytes(
        lang: &str,
        version: [u8; 3],
        words: &[(&str, WordMeta)],
        shorthand: &[(&str, &str)],
        rules: LanguageRules,
    ) -> Vec<u8> {
        let mut sorted_words = words.to_vec();
        sorted_words.sort_by(|a, b| a.0.cmp(b.0));
        let mut wb = MapBuilder::memory();
        for (w, meta) in sorted_words {
            wb.insert(w.as_bytes(), meta.pack()).unwrap();
        }
        let mut sorted_short: Vec<(String, String)> = shorthand
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        sorted_short.sort_by(|a, b| a.0.cmp(&b.0));

        let header = PackHeader {
            lang: lang.to_string(),
            pack_version: version,
            tier: PackTier::Data,
            variants: vec![format!("{lang}_{}", lang.to_uppercase())],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };
        serialize_skdict_v3(&SkdictV3Payload {
            header: &header,
            words_fst: &wb.into_inner().unwrap(),
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &[],
            shorthand: &sorted_short,
            slang_set: &[],
            rules: Some(&rules),
            syllable_dict: &[],
            source_manifest: &[],
        })
    }

    fn feed_text(session: &mut SmartKeySession, engine: &mut SmartKeyEngine, text: &str) -> String {
        let mut buf = String::new();
        for (idx, ch) in text.chars().enumerate() {
            let out = session.process_key(engine, KeyInput::Char(ch), (idx as u64 + 1) * 10);
            apply_key_to_buffer(&mut buf, KeyInput::Char(ch), &out.action);
        }
        buf
    }

    #[test]
    fn lexicon_registry_ignores_legacy_skdict02_emits_one_time_pack_notice_and_falls_back() {
        let dict_dir = unique_tmp_dir("legacy_reg");
        let log_dir = unique_tmp_dir("legacy_log");

        std::fs::write(dict_dir.join("en.skdict"), b"SKDICT02legacy_payload").unwrap();
        std::fs::write(dict_dir.join("fr.skdict"), b"NOT_A_PACK").unwrap();

        let store = RuleStore::in_memory(Preset::Balanced);
        let mut engine =
            SmartKeyEngine::new_from_dirs(std::slice::from_ref(&dict_dir), true, store);

        // Fallback to bundled-en succeeds; invalid fr pack is ignored.
        let en = engine.lexicon().expect("bundled en fallback");
        assert!(en.contains_valid("the", crate::types::Dialect::Mixed));
        assert!(engine.lexicon_for("fr").is_none());

        // PackNotice must NEVER pollute learner_state.json::recent_events.
        assert!(engine.store().state().recent_events.is_empty());

        // Emitting again with the same notice/detail deduplicates (one-time PackNotice).
        let (_, dup_notices) = LexiconRegistry::scan_dirs(std::slice::from_ref(&dict_dir), true);
        engine.emit_notices(dup_notices);

        // Simulates Fcitx5 startup order: set_log_dir arrives after SmartKeyEngine::new_auto.
        engine.store_mut().set_log_dir(Some(log_dir.clone()));

        let records = EventLog::read_records_in_range(&log_dir, None, None).unwrap();
        let legacy_count = records
            .iter()
            .filter(|r| {
                matches!(
                    &r.event,
                    DiagnosticEvent::PackNotice { notice, detail }
                        if notice == "legacy_pack_ignored" && detail.contains("skdict v2")
                )
            })
            .count();
        let invalid_count = records
            .iter()
            .filter(|r| {
                matches!(
                    &r.event,
                    DiagnosticEvent::PackNotice { notice, .. } if notice == "invalid_pack_ignored"
                )
            })
            .count();
        assert_eq!(legacy_count, 1, "records={records:?}");
        assert_eq!(invalid_count, 1, "records={records:?}");

        let _ = std::fs::remove_dir_all(&dict_dir);
        let _ = std::fs::remove_dir_all(&log_dir);
    }

    #[test]
    fn lexicon_registry_user_dir_overrides_system_dir() {
        let user_dir = unique_tmp_dir("user_prio");
        let sys_dir = unique_tmp_dir("sys_prio");

        let user_es = build_synthetic_v3_bytes(
            "es",
            [0, 3, 2],
            &[(
                "hola",
                WordMeta {
                    freq0: 210,
                    freq1: 0,
                    variant_mask: 0x01,
                    flags: 0,
                    word_id: 1,
                },
            )],
            &[],
            LanguageRules::default(),
        );
        let sys_es = build_synthetic_v3_bytes(
            "es",
            [0, 3, 0],
            &[(
                "adios",
                WordMeta {
                    freq0: 200,
                    freq1: 0,
                    variant_mask: 0x01,
                    flags: 0,
                    word_id: 1,
                },
            )],
            &[],
            LanguageRules::default(),
        );
        std::fs::write(user_dir.join("es.skdict"), user_es).unwrap();
        std::fs::write(sys_dir.join("es.skdict"), sys_es).unwrap();

        let (reg, notices) =
            LexiconRegistry::scan_dirs(&[user_dir.clone(), sys_dir.clone()], false);
        assert!(notices.is_empty());
        let es = reg.lexicon("es").expect("es pack loaded");
        assert_eq!(es.header().pack_version, [0, 3, 2]);
        assert!(es.contains_valid("hola", crate::types::Dialect::Mixed));
        assert!(!es.contains_valid("adios", crate::types::Dialect::Mixed));

        let _ = std::fs::remove_dir_all(&user_dir);
        let _ = std::fs::remove_dir_all(&sys_dir);
    }

    #[test]
    fn multilang_session_routes_es_and_fr_packs_and_guards_learner() {
        let es_lex = PackLexicon::from_owned_bytes(build_synthetic_v3_bytes(
            "es",
            [0, 3, 0],
            &[
                (
                    "hola",
                    WordMeta {
                        freq0: 210,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 1,
                    },
                ),
                (
                    "porque",
                    WordMeta {
                        freq0: 215,
                        freq1: 0,
                        variant_mask: 0x01,
                        flags: 0,
                        word_id: 2,
                    },
                ),
            ],
            &[("xq", "porque")],
            LanguageRules {
                capitalize_standalone_i: false,
                ..LanguageRules::default()
            },
        ))
        .unwrap();

        let fr_lex = PackLexicon::from_owned_bytes(build_synthetic_v3_bytes(
            "fr",
            [0, 3, 0],
            &[(
                "homme",
                WordMeta {
                    freq0: 220,
                    freq1: 0,
                    variant_mask: 0x01,
                    flags: 0,
                    word_id: 1,
                },
            )],
            &[],
            LanguageRules {
                clitics: CliticRules {
                    suffixes: Vec::new(),
                    prefixes: vec!["l".to_string()],
                },
                ..LanguageRules::default()
            },
        ))
        .unwrap();

        let mut reg = LexiconRegistry::new();
        reg.insert("es", LatinScorer::new(es_lex));
        reg.insert("fr", LatinScorer::new(fr_lex));

        let mut engine = SmartKeyEngine::from_registry(reg, RuleStore::in_memory(Preset::Balanced));
        let mut session = SmartKeySession::new();
        session.set_lang(LangId("es".to_string()));

        // Spanish shorthand expansion + capitalize_standalone_i=false routing:
        assert_eq!(feed_text(&mut session, &mut engine, "xq i "), "porque i ");

        // `explain_token` inspects active_lang ("es"):
        engine.store_mut().set_active_lang(LangId("es".to_string()));
        let exp = engine.explain_token("xq", &EvalContext::default());
        assert!(matches!(
            exp.outcome,
            EvalOutcome::Autocorrect {
                ref replacement,
                kind: MatchKind::ShorthandExpansion,
                ..
            } if replacement == "porque"
        ));

        // Valid Spanish word untouched 3x must NOT graduate into `[lang.es].personal_dict`:
        for _ in 0..3 {
            let ev = session.on_mobile_untouched_commit(&mut engine, "hola");
            assert_eq!(ev, LearnEvent::None);
        }

        // Non-ASCII accented edit in "es" (`cafe -> café`) MUST stay in `es`, never hijack to `vi`:
        session.on_mobile_manual_fix(&mut engine, "cafe", "café");
        let second = session.on_mobile_manual_fix(&mut engine, "cafe", "café");
        assert!(
            matches!(
                second.as_slice(),
                [LearnEvent::GraduatedCorrection { lang, orig, fixed }]
                    if lang == "es" && orig == "cafe" && fixed == "café"
            ),
            "got {second:?}"
        );

        // Switching to "fr" activates French clitic prefix elision shielding:
        session.set_lang(LangId("fr".to_string()));
        assert_eq!(feed_text(&mut session, &mut engine, "l'homme "), "l'homme ");
    }

    #[test]
    fn installed_vi_skdict_overrides_bundled_vi_and_legacy_vi_falls_back() {
        let dict_dir = unique_tmp_dir("vi_override");
        let header = PackHeader {
            lang: "vi".to_string(),
            pack_version: [0, 9, 0],
            tier: PackTier::ImeBridge,
            variants: vec!["vi".to_string()],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };
        let bytes = serialize_skdict_v3(&SkdictV3Payload {
            header: &header,
            words_fst: &[],
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &[],
            shorthand: &[("zzz".to_string(), "dạ vâng".to_string())],
            slang_set: &["customvi".to_string()],
            rules: None,
            syllable_dict: &["tiếng".to_string()],
            source_manifest: &[],
        });
        std::fs::write(dict_dir.join("vi.skdict"), bytes).unwrap();

        let engine = SmartKeyEngine::new_from_dirs(
            std::slice::from_ref(&dict_dir),
            true,
            RuleStore::in_memory(Preset::Balanced),
        );
        assert!(
            engine.scorer_for("vi").is_none(),
            "ImeBridge pack must not expose LatinScorer"
        );
        assert_eq!(engine.lookup_vi_shorthand("zzz"), Some("dạ vâng"));
        assert!(engine.is_vi_slang_word("customvi"));
        assert_eq!(engine.lookup_vi_shorthand("ko"), None);

        // Legacy v2 vi.skdict is ignored with PackNotice and falls back to bundled-vi.
        let legacy_dir = unique_tmp_dir("vi_legacy");
        let log_dir = unique_tmp_dir("vi_legacy_log");
        std::fs::write(legacy_dir.join("vi.skdict"), b"SKDICT02oldvi").unwrap();
        let mut store = RuleStore::in_memory(Preset::Balanced);
        store.set_log_dir(Some(log_dir.clone()));
        let legacy_eng =
            SmartKeyEngine::new_from_dirs(std::slice::from_ref(&legacy_dir), true, store);
        assert_eq!(legacy_eng.lookup_vi_shorthand("ko"), Some("không"));
        let records = EventLog::read_records_in_range(&log_dir, None, None).unwrap();
        assert!(records.iter().any(|r| matches!(
            &r.event,
            DiagnosticEvent::PackNotice { notice, detail }
                if notice == "legacy_pack_ignored" && detail.contains("skdict v2")
        )));

        let _ = std::fs::remove_dir_all(&dict_dir);
        let _ = std::fs::remove_dir_all(&legacy_dir);
        let _ = std::fs::remove_dir_all(&log_dir);
    }
}
