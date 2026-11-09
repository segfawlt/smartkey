use serde::{Deserialize, Serialize};

use crate::casing::is_expressive_stretch;
use crate::keyboard::damerau_levenshtein_chars;
use crate::lexicon::EnglishLexicon;
use crate::scorer::UserRuleView;
use crate::store::{RuleStore, UndoResolution};
use crate::types::LangId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    SameWord,
    InvalidToken,
    TargetNotValidWord,
    DeltaSpanRewrite,
    MindChangeDistance,
    ValidWordNoContext,
    WeakContextAsymmetry,
}

impl SkipReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SameWord => "same_word",
            Self::InvalidToken => "invalid_token",
            Self::TargetNotValidWord => "target_not_valid_word",
            Self::DeltaSpanRewrite => "delta_span_rewrite",
            Self::MindChangeDistance => "mind_change_distance",
            Self::ValidWordNoContext => "valid_word_no_context",
            Self::WeakContextAsymmetry => "weak_context_asymmetry",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LearnEvent {
    None,
    SelfConfirmCancelled,
    ShieldedWordSession {
        word: String,
    },
    GraduatedUndoKeep {
        typed: String,
        rejected: String,
    },
    BlockedPairPermanent {
        typed: String,
        rejected: String,
    },
    StagedCorrection {
        lang: String,
        orig: String,
        fixed: String,
        count: u32,
    },
    GraduatedCorrection {
        lang: String,
        orig: String,
        fixed: String,
    },
    StagedContextCorrection {
        prev: String,
        orig: String,
        fixed: String,
        count: u32,
    },
    GraduatedContextCorrection {
        prev: String,
        orig: String,
        fixed: String,
    },
    StagedUntouchedUnknown {
        lang: String,
        word: String,
        count: u32,
    },
    GraduatedPersonalWord {
        lang: String,
        word: String,
    },
    GraduatedTelexPassthrough {
        raw_ascii: String,
    },
    Ignored(SkipReason),
}

/// Unicode-safe multiset overlap (`0.0..=1.0`) over arbitrary scalar chars.
fn char_multiset_overlap(a: &[char], b: &[char]) -> f32 {
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 0.0;
    }
    let mut used = vec![false; b.len()];
    let mut shared = 0usize;
    for &ca in a {
        for (j, &cb) in b.iter().enumerate() {
            if !used[j] && ca == cb {
                used[j] = true;
                shared += 1;
                break;
            }
        }
    }
    shared as f32 / max_len as f32
}

fn fold_latin_diacritic(ch: char) -> char {
    match ch {
        'à' | 'á' | 'ả' | 'ã' | 'ạ' | 'ă' | 'ằ' | 'ắ' | 'ẳ' | 'ẵ' | 'ặ' | 'â' | 'ầ' | 'ấ' | 'ẩ'
        | 'ẫ' | 'ậ' | 'ä' | 'å' | 'æ' => 'a',
        'đ' | 'ð' => 'd',
        'è' | 'é' | 'ẻ' | 'ẽ' | 'ẹ' | 'ê' | 'ề' | 'ế' | 'ể' | 'ễ' | 'ệ' | 'ë' => {
            'e'
        }
        'ì' | 'í' | 'ỉ' | 'ĩ' | 'ị' | 'î' | 'ï' => 'i',
        'ò' | 'ó' | 'ỏ' | 'õ' | 'ọ' | 'ô' | 'ồ' | 'ố' | 'ổ' | 'ỗ' | 'ộ' | 'ơ' | 'ờ' | 'ớ' | 'ở'
        | 'ỡ' | 'ợ' | 'ö' | 'ø' => 'o',
        'ù' | 'ú' | 'ủ' | 'ũ' | 'ụ' | 'ư' | 'ừ' | 'ứ' | 'ử' | 'ữ' | 'ự' | 'ü' => {
            'u'
        }
        'ỳ' | 'ý' | 'ỷ' | 'ỹ' | 'ỵ' | 'ÿ' => 'y',
        'ñ' => 'n',
        'ç' => 'c',
        other => other,
    }
}

/// Strips shared prefix `XX` and shared suffix to isolate the replaced span `YYY -> ZZZZ`.
pub fn strip_affixes<'a, 'b>(orig: &'a [char], fixed: &'b [char]) -> (&'a [char], &'b [char]) {
    let mut pre = 0usize;
    let min_len = orig.len().min(fixed.len());
    while pre < min_len && orig[pre] == fixed[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < (orig.len() - pre).min(fixed.len() - pre)
        && orig[orig.len() - 1 - suf] == fixed[fixed.len() - 1 - suf]
    {
        suf += 1;
    }
    (&orig[pre..orig.len() - suf], &fixed[pre..fixed.len() - suf])
}

fn is_prose_word_token(s: &str) -> bool {
    !s.is_empty()
        && s.chars().any(|c| c.is_alphabetic())
        && s.chars()
            .all(|c| c.is_alphabetic() || c == '\'' || c == '’')
}

/// Row 2: `Undo + Keep Verbatim` (`"hok " -> "hot " -> BS -> Space`).
/// - `1x`: Immediately blocks `(typed, rejected)` in `blocklist` on disk AND shields `typed_lower` in-session
///   so the user never has to backspace the same unwanted replacement twice.
/// - `2x`: Permanently promotes `typed` to `personal_dict` in `rules.toml`.
pub fn record_undo_keep_verbatim(
    store: &mut RuleStore,
    lang: &LangId,
    typed_raw: &str,
    rejected: &str,
) -> LearnEvent {
    let typed_lower = typed_raw.to_lowercase();
    let rejected_lower = rejected.to_lowercase();
    if typed_lower.is_empty() {
        return LearnEvent::None;
    }

    store.add_session_word_shield(&typed_lower);
    store.add_session_pair_block(&typed_lower, &rejected_lower);
    store.add_permanent_block(&typed_lower, &rejected_lower);

    let key = format!("{}:{}->{}", lang.as_str(), typed_lower, rejected_lower);
    let cnt = {
        let entry = store.state_mut().staged_undo_keep.entry(key).or_insert(0);
        *entry = entry.saturating_add(1);
        *entry
    };

    if cnt >= 2 {
        if is_prose_word_token(&typed_lower) {
            store.add_personal_word(lang, &typed_lower);
        }
        let _ = store.flush_state();
        LearnEvent::GraduatedUndoKeep {
            typed: typed_lower,
            rejected: rejected_lower,
        }
    } else {
        let _ = store.flush_state();
        LearnEvent::ShieldedWordSession { word: typed_lower }
    }
}

/// Row 1 & Row 3: Resolves an `EditingRestored` word after an autocorrect undo (`1x BackSpace`).
/// - If `final == rejected`: `Self-Confirm Cancel` (proves BackSpace was an accident!).
/// - If `final == raw_typed`: delegates to `record_undo_keep_verbatim` (`1x` session shield, `2x` permanent).
/// - If `final` is a 3rd word (`fomr -> foam -> BS -> form`): `1x` immediate permanent `blocklist(fomr, foam)`
///   plus routes `(fomr -> form)` through the `2x` guarded manual-edit learner.
pub fn resolve_restored_edit(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    prev_word: Option<&str>,
    raw_typed: &str,
    rejected: &str,
    final_committed: &str,
) -> Vec<LearnEvent> {
    let raw_lower = raw_typed.to_lowercase();
    let rej_lower = rejected.to_lowercase();
    let fin_lower = final_committed.to_lowercase();

    // Row 3: Accidental BackSpace (`"chagnes " -> "changes " -> BS -> "changes "`)
    if fin_lower == rej_lower {
        store.resolve_pending_undo_log(raw_typed, rejected, UndoResolution::SelfConfirmCancelled);
        return vec![LearnEvent::SelfConfirmCancelled];
    }

    // Row 2: Kept verbatim (`"hok " -> "hot " -> BS -> "hok "`)
    if fin_lower == raw_lower {
        store.resolve_pending_undo_log(raw_typed, rejected, UndoResolution::KeptVerbatim);
        return vec![record_undo_keep_verbatim(store, lang, raw_typed, rejected)];
    }

    // Row 1: Deliberately edited to a 3rd word (`"fomr " -> "foam " -> BS -> "form "`).
    // Run `Delta-Span` / `MindChangeDistance` guards first so clearing a word at index 0 to write
    // a brand-new sentence (`"chagnes" -> "hello"`) or completing an early-space suffix
    // (`"prodcut" -> "product" -> BS -> "production"`) never poisons `blocklist`.
    store.resolve_pending_undo_log(
        raw_typed,
        rejected,
        UndoResolution::EditedToThird {
            final_word: final_committed.to_string(),
        },
    );
    let edit_ev = record_manual_edit(store, lexicon, lang, prev_word, raw_typed, final_committed);
    if matches!(
        edit_ev,
        LearnEvent::Ignored(
            SkipReason::DeltaSpanRewrite
                | SkipReason::MindChangeDistance
                | SkipReason::InvalidToken
        )
    ) {
        return vec![edit_ev];
    }
    store.add_permanent_block(&raw_lower, &rej_lower);
    let rej_in_lexicon = lang.as_str() == "en"
        && lexicon.is_some_and(|l| l.contains_any_english(&rej_lower));
    if !rej_in_lexicon && store.is_explicit_personal_word(lang, &rej_lower) {
        let demote_key = format!("demote:{}:{}", lang.as_str(), rej_lower);
        let cnt = {
            let e = store
                .state_mut()
                .staged_undo_keep
                .entry(demote_key)
                .or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        if cnt >= 2 {
            store.remove_personal_word(lang, &rej_lower);
        }
        let _ = store.flush_state();
    }
    vec![
        LearnEvent::BlockedPairPermanent {
            typed: raw_lower,
            rejected: rej_lower,
        },
        edit_ev,
    ]
}

/// Row 4 & Row 6: Evaluates a manual edit (`orig -> fixed`) through `Delta-Span Check` + Guards 1–3,
/// and logs the `1x, 2x, 3x+` manual backspace fix attempt.
pub fn record_manual_edit(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    prev_word: Option<&str>,
    orig_raw: &str,
    fixed_raw: &str,
) -> LearnEvent {
    let ev = record_manual_edit_inner(store, lexicon, lang, prev_word, orig_raw, fixed_raw);
    let label = match &ev {
        LearnEvent::StagedCorrection { count, .. } => Some(format!("staged ({count}x)")),
        LearnEvent::GraduatedCorrection { .. } => Some("graduated".to_string()),
        LearnEvent::StagedContextCorrection { count, .. } => {
            Some(format!("staged_context ({count}x)"))
        }
        LearnEvent::GraduatedContextCorrection { .. } => Some("graduated_context".to_string()),
        LearnEvent::Ignored(
            reason @ (SkipReason::TargetNotValidWord
            | SkipReason::DeltaSpanRewrite
            | SkipReason::MindChangeDistance
            | SkipReason::ValidWordNoContext
            | SkipReason::WeakContextAsymmetry),
        ) => Some(format!("skipped:{}", reason.as_str())),
        _ => None,
    };
    if let Some(outcome_str) = label {
        store.log_manual_backspace_fix(orig_raw, fixed_raw, &outcome_str);
    }
    ev
}

fn record_manual_edit_inner(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    prev_word: Option<&str>,
    orig_raw: &str,
    fixed_raw: &str,
) -> LearnEvent {
    let orig_lower = orig_raw.trim().to_lowercase();
    let fixed_lower = fixed_raw.trim().to_lowercase();

    if orig_lower == fixed_lower {
        return LearnEvent::Ignored(SkipReason::SameWord);
    }
    if !is_prose_word_token(&orig_lower) || !is_prose_word_token(&fixed_lower) {
        return LearnEvent::Ignored(SkipReason::InvalidToken);
    }

    let orig_chars: Vec<char> = orig_lower.chars().collect();
    let fixed_chars: Vec<char> = fixed_lower.chars().collect();
    if fixed_chars.len() < 2 {
        return LearnEvent::Ignored(SkipReason::InvalidToken);
    }

    // Route non-ASCII target words (e.g. Vietnamese diacritics `"dak" -> "đắk"`, `"anh" -> "ảnh"`) to `"vi"`!
    let target_has_non_ascii = fixed_chars
        .iter()
        .any(|c| !c.is_ascii() && c.is_alphabetic());
    // In `fcitx5-lotus` (`lang == "vi"`), `PreInputMethod` only observes raw ASCII Telex keys (`"nhir" -> "nhi"`,
    // `"maf" -> "ma"`, `"quas" -> "qua"`). Never learn pure-ASCII Telex key edits as `[lang.vi.corrections]`.
    if lang.as_str() == "vi" && !target_has_non_ascii {
        return LearnEvent::None;
    }
    let effective_lang = if target_has_non_ascii && lang.as_str() == "en" {
        LangId("vi".to_string())
    } else {
        lang.clone()
    };

    // Length & Delta-Span Check (`XXYYY -> XXZZZZ` suffix/idea rewrite rejection)
    if orig_chars.len().abs_diff(fixed_chars.len()) > 2 {
        store.state_mut().record_skipped(
            effective_lang.as_str(),
            prev_word,
            &orig_lower,
            &fixed_lower,
            SkipReason::DeltaSpanRewrite.as_str(),
        );
        let _ = store.flush_state();
        return LearnEvent::Ignored(SkipReason::DeltaSpanRewrite);
    }

    // Compare on diacritic-folded Latin base chars so adding Vietnamese/accents (`"dak" -> "Đắk"`,
    // `"duoc" -> "được"`, `"cafe" -> "café"`) measures base-key similarity (`dl = 0`), while still
    // rejecting unrelated Vietnamese/English mind changes (`"dak" -> "không"` has `dl = 5`).
    let cmp_orig_chars: Vec<char> = orig_chars
        .iter()
        .copied()
        .map(fold_latin_diacritic)
        .collect();
    let cmp_fixed_chars: Vec<char> = fixed_chars
        .iter()
        .copied()
        .map(fold_latin_diacritic)
        .collect();
    let cmp_orig_str: String = cmp_orig_chars.iter().collect();
    let cmp_fixed_str: String = cmp_fixed_chars.iter().collect();

    let (rem_orig, rem_fixed) = strip_affixes(&cmp_orig_chars, &cmp_fixed_chars);
    if rem_orig.len().abs_diff(rem_fixed.len()) > 2 {
        store.state_mut().record_skipped(
            effective_lang.as_str(),
            prev_word,
            &orig_lower,
            &fixed_lower,
            SkipReason::DeltaSpanRewrite.as_str(),
        );
        let _ = store.flush_state();
        return LearnEvent::Ignored(SkipReason::DeltaSpanRewrite);
    }

    // Guard 1: Whole-word similarity + Stripped Delta-Span overlap
    let max_len = cmp_orig_chars.len().max(cmp_fixed_chars.len());
    let whole_dl = damerau_levenshtein_chars(&cmp_orig_str, &cmp_fixed_str);
    let whole_overlap = char_multiset_overlap(&cmp_orig_chars, &cmp_fixed_chars);

    let max_allowed_dl = if max_len <= 4 {
        1
    } else if max_len <= 7 {
        2
    } else if whole_overlap >= 0.85 {
        4
    } else {
        3
    };

    if whole_dl > max_allowed_dl {
        store.state_mut().record_skipped(
            effective_lang.as_str(),
            prev_word,
            &orig_lower,
            &fixed_lower,
            SkipReason::MindChangeDistance.as_str(),
        );
        let _ = store.flush_state();
        return LearnEvent::Ignored(SkipReason::MindChangeDistance);
    }

    if rem_orig.len() >= 2 && rem_fixed.len() >= 2 {
        let rem_orig_str: String = rem_orig.iter().collect();
        let rem_fixed_str: String = rem_fixed.iter().collect();
        let delta_dl = damerau_levenshtein_chars(&rem_orig_str, &rem_fixed_str);
        let delta_overlap = char_multiset_overlap(rem_orig, rem_fixed);
        if delta_dl > 1 && delta_overlap < 0.50 {
            store.state_mut().record_skipped(
                effective_lang.as_str(),
                prev_word,
                &orig_lower,
                &fixed_lower,
                SkipReason::DeltaSpanRewrite.as_str(),
            );
            let _ = store.flush_state();
            return LearnEvent::Ignored(SkipReason::DeltaSpanRewrite);
        }
    }

    // Target word validity check (prevents learning a typo-to-typo edit like `fomr -> fomrr`
    // or promoting a known typo like `recieve` into `personal_dict`).
    let mut target_is_novel_unknown = false;
    if effective_lang.as_str() == "en" {
        if let Some(lex) = lexicon {
            if !lex.contains_any_english(&fixed_lower) && !store.is_personal_word(&fixed_lower) {
                let is_key_bounce_extension = fixed_lower.starts_with(&orig_lower)
                    && fixed_chars.len() == orig_chars.len() + 1
                    && fixed_chars.last() == orig_chars.last();
                let is_stretch = is_expressive_stretch(&fixed_lower, |w| {
                    store.is_personal_word(w) || lex.contains_any_english(w)
                });
                if fixed_chars.len() < 4
                    || is_key_bounce_extension
                    || is_stretch
                    || lex.lookup_known_typos(&fixed_lower).is_some()
                {
                    store.state_mut().record_skipped(
                        effective_lang.as_str(),
                        prev_word,
                        &orig_lower,
                        &fixed_lower,
                        SkipReason::TargetNotValidWord.as_str(),
                    );
                    let _ = store.flush_state();
                    return LearnEvent::Ignored(SkipReason::TargetNotValidWord);
                }
                target_is_novel_unknown = true;
            }
        }
    }

    // Guard 2: Valid-Word Shield (`"form" -> "from"` must NEVER learn as a global unigram rule!).
    // Core lexicon words are permanently shielded; `personal_dict`-only words can be un-dictionaried
    // if the user manually fixes them into another word `2x`.
    let orig_in_lexicon = effective_lang.as_str() == "en"
        && lexicon.is_some_and(|l| l.contains_any_english(&orig_lower));

    if !orig_in_lexicon {
        // If `orig_lower` previously had a `1x` undo-keep shield (e.g. an accidental `1x` undo on `"forn"`),
        // manually fixing `orig_lower -> fixed_lower` immediately clears that undo shield.
        store.clear_undo_shield(&effective_lang, &orig_lower);

        // Track `trouble_words` prior so future similar smashes near `fixed_lower` get a tie-break boost
        *store
            .state_mut()
            .trouble_words
            .entry(fixed_lower.clone())
            .or_insert(0) += 1;

        if target_is_novel_unknown {
            let unk_key = format!("{}:{}", effective_lang.as_str(), fixed_lower);
            let unk_cnt = {
                let e = store
                    .state_mut()
                    .untouched_unknowns
                    .entry(unk_key)
                    .or_insert(0);
                *e = e.saturating_add(1);
                *e
            };
            if unk_cnt >= 3 {
                store.add_personal_word(&effective_lang, &fixed_lower);
            }
        }

        let stage_key = format!(
            "{}:{}->{}",
            effective_lang.as_str(),
            orig_lower,
            fixed_lower
        );
        let count = {
            let e = store
                .state_mut()
                .staged_corrections
                .entry(stage_key)
                .or_insert(0);
            *e = e.saturating_add(1);
            *e
        };

        // Preserve target casing only if explicitly non-ASCII / proper (like `"Đắk"`), otherwise lowercase
        let target_to_save = if target_has_non_ascii {
            fixed_raw.trim().to_string()
        } else {
            fixed_lower.clone()
        };

        if count >= 2 {
            if store.is_explicit_personal_word(&effective_lang, &orig_lower) {
                store.remove_personal_word(&effective_lang, &orig_lower);
            }
            if target_is_novel_unknown && !store.is_personal_word(&fixed_lower) {
                store.add_personal_word_with_stage(
                    &effective_lang,
                    &fixed_lower,
                    "personal_promoted_2x",
                );
            }
            store.add_correction(&effective_lang, &orig_lower, &target_to_save);
            let _ = store.flush_state();
            return LearnEvent::GraduatedCorrection {
                lang: effective_lang.0,
                orig: orig_lower,
                fixed: target_to_save,
            };
        }
        let _ = store.flush_state();
        return LearnEvent::StagedCorrection {
            lang: effective_lang.0,
            orig: orig_lower,
            fixed: target_to_save,
            count,
        };
    }

    // Guard 3: Contextual Bigram Asymmetry Shield (`"sent form" -> "sent from"`)
    let Some(prev_raw) = prev_word else {
        store.state_mut().record_skipped(
            effective_lang.as_str(),
            None,
            &orig_lower,
            &fixed_lower,
            SkipReason::ValidWordNoContext.as_str(),
        );
        let _ = store.flush_state();
        return LearnEvent::Ignored(SkipReason::ValidWordNoContext);
    };
    let prev_lower = prev_raw.trim().to_lowercase();
    if prev_lower.is_empty() {
        return LearnEvent::Ignored(SkipReason::ValidWordNoContext);
    }

    if let Some(lex) = lexicon {
        let bg_orig = lex.bigram_score(&prev_lower, &orig_lower);
        let bg_fixed = lex.bigram_score(&prev_lower, &fixed_lower);
        if bg_orig >= 0.25 || bg_fixed < 0.50 || (bg_fixed - bg_orig) < 0.35 {
            store.state_mut().record_skipped(
                effective_lang.as_str(),
                Some(&prev_lower),
                &orig_lower,
                &fixed_lower,
                SkipReason::WeakContextAsymmetry.as_str(),
            );
            let _ = store.flush_state();
            return LearnEvent::Ignored(SkipReason::WeakContextAsymmetry);
        }
    }

    let ctx_key = format!("{prev_lower}|{orig_lower}->{fixed_lower}");
    let count = {
        let e = store.state_mut().staged_context.entry(ctx_key).or_insert(0);
        *e = e.saturating_add(1);
        *e
    };
    if count >= 2 {
        store.add_context_correction(&prev_lower, &orig_lower, &fixed_lower);
        let _ = store.flush_state();
        LearnEvent::GraduatedContextCorrection {
            prev: prev_lower,
            orig: orig_lower,
            fixed: fixed_lower,
        }
    } else {
        let _ = store.flush_state();
        LearnEvent::StagedContextCorrection {
            prev: prev_lower,
            orig: orig_lower,
            fixed: fixed_lower,
            count,
        }
    }
}

/// Row 5: Records an unknown word that the user typed and left untouched (`3x -> personal_dict`).
pub fn record_untouched_unknown(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    word_raw: &str,
) -> LearnEvent {
    let lower = word_raw.trim().to_lowercase();
    let char_count = lower.chars().count();
    if char_count < 2 || !is_prose_word_token(&lower) {
        return LearnEvent::None;
    }

    // Never pollute `personal_dict` with expressive stretches (`soooo`, `trờiiii`)
    if is_expressive_stretch(&lower, |w| {
        store.is_personal_word(w) || lexicon.is_some_and(|l| l.contains_any_english(w))
    }) {
        return LearnEvent::None;
    }

    if store.is_personal_word(&lower)
        || (lang.as_str() == "en" && lexicon.is_some_and(|l| l.contains_any_english(&lower)))
    {
        return LearnEvent::None;
    }

    let key = format!("{}:{}", lang.as_str(), lower);
    let count = {
        let e = store.state_mut().untouched_unknowns.entry(key).or_insert(0);
        *e = e.saturating_add(1);
        *e
    };

    let undo_prefix = format!("{}:{}->", lang.as_str(), lower);
    let has_prior_undo = store
        .state()
        .staged_undo_keep
        .iter()
        .any(|(k, &v)| v >= 1 && k.starts_with(&undo_prefix));
    let required_count = if has_prior_undo { 1 } else { 3 };

    if count >= required_count {
        store.add_personal_word(lang, &lower);
        let _ = store.flush_state();
        LearnEvent::GraduatedPersonalWord {
            lang: lang.0.clone(),
            word: lower,
        }
    } else {
        let _ = store.flush_state();
        LearnEvent::StagedUntouchedUnknown {
            lang: lang.0.clone(),
            word: lower,
            count,
        }
    }
}

/// Phase 2 (`fcitx5-lotus`): Records when a user repairs a Telex-composed token (`ảu`) back to raw ASCII (`aur`).
/// Graduates to `telex_passthrough` at `2x`.
pub fn record_telex_passthrough_repair(store: &mut RuleStore, raw_ascii: &str) -> LearnEvent {
    let lower = raw_ascii.trim().to_lowercase();
    if lower.len() < 2 || !lower.is_ascii() {
        return LearnEvent::None;
    }
    let count = {
        let e = store
            .state_mut()
            .staged_telex_passthrough
            .entry(lower.clone())
            .or_insert(0);
        *e = e.saturating_add(1);
        *e
    };
    if count >= 2 {
        store.add_telex_passthrough(&lower);
        let _ = store.flush_state();
        LearnEvent::GraduatedTelexPassthrough { raw_ascii: lower }
    } else {
        let _ = store.flush_state();
        LearnEvent::None
    }
}
