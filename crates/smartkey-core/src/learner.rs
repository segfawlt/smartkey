use serde::{Deserialize, Serialize};

use crate::casing::is_expressive_stretch;
use crate::keyboard::damerau_levenshtein_chars;
use crate::lexicon::EnglishLexicon;
use crate::scorer::{is_staged_rejection_pair_for, UserRuleView};
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

/// A staged contraction rejection goes stale once this many corrections of the same pair were
/// accepted (not undone) after it: two stray BackSpaces months apart must not add up to a block,
/// while a user who really wants the raw form rejects it again within a few applications.
const CONTRACTION_REJECT_STALE_AFTER_ACCEPTS: u32 = 10;

/// Rejects the `(typed, rejected)` autocorrect pair. Curated unconditional-contraction pairs
/// (`dont -> don't`) and case-only proper-noun pairs (`english -> English`) are staged first: the permanent block — which also erases a matching
/// learned correction — only lands on the `2x` rejection across sessions, matching the `2x`
/// evidence the manual-fix learner needs. One accidental BackSpace must never permanently
/// silence a near-100% curated rule. Every other pair keeps the immediate `1x` permanent block.
/// Returns `true` when the pair is (now) permanently blocked.
fn reject_pair_permanently(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    typed_lower: &str,
    rejected_lower: &str,
) -> bool {
    if is_staged_rejection_pair_for(lexicon, lang.as_str(), typed_lower, rejected_lower) {
        let key = format!("{}:{}->{}", lang.as_str(), typed_lower, rejected_lower);
        store.sync_state_before_write();
        let applied_now = store
            .state()
            .applied_counts
            .get(&format!("{typed_lower}->{rejected_lower}"))
            .copied()
            .unwrap_or(0);
        let state = store.state_mut();
        // Evidence decay: the application that triggered this rejection is not an acceptance,
        // so subtract it. A cleared `applied_counts` (now < snapshot) keeps the evidence.
        if let Some(&snapshot) = state.staged_contraction_reject_applied_at.get(&key) {
            let accepted = applied_now.saturating_sub(snapshot).saturating_sub(1);
            if accepted >= CONTRACTION_REJECT_STALE_AFTER_ACCEPTS {
                state.staged_contraction_rejects.remove(&key);
            }
        }
        state
            .staged_contraction_reject_applied_at
            .insert(key.clone(), applied_now);
        let cnt = {
            let e = state.staged_contraction_rejects.entry(key).or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        let _ = store.flush_state();
        if cnt < 2 {
            return false;
        }
    }
    store.add_permanent_block(typed_lower, rejected_lower);
    true
}

/// Clears every rejection trace for `(typed, fixed)` after a manual fix graduated: the permanent
/// blocklist pair and the staged contraction-rejection counter. Repeated deliberate fixes must
/// always outrank earlier — possibly accidental — rejections; without this, a stale block both
/// shadows the graduated correction in the scorer and makes the next `add_permanent_block`
/// erase it again, deadlocking recovery by typing.
fn clear_pair_rejection(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    typed_lower: &str,
    fixed: &str,
) {
    let _ = store.remove_blocklist_pair(typed_lower, fixed);
    let key = format!(
        "{}:{}->{}",
        lang.as_str(),
        typed_lower,
        fixed.to_lowercase()
    );
    let state = store.state_mut();
    state.staged_contraction_reject_applied_at.remove(&key);
    if state.staged_contraction_rejects.remove(&key).is_some()
        || is_staged_rejection_pair_for(lexicon, lang.as_str(), typed_lower, &fixed.to_lowercase())
    {
        // The 2x-blocked path leaves a persisted undo-keep key that `is_undo_shielded` reads as a
        // cross-session shield; graduation must lift it too.
        state.staged_undo_keep.remove(&key);
    }
}

/// Row 2: `Undo + Keep Verbatim` (`"hok " -> "hot " -> BS -> Space`).
/// - `1x`: Immediately blocks `(typed, rejected)` in `blocklist` on disk AND shields `typed_lower`
///   in-session so the user never has to backspace the same unwanted replacement twice — EXCEPT
///   curated contraction pairs (`"dont" -> "don't"`), which only stage the rejection (see
///   `reject_pair_permanently`); the session shield already covers the rest of the session.
/// - `2x`: Permanently promotes `typed` to `personal_dict` in `rules.toml`.
pub fn record_undo_keep_verbatim(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    typed_raw: &str,
    rejected: &str,
) -> LearnEvent {
    let typed_lower = typed_raw.to_lowercase();
    let rejected_lower = rejected.to_lowercase();
    if typed_lower.is_empty() {
        return LearnEvent::None;
    }

    if store.is_pinned(&typed_lower) {
        // Pinned words never learn against their curated capitalization, accident or not.
        return LearnEvent::None;
    }
    store.add_session_word_shield(&typed_lower);
    store.add_session_pair_block(&typed_lower, &rejected_lower);
    store.sync_state_before_write();
    if !reject_pair_permanently(store, lexicon, lang, &typed_lower, &rejected_lower) {
        // `1x` rejection of a curated contraction pair: session shield only, nothing permanent.
        // Must return BEFORE touching `staged_undo_keep`: `RuleStore::is_undo_shielded` treats any
        // persisted `staged_undo_keep` key for the word as a cross-session shield, which would
        // silence the curated rule on the very next session and defeat the `2x` requirement.
        return LearnEvent::ShieldedWordSession { word: typed_lower };
    }

    let key = format!("{}:{}->{}", lang.as_str(), typed_lower, rejected_lower);
    let cnt = {
        let entry = store.state_mut().staged_undo_keep.entry(key).or_insert(0);
        *entry = entry.saturating_add(1);
        *entry
    };

    if cnt >= 2 {
        let word_to_save = if typed_raw.trim().chars().any(char::is_uppercase) {
            typed_raw.trim()
        } else {
            &typed_lower
        };
        if is_prose_word_token(&typed_lower) {
            store.add_personal_word_with_stage(lang, word_to_save, "undo_keep_2x");
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
/// - If `final == raw_typed`: delegates to `record_undo_keep_verbatim` (`1x` immediate permanent
///   blocklist for non-curated pairs + session shield, staging curated/case-only pairs at `2x`;
///   promotes `raw_typed` to `personal_dict` at `2x`).
/// - If `final` is a 3rd word (`fomr -> foam -> BS -> form`): `1x` immediate permanent `blocklist(fomr, foam)`
///   (curated contraction pairs stage at `2x` instead, see `reject_pair_permanently`)
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

    // Case-only corrections (`polish -> Polish`) compare equal once lowercased, so Row 3 would
    // swallow a deliberate keep-verbatim. Compare exact case for those pairs.
    let case_only = raw_lower == rej_lower;
    let final_is_rejected = if case_only {
        final_committed == rejected
    } else {
        fin_lower == rej_lower
    };
    let final_is_raw = if case_only {
        final_committed == raw_typed
    } else {
        fin_lower == raw_lower
    };

    // Row 3: Accidental BackSpace (`"chagnes " -> "changes " -> BS -> "changes "`)
    if final_is_rejected {
        store.resolve_pending_undo_log(raw_typed, rejected, UndoResolution::SelfConfirmCancelled);
        return vec![LearnEvent::SelfConfirmCancelled];
    }

    // Row 2: Kept verbatim (`"hok " -> "hot " -> BS -> "hok "`)
    if final_is_raw {
        store.resolve_pending_undo_log(raw_typed, rejected, UndoResolution::KeptVerbatim);
        return vec![record_undo_keep_verbatim(
            store, lexicon, lang, raw_typed, rejected,
        )];
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
    store.sync_state_before_write();
    // Curated contraction pairs stage their rejection (`2x` -> permanent); other pairs keep the
    // immediate `1x` block. Editing to a 3rd word (`"dont" -> "don't" -> BS -> "donut "`) is weak
    // evidence against the contraction — it may simply have been a different intended word.
    let pair_blocked = reject_pair_permanently(store, lexicon, lang, &raw_lower, &rej_lower);
    let rej_in_lexicon =
        lang.as_str() == "en" && lexicon.is_some_and(|l| l.contains_any_english(&rej_lower));
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
    let mut events = vec![edit_ev];
    if pair_blocked {
        events.insert(
            0,
            LearnEvent::BlockedPairPermanent {
                typed: raw_lower,
                rejected: rej_lower,
            },
        );
    }
    events
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
        LearnEvent::GraduatedPersonalWord { .. } => Some("graduated_case".to_string()),
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

fn is_intentional_cased_token(trimmed: &str) -> bool {
    let alpha: Vec<char> = trimmed.chars().filter(|c| c.is_alphabetic()).collect();
    if alpha.len() < 2 {
        return false;
    }
    let has_upper = alpha.iter().any(|c| c.is_uppercase());
    let has_lower = alpha.iter().any(|c| c.is_lowercase());
    let looks_like_shift_slip = alpha[0].is_uppercase()
        && alpha[1].is_uppercase()
        && alpha.iter().skip(2).any(|c| c.is_lowercase());
    has_upper && has_lower && !looks_like_shift_slip
}

fn record_manual_edit_inner(
    store: &mut RuleStore,
    lexicon: Option<&EnglishLexicon>,
    lang: &LangId,
    prev_word: Option<&str>,
    orig_raw: &str,
    fixed_raw: &str,
) -> LearnEvent {
    let orig_trimmed = orig_raw.trim();
    let fixed_trimmed = fixed_raw.trim();
    let orig_lower = orig_trimmed.to_lowercase();
    let fixed_lower = fixed_trimmed.to_lowercase();

    if orig_lower == fixed_lower {
        if orig_trimmed != fixed_trimmed
            && is_prose_word_token(&fixed_lower)
            && fixed_lower.chars().count() >= 2
            && is_intentional_cased_token(fixed_trimmed)
        {
            let target_has_non_ascii = fixed_lower
                .chars()
                .any(|c| !c.is_ascii() && c.is_alphabetic());
            if lang.as_str() == "vi" && !target_has_non_ascii {
                return LearnEvent::None;
            }
            let effective_lang = if target_has_non_ascii && lang.as_str() == "en" {
                LangId("vi".to_string())
            } else {
                lang.clone()
            };
            let has_internal_upper = fixed_trimmed.chars().skip(1).any(char::is_uppercase);
            let is_tech_word = effective_lang.as_str() == "en"
                && lexicon.is_some_and(|l| {
                    l.lookup_proper_noun(&fixed_lower).is_none()
                        && l.lookup_word(&fixed_lower).is_some_and(|m| m.is_tech())
                });
            let is_blocked_proper = effective_lang.as_str() == "en"
                && lexicon.is_some_and(|l| {
                    l.lookup_proper_noun(&fixed_lower)
                        .is_some_and(|p| p == fixed_trimmed)
                })
                && store.is_pair_blocked(&fixed_lower, &fixed_lower);
            let is_non_lex_word = effective_lang.as_str() != "en"
                || lexicon.is_none_or(|l| {
                    !l.contains_any_english(&fixed_lower)
                        && l.lookup_known_typos(&fixed_lower).is_none()
                });
            let is_stretch = !store.is_personal_word(&fixed_lower)
                && is_expressive_stretch(&fixed_lower, |w| {
                    store.is_personal_word(w) || lexicon.is_some_and(|l| l.contains_any_english(w))
                });
            if !is_stretch
                && (has_internal_upper || is_tech_word || is_blocked_proper || is_non_lex_word)
            {
                store.reload_if_modified();
                if is_non_lex_word && !store.is_personal_word(&fixed_lower) {
                    store.add_session_word_shield(&fixed_lower);
                    let unk_key = format!("{}:{fixed_lower}", effective_lang.as_str());
                    let e = store
                        .state_mut()
                        .untouched_unknowns
                        .entry(unk_key.clone())
                        .or_insert(0);
                    *e = e.saturating_add(2);
                    store
                        .state_mut()
                        .staged_personal_canonical
                        .insert(unk_key, fixed_trimmed.to_string());
                }
                let case_key = format!("case:{}:{fixed_lower}", effective_lang.as_str());
                let count = {
                    let e = store
                        .state_mut()
                        .staged_corrections
                        .entry(case_key)
                        .or_insert(0);
                    *e = e.saturating_add(1);
                    *e
                };
                if count >= 2 {
                    let eff_lex = if effective_lang == *lang {
                        lexicon
                    } else {
                        None
                    };
                    clear_pair_rejection(
                        store,
                        eff_lex,
                        &effective_lang,
                        &fixed_lower,
                        fixed_trimmed,
                    );
                    store.state_mut().staged_undo_keep.remove(&format!(
                        "{}:{fixed_lower}->{fixed_lower}",
                        effective_lang.as_str()
                    ));
                    if !is_blocked_proper {
                        store.add_personal_word_with_stage(
                            &effective_lang,
                            fixed_trimmed,
                            "case_fix_2x",
                        );
                        store.clear_untouched_unknown(&effective_lang, &fixed_lower);
                    }
                    let _ = store.flush_state();
                    return LearnEvent::GraduatedPersonalWord {
                        lang: effective_lang.0,
                        word: fixed_trimmed.to_string(),
                    };
                }
                let _ = store.flush_state();
                return LearnEvent::StagedCorrection {
                    lang: effective_lang.0,
                    orig: orig_trimmed.to_string(),
                    fixed: fixed_trimmed.to_string(),
                    count,
                };
            }
        }
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

    // Sync any external edits to `rules.toml` / `learner_state.json` BEFORE mutating in-memory
    // staging maps or flushing state so concurrent CLI commands are never clobbered.
    store.reload_if_modified();

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

        // Preserve target casing if explicitly non-ASCII (`"Đắk"`), mixed-case (`"PyTorch"`, `"FastAPI"`),
        // or intentional TitleCase (`"Hyprland"` when `orig_raw` was lowercase or `target_is_novel_unknown`).
        let fixed_trimmed = fixed_raw.trim();
        let has_internal_upper = fixed_trimmed.chars().skip(1).any(char::is_uppercase);
        let has_intentional_title = fixed_trimmed.chars().next().is_some_and(char::is_uppercase)
            && (target_is_novel_unknown
                || orig_raw
                    .trim()
                    .chars()
                    .next()
                    .is_some_and(char::is_lowercase)
                || lexicon
                    .is_some_and(|l| l.lookup_word(&fixed_lower).is_some_and(|m| m.is_tech())));
        let target_to_save = if target_has_non_ascii || has_internal_upper || has_intentional_title
        {
            fixed_trimmed.to_string()
        } else {
            fixed_lower.clone()
        };

        if target_is_novel_unknown {
            store.add_session_word_shield(&fixed_lower);
            let unk_key = format!("{}:{}", effective_lang.as_str(), fixed_lower);
            if target_to_save.chars().any(char::is_uppercase) {
                store
                    .state_mut()
                    .staged_personal_canonical
                    .insert(unk_key.clone(), target_to_save.clone());
            }
            let unk_cnt = {
                let e = store
                    .state_mut()
                    .untouched_unknowns
                    .entry(unk_key)
                    .or_insert(0);
                *e = e.saturating_add(2);
                *e
            };
            if unk_cnt >= 3 {
                store.add_personal_word_with_stage(
                    &effective_lang,
                    &target_to_save,
                    "personal_promoted_2x",
                );
                store.clear_untouched_unknown(&effective_lang, &fixed_lower);
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

        if count >= 2 {
            if store.is_explicit_personal_word(&effective_lang, &orig_lower) {
                store.remove_personal_word(&effective_lang, &orig_lower);
            }
            if (target_is_novel_unknown || has_internal_upper || has_intentional_title)
                && !store.is_personal_word(&fixed_lower)
            {
                store.add_personal_word_with_stage(
                    &effective_lang,
                    &target_to_save,
                    "personal_promoted_2x",
                );
                store.clear_untouched_unknown(&effective_lang, &fixed_lower);
            }
            store.add_correction(&effective_lang, &orig_lower, &target_to_save);
            // Graduation is strictly stronger evidence than any earlier rejection of the same
            // pair — clear its blocklist entry so the new rule actually fires.
            let eff_lex = if effective_lang == *lang {
                lexicon
            } else {
                None
            };
            clear_pair_rejection(store, eff_lex, &effective_lang, &orig_lower, &target_to_save);
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
        // Same as the plain graduation above: a stale rejection of this pair must not shadow
        // the newly graduated context correction.
        let eff_lex = if effective_lang == *lang {
            lexicon
        } else {
            None
        };
        clear_pair_rejection(store, eff_lex, &effective_lang, &orig_lower, &fixed_lower);
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
    let word_trimmed = word_raw.trim();
    let lower = word_trimmed.to_lowercase();
    let char_count = lower.chars().count();
    if char_count < 2 || !is_prose_word_token(&lower) {
        return LearnEvent::None;
    }

    store.reload_if_modified();

    // Never pollute `personal_dict` with expressive stretches (`soooo`, `trờiiii`)
    if is_expressive_stretch(&lower, |w| {
        store.is_personal_word(w) || lexicon.is_some_and(|l| l.contains_any_english(w))
    }) {
        return LearnEvent::None;
    }

    let has_upper = word_trimmed.chars().any(char::is_uppercase);
    let has_internal_upper = word_trimmed.chars().skip(1).any(char::is_uppercase);
    let is_cased_tech_word = has_upper
        && lexicon.is_some_and(|l| {
            l.lookup_proper_noun(&lower).is_none()
                && l.lookup_word(&lower).is_some_and(|m| m.is_tech())
        });

    if store.is_personal_word(&lower) {
        let canon = if lang.as_str() != "en" {
            store
                .compiled()
                .lang_personal_dict_canonical
                .get(lang.as_str())
                .and_then(|m| m.get(&lower))
                .or_else(|| store.compiled().personal_dict_canonical.get(&lower))
        } else {
            store.compiled().personal_dict_canonical.get(&lower)
        };
        if is_intentional_cased_token(word_trimmed)
            && canon.is_none_or(|c| !c.chars().any(char::is_uppercase))
        {
            let target_lang = if lang.as_str() != "en"
                && !store.is_explicit_personal_word(lang, &lower)
                && store.compiled().personal_dict.contains(&lower)
            {
                LangId::en()
            } else {
                lang.clone()
            };
            let up_key = format!("case_upgrade:{}:{}", target_lang.as_str(), lower);
            let cnt = {
                let e = store
                    .state_mut()
                    .untouched_unknowns
                    .entry(up_key.clone())
                    .or_insert(0);
                *e = e.saturating_add(1);
                *e
            };
            if cnt >= 2 {
                store.state_mut().untouched_unknowns.remove(&up_key);
                store.add_personal_word_with_stage(&target_lang, word_trimmed, "case_upgrade_2x");
                let _ = store.flush_state();
                return LearnEvent::GraduatedPersonalWord {
                    lang: target_lang.0,
                    word: word_trimmed.to_string(),
                };
            }
            let _ = store.flush_state();
            return LearnEvent::StagedUntouchedUnknown {
                lang: target_lang.0,
                word: word_trimmed.to_string(),
                count: cnt,
            };
        }
        return LearnEvent::None;
    }
    if lang.as_str() == "en"
        && !has_internal_upper
        && !is_cased_tech_word
        && lexicon.is_some_and(|l| l.contains_any_english(&lower))
    {
        return LearnEvent::None;
    }

    let key = format!("{}:{}", lang.as_str(), lower);
    if has_upper && is_intentional_cased_token(word_trimmed) {
        store
            .state_mut()
            .staged_personal_canonical
            .insert(key.clone(), word_trimmed.to_string());
    }
    let count = {
        let e = store
            .state_mut()
            .untouched_unknowns
            .entry(key.clone())
            .or_insert(0);
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
    let word_to_save = if has_upper {
        word_trimmed.to_string()
    } else if let Some(canon) = store.state().staged_personal_canonical.get(&key).cloned() {
        canon
    } else {
        lower.clone()
    };

    if count >= required_count {
        let stage = if has_prior_undo {
            "undo_keep_2x"
        } else {
            "untouched_3x"
        };
        store.add_personal_word_with_stage(lang, &word_to_save, stage);
        store.clear_untouched_unknown(lang, &lower);
        let _ = store.flush_state();
        LearnEvent::GraduatedPersonalWord {
            lang: lang.0.clone(),
            word: word_to_save,
        }
    } else {
        let _ = store.flush_state();
        LearnEvent::StagedUntouchedUnknown {
            lang: lang.0.clone(),
            word: word_to_save,
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
    store.sync_state_before_write();
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
