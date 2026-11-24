use std::collections::BTreeSet;
use std::io;

use smartkey_core::{
    AppProfile, ConfidenceTier, DiagnosticEvent, LangId, LoggedDiagnosticRecord, MatchKind,
    RuleOrigin, RuleStore, UndoResolution,
};

use super::theme::{kind_label, manual_outcome_label, profile_label};
use super::types::{
    ActionItem, ActivityRow, ChangeKind, RowAction, RuleEntry, RuleKind, StoreOp, Tone,
};

impl StoreOp {
    /// Applies the op and returns an inverse `StoreOp` when the change can be undone.
    pub(super) fn apply(&self, store: &mut RuleStore) -> Result<Option<StoreOp>, String> {
        let e = |err: io::Error| err.to_string();
        Ok(match self {
            Self::AddWord { lang, word } => store
                .add_user_personal_word(lang, word)
                .map_err(e)?
                .then(|| Self::RemoveWord {
                    lang: lang.clone(),
                    word: word.to_lowercase(),
                    canonical: word.clone(),
                }),
            Self::RemoveWord {
                lang,
                word,
                canonical,
            } => store
                .remove_user_personal_word(lang, word)
                .map_err(e)?
                .then(|| Self::AddWord {
                    lang: lang.clone(),
                    word: canonical.clone(),
                }),
            Self::SetFix { lang, typed, fixed } => {
                match store.set_user_correction(lang, typed, fixed).map_err(e)? {
                    Some(prev) if prev == *fixed => None,
                    Some(prev) => Some(Self::SetFix {
                        lang: lang.clone(),
                        typed: typed.clone(),
                        fixed: prev,
                    }),
                    None => Some(Self::RemoveFix {
                        lang: lang.clone(),
                        typed: typed.clone(),
                        fixed: fixed.clone(),
                    }),
                }
            }
            Self::RemoveFix { lang, typed, fixed } => store
                .remove_correction(lang, typed)
                .map_err(e)?
                .then(|| Self::SetFix {
                    lang: lang.clone(),
                    typed: typed.clone(),
                    fixed: fixed.clone(),
                }),
            Self::SetContext { prev, typed, fixed } => {
                match store
                    .set_user_context_correction(prev, typed, fixed)
                    .map_err(e)?
                {
                    Some(old) if old == *fixed => None,
                    Some(old) => Some(Self::SetContext {
                        prev: prev.clone(),
                        typed: typed.clone(),
                        fixed: old,
                    }),
                    None => Some(Self::RemoveContext {
                        prev: prev.clone(),
                        typed: typed.clone(),
                        fixed: fixed.clone(),
                    }),
                }
            }
            Self::RemoveContext { prev, typed, fixed } => store
                .remove_context_correction(prev, typed)
                .map_err(e)?
                .then(|| Self::SetContext {
                    prev: prev.clone(),
                    typed: typed.clone(),
                    fixed: fixed.clone(),
                }),
            Self::SetSnippet { trigger, expansion } => {
                match store.set_snippet(trigger, expansion).map_err(e)? {
                    Some(old) if old == *expansion => None,
                    Some(old) => Some(Self::SetSnippet {
                        trigger: trigger.clone(),
                        expansion: old,
                    }),
                    None => Some(Self::RemoveSnippet {
                        trigger: trigger.clone(),
                        expansion: expansion.clone(),
                    }),
                }
            }
            Self::RemoveSnippet { trigger, expansion } => store
                .remove_snippet(trigger)
                .map_err(e)?
                .then(|| Self::SetSnippet {
                    trigger: trigger.clone(),
                    expansion: expansion.clone(),
                }),
            Self::AddBlock { typed, rejected } => store
                .add_user_blocklist_pair(typed, rejected)
                .map_err(e)?
                .then(|| Self::RemoveBlock {
                    typed: typed.to_lowercase(),
                    rejected: rejected.to_lowercase(),
                }),
            Self::RemoveBlock { typed, rejected } => store
                .remove_blocklist_pair(typed, rejected)
                .map_err(e)?
                .then(|| Self::AddBlock {
                    typed: typed.clone(),
                    rejected: rejected.clone(),
                }),
            Self::AddPassthrough { raw } => {
                store
                    .add_user_telex_passthrough(raw)
                    .map_err(e)?
                    .then(|| Self::RemovePassthrough {
                        raw: raw.to_lowercase(),
                    })
            }
            Self::RemovePassthrough { raw } => store
                .remove_telex_passthrough(raw)
                .map_err(e)?
                .then(|| Self::AddPassthrough { raw: raw.clone() }),
            Self::SetSetting { key, value } => {
                let before = store.get_setting(key);
                let (canon, saved) = store.set_setting(key, value).map_err(e)?;
                match before {
                    Some(old) if old != saved => Some(Self::SetSetting {
                        key: canon.to_string(),
                        value: old,
                    }),
                    _ => None,
                }
            }
            Self::SetAppProfile { app, profile } => {
                let before = store
                    .compiled()
                    .app_profiles
                    .classify_explicit_override(app);
                if before == *profile {
                    return Ok(None);
                }
                store.remove_app_profile_override(None, app).map_err(e)?;
                if let Some(p) = profile {
                    store.add_app_profile_override(*p, app).map_err(e)?;
                }
                Some(Self::SetAppProfile {
                    app: app.clone(),
                    profile: before,
                })
            }
            Self::KeepRule { stat_key, inner } => {
                store.reload_if_modified();
                let prev_stat = stat_key
                    .as_ref()
                    .and_then(|k| store.state().rule_stats.get(k).cloned());
                let inner_inv = inner.apply(store)?;
                match (stat_key, inner_inv) {
                    (Some(k), Some(inv)) => Some(Self::Batch(vec![
                        Self::RestoreRuleStat {
                            stat_key: k.clone(),
                            stat: prev_stat,
                        },
                        inv,
                    ])),
                    (Some(k), None) => Some(Self::RestoreRuleStat {
                        stat_key: k.clone(),
                        stat: prev_stat,
                    }),
                    (None, inv) => inv,
                }
            }
            Self::DeleteRule { stat_key, inner } => {
                store.reload_if_modified();
                let prev_stat = stat_key
                    .as_ref()
                    .and_then(|k| store.state().rule_stats.get(k).cloned());
                let Some(inner_inv) = inner.apply(store)? else {
                    return Ok(None);
                };
                match stat_key {
                    Some(k) => Some(Self::Batch(vec![
                        inner_inv,
                        Self::RestoreRuleStat {
                            stat_key: k.clone(),
                            stat: prev_stat,
                        },
                    ])),
                    None => Some(inner_inv),
                }
            }
            Self::RestoreRuleStat { stat_key, stat } => {
                store.reload_if_modified();
                let prev = store.state().rule_stats.get(stat_key).cloned();
                match stat {
                    Some(s) => {
                        store
                            .state_mut()
                            .rule_stats
                            .insert(stat_key.clone(), s.clone());
                    }
                    None => {
                        store.state_mut().rule_stats.remove(stat_key);
                    }
                }
                store.flush_state().map_err(e)?;
                Some(Self::RestoreRuleStat {
                    stat_key: stat_key.clone(),
                    stat: prev,
                })
            }
            Self::PromoteStaged {
                queue,
                key,
                canonical,
            } => {
                let add_op = staged_to_add_op(queue, key, canonical.as_deref())?;
                let snapshot = snapshot_staged_entry(store, queue, key);
                let mut inv = Vec::new();
                if let Some(op_inv) = add_op.apply(store)? {
                    inv.push(op_inv);
                }
                let dismissed = store.dismiss_staged_entry(queue, key).map_err(e)?;
                if dismissed {
                    if let Some((count, snap_canon)) = snapshot {
                        inv.push(Self::RestoreStaged {
                            queue: queue.clone(),
                            key: key.clone(),
                            count,
                            canonical: snap_canon.or_else(|| canonical.clone()),
                        });
                    }
                }
                inv.reverse();
                if inv.is_empty() {
                    None
                } else {
                    Some(Self::Batch(inv))
                }
            }
            Self::DismissStaged { queue, key } => {
                let snapshot = snapshot_staged_entry(store, queue, key);
                let dismissed = store.dismiss_staged_entry(queue, key).map_err(e)?;
                if dismissed {
                    snapshot.map(|(count, canonical)| Self::RestoreStaged {
                        queue: queue.clone(),
                        key: key.clone(),
                        count,
                        canonical,
                    })
                } else {
                    None
                }
            }
            Self::RestoreStaged {
                queue,
                key,
                count,
                canonical,
            } => {
                store.reload_if_modified();
                let st = store.state_mut();
                let restored = match queue.as_str() {
                    "fix" | "case_fix" => {
                        st.staged_corrections.insert(key.clone(), *count);
                        true
                    }
                    "context" => {
                        st.staged_context.insert(key.clone(), *count);
                        true
                    }
                    "untouched_word" | "case_upgrade" => {
                        st.untouched_unknowns.insert(key.clone(), *count);
                        if let Some(canon) = canonical {
                            st.staged_personal_canonical
                                .insert(key.clone(), canon.clone());
                        }
                        true
                    }
                    "personal_demote" | "undo_keep" => {
                        st.staged_undo_keep.insert(key.clone(), *count);
                        true
                    }
                    "contraction_reject" => {
                        st.staged_contraction_rejects.insert(key.clone(), *count);
                        true
                    }
                    "telex_passthrough" => {
                        st.staged_telex_passthrough.insert(key.clone(), *count);
                        true
                    }
                    _ => false,
                };
                if restored {
                    store.flush_state().map_err(e)?;
                    Some(Self::DismissStaged {
                        queue: queue.clone(),
                        key: key.clone(),
                    })
                } else {
                    None
                }
            }
            Self::Batch(ops) => {
                let mut inv = Vec::new();
                for op in ops {
                    if let Some(back) = op.apply(store)? {
                        inv.push(back);
                    }
                }
                inv.reverse();
                if inv.is_empty() {
                    None
                } else {
                    Some(Self::Batch(inv))
                }
            }
        })
    }
}

fn snapshot_staged_entry(
    store: &mut RuleStore,
    queue: &str,
    key: &str,
) -> Option<(u32, Option<String>)> {
    store.reload_if_modified();
    let st = store.state();
    match queue {
        "fix" | "case_fix" => st.staged_corrections.get(key).copied().map(|c| (c, None)),
        "context" => st.staged_context.get(key).copied().map(|c| (c, None)),
        "untouched_word" | "case_upgrade" => st.untouched_unknowns.get(key).copied().map(|c| {
            let canon = st.staged_personal_canonical.get(key).cloned();
            (c, canon)
        }),
        "personal_demote" | "undo_keep" => st.staged_undo_keep.get(key).copied().map(|c| (c, None)),
        "contraction_reject" => st
            .staged_contraction_rejects
            .get(key)
            .copied()
            .map(|c| (c, None)),
        "telex_passthrough" => st
            .staged_telex_passthrough
            .get(key)
            .copied()
            .map(|c| (c, None)),
        _ => None,
    }
}

fn staged_to_add_op(queue: &str, key: &str, canonical: Option<&str>) -> Result<StoreOp, String> {
    match queue {
        "fix" => {
            let (lang, pair) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            let (typed, fixed) = pair
                .split_once("->")
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            Ok(StoreOp::SetFix {
                lang: LangId(lang.to_string()),
                typed: typed.to_string(),
                fixed: fixed.to_string(),
            })
        }
        "case_fix" => {
            let rest = key.strip_prefix("case:").unwrap_or(key);
            let (lang, word) = rest
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged word '{key}'"))?;
            Ok(StoreOp::AddWord {
                lang: LangId(lang.to_string()),
                word: canonical.unwrap_or(word).to_string(),
            })
        }
        "context" => {
            let (lhs, fixed) = key
                .split_once("->")
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            let (prev, typed) = lhs
                .split_once('|')
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            Ok(StoreOp::SetContext {
                prev: prev.to_string(),
                typed: typed.to_string(),
                fixed: fixed.to_string(),
            })
        }
        "untouched_word" => {
            let (lang, word) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged word '{key}'"))?;
            Ok(StoreOp::AddWord {
                lang: LangId(lang.to_string()),
                word: canonical.unwrap_or(word).to_string(),
            })
        }
        "telex_passthrough" => Ok(StoreOp::AddPassthrough {
            raw: key.to_string(),
        }),
        other => Err(format!(
            "SmartKey can't learn '{other}' entries early; remove it instead"
        )),
    }
}

impl RuleEntry {
    pub(super) fn delete_op(&self) -> StoreOp {
        let inner = match self.kind {
            RuleKind::Fix => StoreOp::RemoveFix {
                lang: self.lang.clone(),
                typed: self.a.clone(),
                fixed: self.b.clone(),
            },
            RuleKind::Context => StoreOp::RemoveContext {
                prev: self.prev.clone(),
                typed: self.a.clone(),
                fixed: self.b.clone(),
            },
            RuleKind::Snippet => StoreOp::RemoveSnippet {
                trigger: self.a.clone(),
                expansion: self.b.clone(),
            },
            RuleKind::Word => StoreOp::RemoveWord {
                lang: self.lang.clone(),
                word: self.a.clone(),
                canonical: self.b.clone(),
            },
            RuleKind::Block => StoreOp::RemoveBlock {
                typed: self.a.clone(),
                rejected: self.b.clone(),
            },
            RuleKind::Passthrough => StoreOp::RemovePassthrough {
                raw: self.a.clone(),
            },
        };
        StoreOp::DeleteRule {
            stat_key: self.stat_key.clone(),
            inner: Box::new(inner),
        }
    }

    pub(super) fn keep_op(&self) -> StoreOp {
        let inner = match self.kind {
            RuleKind::Fix => StoreOp::SetFix {
                lang: self.lang.clone(),
                typed: self.a.clone(),
                fixed: self.b.clone(),
            },
            RuleKind::Context => StoreOp::SetContext {
                prev: self.prev.clone(),
                typed: self.a.clone(),
                fixed: self.b.clone(),
            },
            RuleKind::Snippet => StoreOp::SetSnippet {
                trigger: self.a.clone(),
                expansion: self.b.clone(),
            },
            RuleKind::Word => StoreOp::AddWord {
                lang: self.lang.clone(),
                word: self.b.clone(),
            },
            RuleKind::Block => StoreOp::AddBlock {
                typed: self.a.clone(),
                rejected: self.b.clone(),
            },
            RuleKind::Passthrough => StoreOp::AddPassthrough {
                raw: self.a.clone(),
            },
        };
        StoreOp::KeepRule {
            stat_key: self.stat_key.clone(),
            inner: Box::new(inner),
        }
    }

    pub(super) fn title(&self) -> String {
        let base = self.item_label();
        if self.lang.as_str() != "en" && matches!(self.kind, RuleKind::Fix | RuleKind::Word) {
            format!("{base}  ({})", self.lang.as_str())
        } else {
            base
        }
    }

    pub(super) fn item_label(&self) -> String {
        match self.kind {
            RuleKind::Fix | RuleKind::Block | RuleKind::Snippet => {
                format!("{} → {}", self.a, self.b)
            }
            RuleKind::Context => format!("{} {} → {}", self.prev, self.a, self.b),
            RuleKind::Word => self.b.clone(),
            RuleKind::Passthrough => self.a.clone(),
        }
    }

    pub(super) fn type_badge(&self) -> String {
        match self.kind {
            RuleKind::Word => format!("Word ({})", self.lang.as_str()),
            RuleKind::Fix => format!("Typo fix ({})", self.lang.as_str()),
            RuleKind::Context => "Phrase fix".to_string(),
            RuleKind::Snippet => "Snippet".to_string(),
            RuleKind::Block => "Never-fix block".to_string(),
            RuleKind::Passthrough => "Telex keep (vi)".to_string(),
        }
    }

    pub(super) fn kind_noun(&self) -> &'static str {
        match self.kind {
            RuleKind::Word => "Word",
            RuleKind::Fix => "Fix",
            RuleKind::Context => "Phrase fix",
            RuleKind::Snippet => "Snippet",
            RuleKind::Block => "Block",
            RuleKind::Passthrough => "Telex keep",
        }
    }

    pub(super) fn added_by(&self) -> &'static str {
        if self.pinned {
            return "you (pinned)";
        }
        match self.origin {
            None => "rules.toml",
            Some(RuleOrigin::Manual) => "you",
            Some(RuleOrigin::ManualFix2x) => "learned: fixed 2×",
            Some(RuleOrigin::UndoReject) => "learned: undo",
            Some(RuleOrigin::Untouched3x) => "learned: typed 3×",
        }
    }

    pub(super) fn source_text(&self) -> &'static str {
        if self.pinned {
            return "Pinned manually";
        }
        match self.origin {
            None => "Edited by hand in rules.toml",
            Some(RuleOrigin::Manual) => "Added by you",
            Some(RuleOrigin::ManualFix2x) => "Fixed by hand 2× (graduated)",
            Some(RuleOrigin::UndoReject) => "Blocked after Backspace undo",
            Some(RuleOrigin::Untouched3x) => "Typed 3× without fixing",
        }
    }

    pub(super) fn learned_detail(&self) -> String {
        let how = match self.origin {
            Some(RuleOrigin::ManualFix2x) => "learned from your edits",
            Some(RuleOrigin::UndoReject) => "blocked when you undid it",
            Some(RuleOrigin::Untouched3x) => "typed 3× unfixed",
            _ => "in rules.toml",
        };
        let mut parts = vec![how.to_string()];
        if self.kind != RuleKind::Block {
            parts.push(if self.hits == 0 {
                "never used since".to_string()
            } else {
                format!("used {}×", self.hits)
            });
        }
        if self.strikes > 0 {
            parts.push(format!("undone {}×", self.strikes));
        }
        parts.join(" · ")
    }

    pub(super) fn problem_diagnosis(&self) -> String {
        if self.strikes >= 2 {
            let total = self.hits.saturating_add(self.strikes);
            let pct = (u64::from(self.strikes) * 100) / u64::from(total.max(1));
            return format!("Undone {} of {} uses ({}%)", self.strikes, total, pct);
        }
        if self.strikes == 1 {
            return "Undone once after firing".to_string();
        }
        if self.kind == RuleKind::Block {
            return "Auto-blocked after you undid this correction".to_string();
        }
        if self.kind == RuleKind::Word
            && self.lang.as_str() == "vi"
            && self.a.is_ascii()
            && self.hits == 0
        {
            return "Saved in Vietnamese (vi) despite ASCII letters · never used since (0×)"
                .to_string();
        }
        if self.kind == RuleKind::Word && self.hits == 0 {
            return "Saved automatically after 3 untouched uses · never used since (0×)"
                .to_string();
        }
        if self.origin == Some(RuleOrigin::ManualFix2x) && self.hits < 3 {
            return format!(
                "Learned automatically after 2 fixes · low usage ({}×)",
                self.hits
            );
        }
        "Waiting for confirmation".to_string()
    }

    pub(super) fn keep_effect(&self) -> String {
        match self.kind {
            RuleKind::Word => format!("Stays in {} dictionary", self.lang.as_str()),
            RuleKind::Block => "Block permanently".to_string(),
            RuleKind::Fix | RuleKind::Context => "Save permanent fix".to_string(),
            RuleKind::Passthrough => "Keep raw in Telex".to_string(),
            RuleKind::Snippet => "Keep snippet".to_string(),
        }
    }

    pub(super) fn remove_effect(&self) -> String {
        match self.kind {
            RuleKind::Word => "Delete; allow auto-fix".to_string(),
            RuleKind::Block => "Unblock; allow again".to_string(),
            RuleKind::Fix | RuleKind::Context => "Delete; stop fixing".to_string(),
            RuleKind::Passthrough => "Stop Telex exception".to_string(),
            RuleKind::Snippet => "Delete snippet".to_string(),
        }
    }

    pub(super) fn warning_tag(&self) -> Option<&'static str> {
        if self.strikes >= 2 {
            Some("often undone")
        } else if self.kind == RuleKind::Word && self.lang.as_str() == "vi" && self.a.is_ascii() {
            Some("ASCII in vi")
        } else {
            None
        }
    }

    pub(super) fn tokens(&self) -> [&str; 2] {
        [self.a.as_str(), self.b.as_str()]
    }
}

pub(super) fn collect_seen_apps(
    records: &[LoggedDiagnosticRecord],
    tokens: &[&str],
) -> Vec<String> {
    let mut apps = Vec::new();
    for r in records.iter().rev() {
        let Some(app) = r.app.as_deref().filter(|s| !s.trim().is_empty()) else {
            continue;
        };
        let hit = match &r.event {
            DiagnosticEvent::AutocorrectApplied { typed, applied, .. } => {
                tokens_match(tokens, typed, Some(applied))
            }
            DiagnosticEvent::BackspaceUndo {
                typed, rejected, ..
            } => tokens_match(tokens, typed, Some(rejected)),
            DiagnosticEvent::ManualBackspaceFix { orig, fixed, .. } => {
                tokens_match(tokens, orig, Some(fixed))
            }
            DiagnosticEvent::UnfixedUnknown { word, .. }
            | DiagnosticEvent::WordUnchanged { word, .. }
            | DiagnosticEvent::RealWordDeclined { word, .. } => tokens_match(tokens, word, None),
            DiagnosticEvent::RuleLearned { rule, .. } => tokens
                .iter()
                .any(|t| !t.is_empty() && rule.eq_ignore_ascii_case(t)),
            _ => false,
        };
        if hit && !apps.iter().any(|existing| existing == app) {
            apps.push(app.to_string());
            if apps.len() >= 4 {
                break;
            }
        }
    }
    apps
}

fn tokens_match(tokens: &[&str], a: &str, b: Option<&str>) -> bool {
    tokens.iter().any(|t| {
        !t.is_empty()
            && (t.eq_ignore_ascii_case(a) || b.is_some_and(|other| t.eq_ignore_ascii_case(other)))
    })
}

pub(super) fn humanize_rule(rule: &str) -> String {
    if let Some(w) = rule.strip_prefix("personal_dict:") {
        format!("new word \"{w}\"")
    } else if let Some(w) = rule.strip_prefix("telex_passthrough:") {
        format!("Telex keeps \"{w}\"")
    } else if let Some(pair) = rule.strip_prefix("block:") {
        format!("never {}", pair.replace("->", " → "))
    } else {
        rule.replace("->", " → ")
    }
}

/// Staging counters keep ticking after a rule graduates; hide ones that are already active rules.
pub(super) fn staged_already_active(queue: &str, key: &str, active: &BTreeSet<String>) -> bool {
    let probe = match queue {
        "fix" => key
            .split_once("->")
            .and_then(|(lhs, _)| lhs.split_once(':'))
            .map(|(lang, typed)| format!("fix:{lang}:{typed}")),
        "context" => key.split_once("->").map(|(lhs, _)| format!("ctx:{lhs}")),
        "untouched_word" => key
            .split_once(':')
            .map(|(lang, w)| format!("word:{lang}:{}", w.to_lowercase())),
        "case_fix" => key
            .strip_prefix("case:")
            .and_then(|r| r.split_once(':'))
            .map(|(lang, w)| format!("word:{lang}:{}", w.to_lowercase())),
        "telex_passthrough" => Some(format!("pt:{key}")),
        _ => None,
    };
    probe.is_some_and(|p| active.contains(&p))
}

pub(super) struct StagedViewMeta {
    pub(super) type_badge: String,
    pub(super) kind_noun: &'static str,
    pub(super) title: String,
    pub(super) item_label: String,
    pub(super) lang: String,
    pub(super) problem: String,
    pub(super) source: String,
    pub(super) detail: String,
    pub(super) keep_effect: String,
    pub(super) remove_effect: String,
    pub(super) tokens: Vec<String>,
}

pub(super) fn describe_staged_rich(
    queue: &str,
    key: &str,
    canonical: Option<&str>,
    count: u32,
    threshold: u32,
) -> Option<StagedViewMeta> {
    let progress = if count >= threshold {
        format!("{count}× (not learned yet)")
    } else {
        format!("{count} of {threshold} times")
    };
    match queue {
        "fix" => {
            let (lang, pair) = key.split_once(':')?;
            let (typed, fixed) = pair.split_once("->")?;
            let item_label = format!("{typed} → {fixed}");
            let title = if lang == "en" {
                item_label.clone()
            } else {
                format!("{item_label}  ({lang})")
            };
            Some(StagedViewMeta {
                type_badge: format!("Pending fix ({lang})"),
                kind_noun: "Fix",
                title,
                item_label: item_label.clone(),
                lang: lang.to_string(),
                problem: if count >= threshold {
                    format!("Corrected by hand {count}× without auto-graduating yet")
                } else {
                    format!("Corrected by hand {count} of {threshold} times")
                },
                source: format!("Staging counter ({count}/{threshold})"),
                detail: format!("you fixed this {progress}"),
                keep_effect: format!("Learn into {lang} fixes"),
                remove_effect: "Reset staged count".to_string(),
                tokens: vec![typed.to_string(), fixed.to_string()],
            })
        }
        "context" => {
            let (lhs, fixed) = key.split_once("->")?;
            let (prev, typed) = lhs.split_once('|')?;
            let item_label = format!("{prev} {typed} → {fixed}");
            Some(StagedViewMeta {
                type_badge: "Pending phrase fix".to_string(),
                kind_noun: "Phrase",
                title: item_label.clone(),
                item_label: item_label.clone(),
                lang: "en".to_string(),
                problem: format!(
                    "Corrected in phrase \"{prev} {typed}\" {count} of {threshold} times"
                ),
                source: format!("Staging counter ({count}/{threshold})"),
                detail: format!("you fixed this {progress}"),
                keep_effect: "Learn phrase fix now".to_string(),
                remove_effect: "Reset staged count".to_string(),
                tokens: vec![typed.to_string(), fixed.to_string()],
            })
        }
        "untouched_word" => {
            let (lang, word) = key.split_once(':')?;
            let shown = canonical.unwrap_or(word);
            let title = if lang == "en" {
                shown.to_string()
            } else {
                format!("{shown}  ({lang})")
            };
            Some(StagedViewMeta {
                type_badge: format!("Pending word ({lang})"),
                kind_noun: "Word",
                title,
                item_label: shown.to_string(),
                lang: lang.to_string(),
                problem: format!("Typed {count} of {threshold} times without fixing"),
                source: format!("Staging counter ({count}/{threshold})"),
                detail: format!("typed {progress} without fixing"),
                keep_effect: format!("Add to {lang} dictionary"),
                remove_effect: "Forget staged count".to_string(),
                tokens: vec![word.to_string(), shown.to_string()],
            })
        }
        "case_fix" => {
            let rest = key.strip_prefix("case:")?;
            let (lang, word) = rest.split_once(':')?;
            let shown = canonical.unwrap_or(word);
            Some(StagedViewMeta {
                type_badge: format!("Pending casing ({lang})"),
                kind_noun: "Word",
                title: shown.to_string(),
                item_label: shown.to_string(),
                lang: lang.to_string(),
                problem: format!("Capitalisation adjusted {count} of {threshold} times"),
                source: format!("Staging counter ({count}/{threshold})"),
                detail: format!("capitalisation fixed {progress}"),
                keep_effect: format!("Add to {lang} dictionary"),
                remove_effect: "Forget staged count".to_string(),
                tokens: vec![word.to_string(), shown.to_string()],
            })
        }
        "telex_passthrough" => Some(StagedViewMeta {
            type_badge: "Pending Telex keep (vi)".to_string(),
            kind_noun: "Telex",
            title: key.to_string(),
            item_label: key.to_string(),
            lang: "vi".to_string(),
            problem: format!("Kept as raw ASCII {count} of {threshold} times in Telex"),
            source: format!("Staging counter ({count}/{threshold})"),
            detail: format!("kept as typed in Telex {progress}"),
            keep_effect: "Keep raw in Telex".to_string(),
            remove_effect: "Forget staged count".to_string(),
            tokens: vec![key.to_string()],
        }),
        _ => None,
    }
}

pub(super) fn collect_rules(store: &RuleStore) -> Vec<RuleEntry> {
    let c = store.compiled();
    let mut out = Vec::new();
    let mut push = |kind: RuleKind,
                    lang: LangId,
                    a: &str,
                    b: &str,
                    prev: &str,
                    stat_key: Option<String>,
                    pinned: bool| {
        let (tier, origin, hits, strikes, created_ms, last_hit_ms) = match &stat_key {
            Some(k) => {
                let (tier, stat, hits, strikes) = store.classify_rule_confidence(k);
                (
                    tier,
                    stat.map(|s| s.origin),
                    hits,
                    strikes,
                    stat.map(|s| s.created_epoch_ms).unwrap_or(0),
                    stat.and_then(|s| s.last_hit_epoch_ms),
                )
            }
            None => (
                ConfidenceTier::Manual,
                Some(RuleOrigin::Manual),
                0,
                0,
                0,
                None,
            ),
        };
        out.push(RuleEntry {
            kind,
            lang,
            a: a.to_string(),
            b: b.to_string(),
            prev: prev.to_string(),
            tier,
            origin,
            pinned,
            hits,
            strikes,
            created_ms,
            last_hit_ms,
            stat_key,
        });
    };

    for (typed, fixed) in &c.corrections {
        push(
            RuleKind::Fix,
            LangId::en(),
            typed,
            fixed,
            "",
            Some(RuleStore::stat_key_fix("en", typed)),
            false,
        );
    }
    for (lk, map) in &c.lang_corrections {
        for (typed, fixed) in map {
            push(
                RuleKind::Fix,
                LangId(lk.clone()),
                typed,
                fixed,
                "",
                Some(RuleStore::stat_key_fix(lk, typed)),
                false,
            );
        }
    }
    for ((prev, typed), fixed) in &c.context_corrections {
        push(
            RuleKind::Context,
            LangId::en(),
            typed,
            fixed,
            prev,
            Some(RuleStore::stat_key_ctx(prev, typed)),
            false,
        );
    }
    for (trigger, expansion) in &c.snippets {
        push(
            RuleKind::Snippet,
            LangId::en(),
            trigger,
            expansion,
            "",
            None,
            false,
        );
    }
    for w in &c.personal_dict {
        let canon = c
            .personal_dict_canonical
            .get(w)
            .map(String::as_str)
            .unwrap_or(w);
        push(
            RuleKind::Word,
            LangId::en(),
            w,
            canon,
            "",
            Some(RuleStore::stat_key_word("en", w)),
            c.pinned.contains(w),
        );
    }
    for (lk, set) in &c.lang_personal_dict {
        if lk == "en" {
            continue;
        }
        let canon_map = c.lang_personal_dict_canonical.get(lk);
        for w in set {
            let canon = canon_map
                .and_then(|m| m.get(w))
                .map(String::as_str)
                .unwrap_or(w);
            push(
                RuleKind::Word,
                LangId(lk.clone()),
                w,
                canon,
                "",
                Some(RuleStore::stat_key_word(lk, w)),
                false,
            );
        }
    }
    for (t, r) in &c.blocklist {
        push(
            RuleKind::Block,
            LangId::en(),
            t,
            r,
            "",
            Some(RuleStore::stat_key_block(t, r)),
            false,
        );
    }
    for w in &c.telex_passthrough {
        push(
            RuleKind::Passthrough,
            LangId::vi(),
            w,
            "",
            "",
            Some(RuleStore::stat_key_pt(w)),
            false,
        );
    }
    out
}

pub(super) fn describe_event(ev: &DiagnosticEvent) -> (String, String, String, Tone) {
    match ev {
        DiagnosticEvent::AutocorrectApplied {
            typed,
            applied,
            kind,
            total_count,
        } => (
            format!("{typed} → {applied}"),
            kind_label(*kind).to_string(),
            if *total_count > 1 {
                format!("{total_count}× so far")
            } else {
                String::new()
            },
            Tone::Normal,
        ),
        DiagnosticEvent::BackspaceUndo {
            typed,
            rejected,
            resolution,
            ..
        } => {
            let note = match resolution {
                UndoResolution::Triggered => String::new(),
                UndoResolution::KeptVerbatim => "kept what you typed".to_string(),
                UndoResolution::EditedToThird { final_word } => format!("you wrote {final_word}"),
                UndoResolution::SelfConfirmCancelled => "you retyped the fix".to_string(),
                UndoResolution::ClauseErased => "you erased it".to_string(),
            };
            (
                format!("{typed} → {rejected}"),
                "you undid it".to_string(),
                note,
                Tone::Warn,
            )
        }
        DiagnosticEvent::ManualBackspaceFix {
            orig,
            fixed,
            outcome,
            ..
        } => (
            format!("{orig} → {fixed}"),
            "you fixed it".to_string(),
            manual_outcome_label(outcome),
            Tone::Normal,
        ),
        DiagnosticEvent::UnfixedUnknown {
            word,
            best_candidate,
            ..
        } => (
            word.clone(),
            match best_candidate {
                Some(c) => format!("left alone (maybe {c}?)"),
                None => "unknown word".to_string(),
            },
            String::new(),
            Tone::Muted,
        ),
        DiagnosticEvent::RealWordDeclined {
            word,
            prev,
            candidate,
            ..
        } => (
            format!("{prev} {word}"),
            format!("left alone ({candidate} too risky)"),
            String::new(),
            Tone::Muted,
        ),
        DiagnosticEvent::AppBypass {
            app,
            profile,
            reason,
        } => (
            app.clone(),
            if *profile == AppProfile::Bypass {
                "SmartKey off here".to_string()
            } else {
                format!("treated as {}", profile_label(*profile))
            },
            reason.replace(['-', '_'], " "),
            Tone::Muted,
        ),
        DiagnosticEvent::RuleLearned {
            rule,
            stage,
            previous,
        } => (
            humanize_rule(rule),
            "SmartKey learned this".to_string(),
            match (stage.as_str(), previous) {
                ("graduated_2x", _) => "after 2 fixes".to_string(),
                ("superseded", Some(old)) => format!("replaces {old}"),
                ("block_permanent", _) => "after you undid it".to_string(),
                (s, _) if s.contains("untouched") => "typed 3× unfixed".to_string(),
                (s, _) => s.replace('_', " "),
            },
            Tone::Normal,
        ),
        DiagnosticEvent::WordUnchanged { word, reason } => (
            word.clone(),
            format!("kept ({})", kind_label(*reason)),
            String::new(),
            Tone::Muted,
        ),
        DiagnosticEvent::PackNotice { notice, detail } => (
            notice.replace('_', " "),
            detail.clone(),
            String::new(),
            Tone::Warn,
        ),
    }
}

pub(super) fn explain_target(ev: &DiagnosticEvent) -> Option<(Option<String>, String)> {
    match ev {
        DiagnosticEvent::AutocorrectApplied { typed, .. }
        | DiagnosticEvent::BackspaceUndo { typed, .. } => Some((None, typed.clone())),
        DiagnosticEvent::ManualBackspaceFix { orig, .. } => Some((None, orig.clone())),
        DiagnosticEvent::UnfixedUnknown { word, .. }
        | DiagnosticEvent::WordUnchanged { word, .. } => Some((None, word.clone())),
        DiagnosticEvent::RealWordDeclined { word, prev, .. } => {
            Some((Some(prev.clone()), word.clone()))
        }
        _ => None,
    }
}

fn is_single_word(s: &str) -> bool {
    !s.trim().is_empty() && !s.trim().contains(char::is_whitespace)
}

pub(super) fn activity_actions(row: &ActivityRow) -> Vec<ActionItem> {
    let lang = if row.lang.is_empty() || row.lang == "-" {
        LangId::en()
    } else {
        LangId(row.lang.clone())
    };
    let mut items = Vec::new();
    let mut add = |key: char, label: String, hint: String, action: RowAction| {
        items.push(ActionItem {
            key,
            label,
            hint,
            action,
        });
    };
    let word_item = |word: &str| RowAction::Ops {
        kind: ChangeKind::Added,
        ops: vec![StoreOp::AddWord {
            lang: lang.clone(),
            word: word.to_string(),
        }],
        msg: format!("Added \"{word}\" to your words"),
    };
    match &row.event {
        DiagnosticEvent::AutocorrectApplied {
            typed,
            applied,
            kind,
            ..
        } => {
            add(
                'b',
                "Never do this again".to_string(),
                format!("blocks {typed} → {applied}"),
                RowAction::Ops {
                    kind: ChangeKind::Blocked,
                    ops: vec![StoreOp::AddBlock {
                        typed: typed.clone(),
                        rejected: applied.clone(),
                    }],
                    msg: format!("Blocked {typed} → {applied}"),
                },
            );
            if *kind != MatchKind::UserCorrectionRule {
                add(
                    'a',
                    "Always fix it this way".to_string(),
                    "saves it as your fix".to_string(),
                    RowAction::Ops {
                        kind: ChangeKind::Added,
                        ops: vec![StoreOp::SetFix {
                            lang: lang.clone(),
                            typed: typed.clone(),
                            fixed: applied.clone(),
                        }],
                        msg: format!("Saved fix {typed} → {applied}"),
                    },
                );
            }
            if is_single_word(typed) {
                add(
                    'w',
                    format!("\"{typed}\" is a real word"),
                    "adds it to your words".to_string(),
                    word_item(typed),
                );
            }
        }
        DiagnosticEvent::BackspaceUndo {
            typed, rejected, ..
        } => {
            add(
                'b',
                "Never do this again".to_string(),
                format!("blocks {typed} → {rejected}"),
                RowAction::Ops {
                    kind: ChangeKind::Blocked,
                    ops: vec![StoreOp::AddBlock {
                        typed: typed.clone(),
                        rejected: rejected.clone(),
                    }],
                    msg: format!("Blocked {typed} → {rejected}"),
                },
            );
            if is_single_word(typed) {
                add(
                    'w',
                    format!("\"{typed}\" is a real word"),
                    "adds it to your words".to_string(),
                    word_item(typed),
                );
            }
        }
        DiagnosticEvent::ManualBackspaceFix { orig, fixed, .. } => {
            add(
                'a',
                format!("Always fix {orig} → {fixed}"),
                "saves it now instead of waiting".to_string(),
                RowAction::Ops {
                    kind: ChangeKind::Added,
                    ops: vec![StoreOp::SetFix {
                        lang: lang.clone(),
                        typed: orig.clone(),
                        fixed: fixed.clone(),
                    }],
                    msg: format!("Saved fix {orig} → {fixed}"),
                },
            );
        }
        DiagnosticEvent::UnfixedUnknown {
            word,
            best_candidate,
            ..
        } => {
            if is_single_word(word) {
                add(
                    'w',
                    format!("\"{word}\" is a real word"),
                    "stops it showing as missed".to_string(),
                    word_item(word),
                );
            }
            if let Some(c) = best_candidate {
                add(
                    'a',
                    format!("Always fix {word} → {c}"),
                    "saves it as your fix".to_string(),
                    RowAction::Ops {
                        kind: ChangeKind::Added,
                        ops: vec![StoreOp::SetFix {
                            lang: lang.clone(),
                            typed: word.clone(),
                            fixed: c.clone(),
                        }],
                        msg: format!("Saved fix {word} → {c}"),
                    },
                );
            }
        }
        DiagnosticEvent::RealWordDeclined {
            word,
            prev,
            candidate,
            ..
        } => {
            add(
                'a',
                format!("Fix \"{word}\" → \"{candidate}\" after \"{prev}\""),
                "only in this phrase".to_string(),
                RowAction::Ops {
                    kind: ChangeKind::Added,
                    ops: vec![StoreOp::SetContext {
                        prev: prev.clone(),
                        typed: word.clone(),
                        fixed: candidate.clone(),
                    }],
                    msg: format!("Saved fix {prev} {word} → {candidate}"),
                },
            );
        }
        DiagnosticEvent::RuleLearned { .. } => {
            add(
                'v',
                "Review learned rules".to_string(),
                "opens the Review screen".to_string(),
                RowAction::GoReview,
            );
        }
        _ => {}
    }
    if let Some((prev, token)) = explain_target(&row.event) {
        add(
            'x',
            "Why did this happen?".to_string(),
            "scores and other guesses".to_string(),
            RowAction::Explain {
                lang: lang.clone(),
                prev,
                token,
            },
        );
    }
    let app = match &row.event {
        DiagnosticEvent::AppBypass { app, .. } => Some(app.clone()),
        _ => row.app.clone(),
    };
    if let Some(app) = app.filter(|a| !a.trim().is_empty()) {
        add(
            'p',
            format!("App mode for {app}"),
            "off, browser, chat, or prose".to_string(),
            RowAction::PickApp { app },
        );
    }
    items
}
