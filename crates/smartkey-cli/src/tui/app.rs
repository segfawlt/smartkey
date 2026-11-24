use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;
use smartkey_core::{
    normalize_program_id, ConfidenceTier, DiagnosticEvent, EvalContext, EvalOutcome, EventLog,
    LangId, LatinScorer, Preset, RuleStore, SmartKeyEngine,
};

use crate::{
    collect_staged_entries, detect_fcitx5_module_so, detect_fcitx5_running, format_age_ago,
    inspect_im_environment, load_rule_store_mut, load_rule_store_readonly,
};

use super::ops::{
    activity_actions, collect_rules, collect_seen_apps, describe_event, describe_staged_rich,
    explain_target, staged_already_active,
};
use super::theme::{clamp, cli_error_text, language_name, near_x, near_y, point_in, step};
use super::types::{
    range_indices, ActivityFilter, ActivityRow, ChangeEntry, ChangeKind, DictRow, DictSection,
    DragTarget, Form, LayoutRects, LayoutState, Modal, ReviewItem, ReviewSection, ReviewTab,
    RowAction, RuleEntry, RuleKind, Screen, SettingsRow, StoreOp, Toast, ACTIVITY_MAX_ROWS,
    ACTIVITY_WINDOW_MS, APP_CHOICES, CORRECTION_SPECS, DETAIL_MIN_WIDTH, DIAGNOSTIC_SPECS,
    INSPECT_PCT_MAX, INSPECT_PCT_MIN, MAX_ALMOST, MAX_HISTORY, MAX_SEEN_APPS, SIDEBAR_MIN_TOTAL,
    SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SPLIT_PCT_MAX, SPLIT_PCT_MIN, UNDO_SPECS, UNLISTED_SPEC,
    VIETNAMESE_SPECS,
};

pub(crate) struct App {
    pub(super) config_dir: PathBuf,
    pub(super) log_dir: PathBuf,
    pub(super) notify_reload: fn() -> bool,
    pub(super) screen: Screen,
    pub(super) modal: Option<Modal>,
    pub(super) filter_editing: bool,
    pub(super) focus_sidebar: bool,
    pub(super) pending_g: bool,
    pub(super) visual_anchor: Option<usize>,
    pub(super) toast: Option<Toast>,
    pub(super) history: Vec<ChangeEntry>,
    pub(super) next_change_id: u64,
    pub(super) undo_stack: Vec<u64>,
    pub(super) redo_stack: Vec<u64>,
    pub(super) layout: LayoutState,
    pub(super) dragging: Option<DragTarget>,
    pub(super) rects: Cell<LayoutRects>,

    pub(super) parse_locked: bool,
    pub(super) parse_error: Option<String>,
    pub(super) env_warnings: Vec<String>,
    pub(super) enabled: bool,
    pub(super) vi_lotus: bool,
    pub(super) lotus_installed: bool,
    pub(super) fcitx5_running: bool,
    pub(super) last_refresh_ms: u64,
    pub(super) mode_summary: String,
    pub(super) undo_summary: String,
    pub(super) last_app: Option<String>,
    pub(super) brain_words: usize,
    pub(super) brain_fixes: usize,
    pub(super) brain_blocks: usize,

    pub(super) activity: Vec<ActivityRow>,
    pub(super) activity_view: Vec<usize>,
    pub(super) activity_idx: usize,
    pub(super) activity_filter: ActivityFilter,
    pub(super) activity_search: String,
    pub(super) day_fixes: usize,
    pub(super) day_undone: usize,
    pub(super) day_by_hand: usize,

    pub(super) review_tab: ReviewTab,
    pub(super) review: Vec<ReviewItem>,
    pub(super) review_idx: usize,
    pub(super) review_marked: BTreeSet<String>,
    pub(super) review_hidden: usize,
    pub(super) changes_idx: usize,

    pub(super) dict: Vec<DictRow>,
    pub(super) dict_counts: [usize; 5],
    pub(super) dict_section: DictSection,
    pub(super) dict_view: Vec<usize>,
    pub(super) dict_idx: usize,
    pub(super) dict_filter: String,

    pub(super) settings_rows: Vec<SettingsRow>,
    pub(super) settings_values: BTreeMap<String, String>,
    pub(super) settings_idx: usize,
}

impl App {
    pub(crate) fn new(config_dir: PathBuf, log_dir: PathBuf, notify_reload: fn() -> bool) -> Self {
        let mut app = Self {
            config_dir,
            log_dir,
            notify_reload,
            screen: Screen::Activity,
            modal: None,
            filter_editing: false,
            focus_sidebar: false,
            pending_g: false,
            visual_anchor: None,
            toast: None,
            history: Vec::new(),
            next_change_id: 1,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            layout: LayoutState::default(),
            dragging: None,
            rects: Cell::new(LayoutRects::default()),
            parse_locked: false,
            parse_error: None,
            env_warnings: inspect_im_environment().1,
            enabled: true,
            vi_lotus: false,
            lotus_installed: false,
            fcitx5_running: false,
            last_refresh_ms: current_epoch_ms(),
            mode_summary: String::new(),
            undo_summary: String::new(),
            last_app: None,
            brain_words: 0,
            brain_fixes: 0,
            brain_blocks: 0,
            activity: Vec::new(),
            activity_view: Vec::new(),
            activity_idx: 0,
            activity_filter: ActivityFilter::All,
            activity_search: String::new(),
            day_fixes: 0,
            day_undone: 0,
            day_by_hand: 0,
            review_tab: ReviewTab::Queue,
            review: Vec::new(),
            review_idx: 0,
            review_marked: BTreeSet::new(),
            review_hidden: 0,
            changes_idx: 0,
            dict: Vec::new(),
            dict_counts: [0; 5],
            dict_section: DictSection::Words,
            dict_view: Vec::new(),
            dict_idx: 0,
            dict_filter: String::new(),
            settings_rows: Vec::new(),
            settings_values: BTreeMap::new(),
            settings_idx: 0,
        };
        app.refresh();
        app
    }

    pub(super) fn is_idle(&self) -> bool {
        self.modal.is_none() && !self.filter_editing && self.dragging.is_none()
    }

    pub(crate) fn refresh(&mut self) {
        let store = match load_rule_store_readonly(&self.config_dir, Preset::Balanced) {
            Ok((s, _)) => s,
            Err(err) => {
                self.set_error(format!("Could not read settings: {}", cli_error_text(&err)));
                return;
            }
        };
        self.parse_locked = store.is_parse_locked();
        self.parse_error = store.parse_error().map(str::to_string);

        let c = store.compiled();
        self.enabled = c.enabled;
        self.vi_lotus = c.vi_lotus_enabled;
        self.lotus_installed = detect_fcitx5_module_so("liblotus.so").is_some();
        self.fcitx5_running = detect_fcitx5_running();

        let now = current_epoch_ms();
        self.last_refresh_ms = now;
        let records = EventLog::read_records_in_range(
            &self.log_dir,
            Some(now.saturating_sub(ACTIVITY_WINDOW_MS)),
            None,
        )
        .unwrap_or_default();

        self.build_activity(&records, &store, now);
        let rules = collect_rules(&store);
        self.build_review(&rules, &store, &records);
        self.build_dictionary(&rules);
        self.build_settings(&store, &records);

        self.brain_words = self.dict_counts[DictSection::Words.index()];
        self.brain_fixes = self.dict_counts[DictSection::Fixes.index()];
        self.brain_blocks = self.dict_counts[DictSection::NeverFix.index()];
        self.mode_summary = build_mode_summary(&self.settings_values);
        self.undo_summary = build_undo_summary(&self.settings_values);
        self.last_app = self
            .activity
            .iter()
            .find_map(|r| match (&r.app, &r.event) {
                (_, DiagnosticEvent::AppBypass { app, .. }) => Some(normalize_program_id(app)),
                (Some(a), _) => Some(normalize_program_id(a)),
                _ => None,
            })
            .filter(|s| !s.is_empty());
    }

    fn build_activity(
        &mut self,
        records: &[smartkey_core::LoggedDiagnosticRecord],
        store: &RuleStore,
        now: u64,
    ) {
        let day_start = now.saturating_sub(86_400_000);
        self.day_fixes = 0;
        self.day_undone = 0;
        self.day_by_hand = 0;
        for r in records.iter().filter(|r| r.ts_ms >= day_start) {
            match r.event {
                DiagnosticEvent::AutocorrectApplied { .. } => self.day_fixes += 1,
                DiagnosticEvent::BackspaceUndo { .. } => self.day_undone += 1,
                DiagnosticEvent::ManualBackspaceFix { .. } => self.day_by_hand += 1,
                _ => {}
            }
        }

        fn push(
            rows: &mut Vec<ActivityRow>,
            ts_ms: u64,
            app: Option<String>,
            lang: String,
            ev: &DiagnosticEvent,
        ) {
            if let (Some(last), DiagnosticEvent::AppBypass { app: a, .. }) = (rows.last_mut(), ev) {
                if let DiagnosticEvent::AppBypass { app: b, .. } = &last.event {
                    if a == b {
                        last.repeat += 1;
                        return;
                    }
                }
            }
            let (change, why, note, tone) = describe_event(ev);
            rows.push(ActivityRow {
                ts_ms,
                app,
                lang,
                change,
                why,
                note,
                tone,
                repeat: 1,
                event: ev.clone(),
            });
        }
        let mut rows: Vec<ActivityRow> = Vec::new();
        if records.is_empty() {
            for ev in store.state().recent_events.iter().rev() {
                push(&mut rows, 0, None, "en".to_string(), ev);
            }
        } else {
            for r in records.iter().rev() {
                push(&mut rows, r.ts_ms, r.app.clone(), r.lang.clone(), &r.event);
                if rows.len() >= ACTIVITY_MAX_ROWS {
                    break;
                }
            }
        }
        self.activity = rows;
        self.rebuild_activity_view();
    }

    pub(super) fn rebuild_activity_view(&mut self) {
        let needle = self.activity_search.trim().to_lowercase();
        self.activity_view = self
            .activity
            .iter()
            .enumerate()
            .filter(|(_, r)| self.activity_filter.matches(&r.event))
            .filter(|(_, r)| {
                needle.is_empty()
                    || r.change.to_lowercase().contains(&needle)
                    || r.why.to_lowercase().contains(&needle)
                    || r.app
                        .as_deref()
                        .unwrap_or("")
                        .to_lowercase()
                        .contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        clamp(&mut self.activity_idx, self.activity_view.len());
    }

    fn build_review(
        &mut self,
        rules: &[RuleEntry],
        store: &RuleStore,
        records: &[smartkey_core::LoggedDiagnosticRecord],
    ) {
        let mut items = Vec::new();
        let mut learned: Vec<&RuleEntry> = rules
            .iter()
            .filter(|r| {
                matches!(
                    r.tier,
                    ConfidenceTier::Suspect | ConfidenceTier::Provisional
                )
            })
            .collect();
        learned.sort_by_key(|r| {
            (
                r.tier != ConfidenceTier::Suspect,
                std::cmp::Reverse(r.strikes),
            )
        });
        for r in learned {
            let section = if r.kind == RuleKind::Block {
                ReviewSection::Blocked
            } else {
                ReviewSection::Learned
            };
            items.push(ReviewItem {
                id: format!(
                    "rule:{:?}:{}:{}:{}:{}",
                    r.kind,
                    r.lang.as_str(),
                    r.prev,
                    r.a,
                    r.b
                ),
                section,
                type_badge: r.type_badge(),
                kind_noun: r.kind_noun(),
                title: r.title(),
                item_label: r.item_label(),
                lang_label: language_name(r.lang.as_str()),
                problem: r.problem_diagnosis(),
                source: r.source_text().to_string(),
                detail: r.learned_detail(),
                warning: r.warning_tag(),
                added_ms: r.created_ms,
                last_hit_ms: r.last_hit_ms,
                hits: r.hits,
                strikes: r.strikes,
                seen_apps: collect_seen_apps(records, &r.tokens()),
                keep_effect: r.keep_effect(),
                remove_effect: r.remove_effect(),
                keep_op: r.keep_op(),
                remove_op: r.delete_op(),
            });
        }

        let active: BTreeSet<String> = rules
            .iter()
            .map(|r| match r.kind {
                RuleKind::Fix => format!("fix:{}:{}", r.lang.as_str(), r.a),
                RuleKind::Context => format!("ctx:{}|{}", r.prev, r.a),
                RuleKind::Word => format!("word:{}:{}", r.lang.as_str(), r.a),
                RuleKind::Passthrough => format!("pt:{}", r.a),
                _ => String::new(),
            })
            .collect();
        let staged = collect_staged_entries(store.state());
        let mut almost: Vec<(u32, ReviewItem)> = Vec::new();
        let mut already = 0usize;
        for s in &staged {
            if staged_already_active(s.queue, &s.key, &active) {
                already += 1;
                continue;
            }
            if s.count < 2 {
                continue;
            }
            let Some(meta) = describe_staged_rich(
                s.queue,
                &s.key,
                s.canonical.as_deref(),
                s.count,
                s.threshold,
            ) else {
                continue;
            };
            let token_refs: Vec<&str> = meta.tokens.iter().map(String::as_str).collect();
            let seen_apps = collect_seen_apps(records, &token_refs);
            almost.push((
                s.count,
                ReviewItem {
                    id: format!("staged:{}:{}", s.queue, s.key),
                    section: ReviewSection::Almost,
                    type_badge: meta.type_badge,
                    kind_noun: meta.kind_noun,
                    title: meta.title,
                    item_label: meta.item_label,
                    lang_label: language_name(&meta.lang),
                    problem: meta.problem,
                    source: meta.source,
                    detail: meta.detail,
                    warning: None,
                    added_ms: 0,
                    last_hit_ms: None,
                    hits: s.count,
                    strikes: 0,
                    seen_apps,
                    keep_effect: meta.keep_effect,
                    remove_effect: meta.remove_effect,
                    keep_op: StoreOp::PromoteStaged {
                        queue: s.queue.to_string(),
                        key: s.key.clone(),
                        canonical: s.canonical.clone(),
                    },
                    remove_op: StoreOp::DismissStaged {
                        queue: s.queue.to_string(),
                        key: s.key.clone(),
                    },
                },
            ));
        }
        almost.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
        almost.truncate(MAX_ALMOST);
        let shown = almost.len();
        items.extend(almost.into_iter().map(|(_, item)| item));
        self.review_hidden = staged.len().saturating_sub(shown + already);
        items.sort_by_key(|i| match i.section {
            ReviewSection::Learned => 0,
            ReviewSection::Blocked => 1,
            ReviewSection::Almost => 2,
        });
        let ids: BTreeSet<String> = items.iter().map(|i| i.id.clone()).collect();
        self.review_marked.retain(|id| ids.contains(id));
        self.review = items;
        clamp(&mut self.review_idx, self.review.len());
        clamp(&mut self.changes_idx, self.history.len());
    }

    fn build_dictionary(&mut self, rules: &[RuleEntry]) {
        let mut rows = Vec::new();
        let now = current_epoch_ms();
        for r in rules {
            let last = r
                .last_hit_ms
                .map(|t| format!("{} ago", format_age_ago(now, t)))
                .unwrap_or_else(|| "never".to_string());
            let (section, cols, label) = match r.kind {
                RuleKind::Word => (
                    DictSection::Words,
                    vec![
                        r.b.clone(),
                        r.lang.as_str().to_string(),
                        r.added_by().to_string(),
                        r.hits.to_string(),
                        last,
                    ],
                    format!("word \"{}\"", r.b),
                ),
                RuleKind::Fix => (
                    DictSection::Fixes,
                    vec![
                        r.a.clone(),
                        r.b.clone(),
                        String::new(),
                        r.lang.as_str().to_string(),
                        r.added_by().to_string(),
                        r.hits.to_string(),
                    ],
                    format!("fix {} → {}", r.a, r.b),
                ),
                RuleKind::Context => (
                    DictSection::Fixes,
                    vec![
                        r.a.clone(),
                        r.b.clone(),
                        r.prev.clone(),
                        "en".to_string(),
                        r.added_by().to_string(),
                        r.hits.to_string(),
                    ],
                    format!("fix {} {} → {}", r.prev, r.a, r.b),
                ),
                RuleKind::Snippet => (
                    DictSection::Snippets,
                    vec![r.a.clone(), r.b.clone()],
                    format!("snippet {}", r.a),
                ),
                RuleKind::Block => (
                    DictSection::NeverFix,
                    vec![r.a.clone(), r.b.clone(), r.added_by().to_string()],
                    format!("block {} → {}", r.a, r.b),
                ),
                RuleKind::Passthrough => (
                    DictSection::TelexKeep,
                    vec![r.a.clone(), r.added_by().to_string()],
                    format!("Telex exception \"{}\"", r.a),
                ),
            };
            let haystack = cols.join(" ").to_lowercase();
            rows.push(DictRow {
                section,
                cols,
                haystack,
                label,
                delete_op: r.delete_op(),
            });
        }
        rows.sort_by(|a, b| a.cols[0].to_lowercase().cmp(&b.cols[0].to_lowercase()));
        let mut counts = [0usize; 5];
        for r in &rows {
            counts[r.section.index()] += 1;
        }
        self.dict_counts = counts;
        self.dict = rows;
        self.rebuild_dict_view();
    }

    pub(super) fn rebuild_dict_view(&mut self) {
        let needle = self.dict_filter.trim().to_lowercase();
        self.dict_view = self
            .dict
            .iter()
            .enumerate()
            .filter(|(_, r)| r.section == self.dict_section)
            .filter(|(_, r)| needle.is_empty() || r.haystack.contains(&needle))
            .map(|(i, _)| i)
            .collect();
        clamp(&mut self.dict_idx, self.dict_view.len());
    }

    fn build_settings(
        &mut self,
        store: &RuleStore,
        records: &[smartkey_core::LoggedDiagnosticRecord],
    ) {
        self.settings_values = store
            .list_settings()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();

        let ap = &store.compiled().app_profiles;
        let mut seen: Vec<(String, u64)> = Vec::new();
        for r in records.iter().rev() {
            let mut names: Vec<&str> = Vec::new();
            if let Some(a) = r.app.as_deref() {
                names.push(a);
            }
            if let DiagnosticEvent::AppBypass { app, .. } = &r.event {
                names.push(app.as_str());
            }
            for n in names {
                let norm = normalize_program_id(n);
                if !norm.is_empty() && !seen.iter().any(|(s, _)| *s == norm) {
                    seen.push((norm, r.ts_ms));
                }
            }
        }
        seen.truncate(MAX_SEEN_APPS);
        for set in [&ap.bypass, &ap.browser, &ap.chat, &ap.prose] {
            for a in set {
                if !seen.iter().any(|(s, _)| s == a) {
                    seen.push((a.clone(), 0));
                }
            }
        }

        let mut rows = vec![SettingsRow::Header("Correction")];
        rows.extend(CORRECTION_SPECS.iter().copied().map(SettingsRow::Setting));
        rows.push(SettingsRow::Header("Undo"));
        rows.extend(UNDO_SPECS.iter().copied().map(SettingsRow::Setting));
        rows.push(SettingsRow::Header("Vietnamese"));
        rows.extend(VIETNAMESE_SPECS.iter().copied().map(SettingsRow::Setting));
        rows.push(SettingsRow::Header("Apps you typed in recently"));
        for (app, _) in seen {
            let (profile, reason) = ap.classify_app(&app);
            rows.push(SettingsRow::App {
                chosen: ap.classify_explicit_override(&app),
                app,
                profile,
                reason,
            });
        }
        rows.push(SettingsRow::Setting(UNLISTED_SPEC));
        rows.push(SettingsRow::Header("Logs and diagnostics"));
        rows.extend(DIAGNOSTIC_SPECS.iter().copied().map(SettingsRow::Setting));
        self.settings_rows = rows;
        clamp(&mut self.settings_idx, self.settings_rows.len());
        if matches!(
            self.settings_rows.get(self.settings_idx),
            Some(SettingsRow::Header(_))
        ) {
            self.move_settings(1);
        }
    }

    // -- status ----------------------------------------------------------------

    pub(super) fn set_status(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: false,
        });
    }

    pub(super) fn set_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: true,
        });
    }

    // -- mutations & history ---------------------------------------------------

    fn apply_ops(&mut self, ops: &[StoreOp]) -> Result<Vec<StoreOp>, (String, Vec<StoreOp>)> {
        if self.parse_locked {
            return Err((
                "rules.toml has a syntax error, so changes are blocked until it is fixed"
                    .to_string(),
                Vec::new(),
            ));
        }
        let mut store = match load_rule_store_mut(&self.config_dir, Preset::Balanced) {
            Ok(s) => s,
            Err(err) => return Err((cli_error_text(&err).to_string(), Vec::new())),
        };
        if store.is_parse_locked() {
            return Err((
                "rules.toml has a syntax error, so changes are blocked until it is fixed"
                    .to_string(),
                Vec::new(),
            ));
        }
        let mut inverses = Vec::new();
        for op in ops {
            match op.apply(&mut store) {
                Ok(Some(inv)) => inverses.push(inv),
                Ok(None) => {}
                Err(e) => {
                    inverses.reverse();
                    return Err((e, inverses));
                }
            }
        }
        inverses.reverse();
        Ok(inverses)
    }

    pub(super) fn mutate(&mut self, kind: ChangeKind, ops: Vec<StoreOp>, msg: String) {
        if ops.is_empty() {
            return;
        }
        let result = self.apply_ops(&ops);
        let signaled = (self.notify_reload)();
        self.refresh();
        let suffix = if signaled {
            ""
        } else {
            " (saved; Fcitx5 is not running)"
        };
        match result {
            Ok(inverses) => {
                if !inverses.is_empty() {
                    let id = self.record_change(kind, msg.clone(), ops, inverses);
                    self.undo_stack.push(id);
                    self.redo_stack.clear();
                    self.set_status(format!("{msg}{suffix} · u undo"));
                } else {
                    self.set_status(format!("{msg}{suffix}"));
                }
            }
            Err((err, partial)) => {
                if !partial.is_empty() {
                    let id = self.record_change(kind, msg.clone(), Vec::new(), partial);
                    self.undo_stack.push(id);
                }
                self.set_error(err);
            }
        }
    }

    fn record_change(
        &mut self,
        kind: ChangeKind,
        summary: String,
        forward_ops: Vec<StoreOp>,
        inverse_ops: Vec<StoreOp>,
    ) -> u64 {
        let id = self.next_change_id;
        self.next_change_id += 1;
        if self.history.len() >= MAX_HISTORY {
            self.history.remove(0);
        }
        self.history.push(ChangeEntry {
            id,
            kind,
            summary,
            screen: self.screen,
            ts_ms: current_epoch_ms(),
            forward_ops,
            inverse_ops,
            reverted: false,
        });
        clamp(&mut self.changes_idx, self.history.len());
        id
    }

    pub(super) fn undo_last(&mut self) {
        while let Some(id) = self.undo_stack.pop() {
            let Some(pos) = self.history.iter().position(|e| e.id == id) else {
                continue;
            };
            if self.history[pos].reverted || self.history[pos].inverse_ops.is_empty() {
                continue;
            }
            let ops = self.history[pos].inverse_ops.clone();
            let label = self.history[pos].summary.clone();
            let result = self.apply_ops(&ops);
            (self.notify_reload)();
            self.refresh();
            match result {
                Ok(_) => {
                    if let Some(entry) = self.history.iter_mut().find(|e| e.id == id) {
                        entry.reverted = true;
                    }
                    self.redo_stack.push(id);
                    self.set_status(format!("Undid: {label} · Ctrl+r redo"));
                }
                Err((err, _)) => self.set_error(format!("Undo failed: {err}")),
            }
            return;
        }
        self.set_status("Nothing to undo");
    }

    pub(super) fn redo_last(&mut self) {
        while let Some(id) = self.redo_stack.pop() {
            let Some(pos) = self.history.iter().position(|e| e.id == id) else {
                continue;
            };
            if !self.history[pos].reverted || self.history[pos].forward_ops.is_empty() {
                continue;
            }
            let ops = self.history[pos].forward_ops.clone();
            let label = self.history[pos].summary.clone();
            let result = self.apply_ops(&ops);
            (self.notify_reload)();
            self.refresh();
            match result {
                Ok(_) => {
                    if let Some(entry) = self.history.iter_mut().find(|e| e.id == id) {
                        entry.reverted = false;
                    }
                    self.undo_stack.push(id);
                    self.set_status(format!("Redid: {label} · u undo"));
                }
                Err((err, _)) => self.set_error(format!("Redo failed: {err}")),
            }
            return;
        }
        self.set_status("Nothing to redo");
    }

    pub(super) fn toggle_selected_change(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let hist_idx = self.history.len() - 1 - self.changes_idx.min(self.history.len() - 1);
        let entry = self.history[hist_idx].clone();
        let id = entry.id;
        if !entry.reverted {
            if entry.inverse_ops.is_empty() {
                self.set_error("This change cannot be undone");
                return;
            }
            let result = self.apply_ops(&entry.inverse_ops);
            (self.notify_reload)();
            self.refresh();
            match result {
                Ok(_) => {
                    if let Some(e) = self.history.iter_mut().find(|e| e.id == id) {
                        e.reverted = true;
                    }
                    self.undo_stack.retain(|&x| x != id);
                    self.redo_stack.push(id);
                    self.set_status(format!("Reverted: {}", entry.summary));
                }
                Err((err, _)) => self.set_error(format!("Revert failed: {err}")),
            }
        } else {
            if entry.forward_ops.is_empty() {
                self.set_error("This change cannot be reapplied");
                return;
            }
            let result = self.apply_ops(&entry.forward_ops);
            (self.notify_reload)();
            self.refresh();
            match result {
                Ok(_) => {
                    if let Some(e) = self.history.iter_mut().find(|e| e.id == id) {
                        e.reverted = false;
                    }
                    self.redo_stack.retain(|&x| x != id);
                    self.undo_stack.push(id);
                    self.set_status(format!("Reapplied: {}", entry.summary));
                }
                Err((err, _)) => self.set_error(format!("Reapply failed: {err}")),
            }
        }
    }

    pub(super) fn active_change_count(&self) -> usize {
        self.history.iter().filter(|e| !e.reverted).count()
    }

    pub(super) fn switch_screen(&mut self, screen: Screen) {
        if self.screen != screen {
            self.screen = screen;
            self.visual_anchor = None;
            self.pending_g = false;
        }
        self.focus_sidebar = false;
    }

    // -- keys ------------------------------------------------------------------

    /// Handles one key press. Returns `true` when the app should exit.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.modal.is_some() {
            self.pending_g = false;
            self.handle_modal_key(key);
            return false;
        }
        if self.filter_editing {
            self.pending_g = false;
            self.handle_filter_key(key);
            return false;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            self.pending_g = false;
            match key.code {
                KeyCode::Char('r') => {
                    self.redo_last();
                    return false;
                }
                KeyCode::Char('h') => {
                    self.visual_anchor = None;
                    self.focus_sidebar = true;
                    return false;
                }
                KeyCode::Char('l') => {
                    self.focus_sidebar = false;
                    return false;
                }
                KeyCode::Char('d') => {
                    self.move_active(10);
                    return false;
                }
                KeyCode::Char('u') => {
                    self.move_active(-10);
                    return false;
                }
                KeyCode::Char('f') => {
                    self.move_active(20);
                    return false;
                }
                KeyCode::Char('b') => {
                    self.move_active(-20);
                    return false;
                }
                _ => {}
            }
        }

        if key.code == KeyCode::Char('g') {
            if self.pending_g {
                self.pending_g = false;
                self.jump_edge(false);
            } else {
                self.pending_g = true;
            }
            return false;
        }
        self.pending_g = false;

        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char(c @ '1'..='4') => {
                self.switch_screen(Screen::from_index((c as u8 - b'1') as usize));
                return false;
            }
            KeyCode::Tab => {
                self.switch_screen(Screen::from_index(self.screen.index() + 1));
                return false;
            }
            KeyCode::BackTab => {
                self.switch_screen(Screen::from_index(self.screen.index() + 3));
                return false;
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                return false;
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.set_status("Reloaded from disk");
                return false;
            }
            KeyCode::Char('<') => {
                self.adjust_split(-1);
                return false;
            }
            KeyCode::Char('>') => {
                self.adjust_split(1);
                return false;
            }
            KeyCode::Char('+') => {
                self.layout.review_inspect_pct = self
                    .layout
                    .review_inspect_pct
                    .saturating_add(5)
                    .clamp(INSPECT_PCT_MIN, INSPECT_PCT_MAX);
                return false;
            }
            KeyCode::Char('-') => {
                self.layout.review_inspect_pct = self
                    .layout
                    .review_inspect_pct
                    .saturating_sub(5)
                    .clamp(INSPECT_PCT_MIN, INSPECT_PCT_MAX);
                return false;
            }
            KeyCode::Char('=') => {
                self.layout = LayoutState::default();
                self.set_status("Reset layout");
                return false;
            }
            KeyCode::Char('z') => {
                self.layout.zoomed = !self.layout.zoomed;
                return false;
            }
            _ => {}
        }

        if self.focus_sidebar {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    self.screen = Screen::from_index(self.screen.index() + 1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.screen = Screen::from_index(self.screen.index() + 3)
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') | KeyCode::Esc => {
                    self.focus_sidebar = false
                }
                KeyCode::Char('G') => self.screen = Screen::Settings,
                KeyCode::Char('u') => self.undo_last(),
                _ => {}
            }
            return false;
        }

        if key.code == KeyCode::Esc {
            if self.visual_anchor.take().is_some() {
                return false;
            }
            if !self.has_pending_filter() {
                self.focus_sidebar = true;
                return false;
            }
        }

        if key.code == KeyCode::Char('u')
            && !(self.screen == Screen::Review && self.review_tab == ReviewTab::Changes)
        {
            self.undo_last();
            return false;
        }

        match self.screen {
            Screen::Activity => self.handle_activity_key(key),
            Screen::Review => self.handle_review_key(key),
            Screen::Dictionary => self.handle_dictionary_key(key),
            Screen::Settings => self.handle_settings_key(key),
        }
        false
    }

    fn adjust_split(&mut self, dir: i16) {
        if self.focus_sidebar {
            let next = (self.layout.sidebar_width as i16 + dir * 2)
                .clamp(SIDEBAR_WIDTH_MIN as i16, SIDEBAR_WIDTH_MAX as i16);
            self.layout.sidebar_width = next as u16;
        } else {
            let next = (self.layout.split_pct as i16 + dir * 4)
                .clamp(SPLIT_PCT_MIN as i16, SPLIT_PCT_MAX as i16);
            self.layout.split_pct = next as u16;
        }
    }

    fn move_active(&mut self, delta: isize) {
        if self.focus_sidebar {
            let step_idx = if delta >= 0 { 1 } else { 3 };
            self.screen = Screen::from_index(self.screen.index() + step_idx);
            return;
        }
        match self.screen {
            Screen::Activity => step(&mut self.activity_idx, delta, self.activity_view.len()),
            Screen::Review => match self.review_tab {
                ReviewTab::Queue => step(&mut self.review_idx, delta, self.review.len()),
                ReviewTab::Changes => step(&mut self.changes_idx, delta, self.history.len()),
            },
            Screen::Dictionary => step(&mut self.dict_idx, delta, self.dict_view.len()),
            Screen::Settings => self.move_settings(delta),
        }
    }

    fn jump_edge(&mut self, bottom: bool) {
        if self.focus_sidebar {
            self.screen = if bottom {
                Screen::Settings
            } else {
                Screen::Activity
            };
            return;
        }
        match self.screen {
            Screen::Activity => {
                self.activity_idx = if bottom {
                    self.activity_view.len().saturating_sub(1)
                } else {
                    0
                };
            }
            Screen::Review => match self.review_tab {
                ReviewTab::Queue => {
                    self.review_idx = if bottom {
                        self.review.len().saturating_sub(1)
                    } else {
                        0
                    };
                }
                ReviewTab::Changes => {
                    self.changes_idx = if bottom {
                        self.history.len().saturating_sub(1)
                    } else {
                        0
                    };
                }
            },
            Screen::Dictionary => {
                self.dict_idx = if bottom {
                    self.dict_view.len().saturating_sub(1)
                } else {
                    0
                };
            }
            Screen::Settings => {
                if bottom {
                    self.settings_idx = self.settings_rows.len().saturating_sub(1);
                    if matches!(
                        self.settings_rows.get(self.settings_idx),
                        Some(SettingsRow::Header(_))
                    ) {
                        self.move_settings(-1);
                    }
                } else {
                    self.settings_idx = 0;
                    self.move_settings(1);
                }
            }
        }
    }

    fn has_pending_filter(&self) -> bool {
        match self.screen {
            Screen::Activity => !self.activity_search.is_empty(),
            Screen::Dictionary => !self.dict_filter.is_empty(),
            _ => false,
        }
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        let on_activity = self.screen == Screen::Activity;
        match key.code {
            KeyCode::Enter => self.filter_editing = false,
            KeyCode::Esc => {
                self.filter_editing = false;
                if on_activity {
                    self.activity_search.clear();
                } else {
                    self.dict_filter.clear();
                }
            }
            KeyCode::Backspace => {
                if on_activity {
                    self.activity_search.pop();
                } else {
                    self.dict_filter.pop();
                }
            }
            KeyCode::Char(c) => {
                if on_activity {
                    self.activity_search.push(c);
                } else {
                    self.dict_filter.push(c);
                }
            }
            _ => {}
        }
        if on_activity {
            self.rebuild_activity_view();
        } else {
            self.rebuild_dict_view();
        }
    }

    pub(super) fn selected_activity(&self) -> Option<&ActivityRow> {
        self.activity_view
            .get(self.activity_idx)
            .and_then(|&i| self.activity.get(i))
    }

    fn handle_activity_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c @ ('b' | 'a' | 'w' | 'p' | 'v')) => {
                let action = self
                    .selected_activity()
                    .and_then(|row| activity_actions(row).into_iter().find(|i| i.key == c))
                    .map(|i| i.action);
                if let Some(action) = action {
                    self.run_action(action);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                step(&mut self.activity_idx, 1, self.activity_view.len())
            }
            KeyCode::Up | KeyCode::Char('k') => {
                step(&mut self.activity_idx, -1, self.activity_view.len())
            }
            KeyCode::Char('G') => self.jump_edge(true),
            KeyCode::PageDown => step(&mut self.activity_idx, 10, self.activity_view.len()),
            KeyCode::PageUp => step(&mut self.activity_idx, -10, self.activity_view.len()),
            KeyCode::Char('/') => self.filter_editing = true,
            KeyCode::Esc if !self.activity_search.is_empty() => {
                self.activity_search.clear();
                self.rebuild_activity_view();
            }
            KeyCode::Char('f') => {
                self.activity_filter = self.activity_filter.next();
                self.rebuild_activity_view();
                self.set_status(format!("Showing {}", self.activity_filter.label()));
            }
            KeyCode::Char('e') => {
                let next = !self.enabled;
                self.mutate(
                    ChangeKind::Changed,
                    vec![StoreOp::SetSetting {
                        key: "settings.enabled".to_string(),
                        value: next.to_string(),
                    }],
                    if next {
                        "SmartKey on".to_string()
                    } else {
                        "SmartKey off".to_string()
                    },
                );
            }
            KeyCode::Char('x') => {
                let input = self
                    .selected_activity()
                    .and_then(|r| explain_target(&r.event))
                    .map(|(prev, tok)| match prev {
                        Some(p) => format!("{p} {tok}"),
                        None => tok,
                    })
                    .unwrap_or_default();
                self.modal = Some(Modal::ExplainInput { input });
            }
            KeyCode::Enter => {
                if let Some(row) = self.selected_activity() {
                    let items = activity_actions(row);
                    let title = if row.change.is_empty() {
                        row.why.clone()
                    } else {
                        row.change.clone()
                    };
                    self.modal = Some(Modal::Actions {
                        title,
                        items,
                        selected: 0,
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_review_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Char('h')
            | KeyCode::Char('l')
            | KeyCode::Char('[')
            | KeyCode::Char(']') => {
                self.review_tab = match self.review_tab {
                    ReviewTab::Queue => ReviewTab::Changes,
                    ReviewTab::Changes => ReviewTab::Queue,
                };
                self.visual_anchor = None;
                return;
            }
            _ => {}
        }

        if self.review_tab == ReviewTab::Changes {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    step(&mut self.changes_idx, 1, self.history.len())
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    step(&mut self.changes_idx, -1, self.history.len())
                }
                KeyCode::Char('G') => self.jump_edge(true),
                KeyCode::PageDown => step(&mut self.changes_idx, 10, self.history.len()),
                KeyCode::PageUp => step(&mut self.changes_idx, -10, self.history.len()),
                KeyCode::Char('u') | KeyCode::Enter => self.toggle_selected_change(),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Down | KeyCode::Char('j') => step(&mut self.review_idx, 1, self.review.len()),
            KeyCode::Up | KeyCode::Char('k') => step(&mut self.review_idx, -1, self.review.len()),
            KeyCode::Char('G') => self.jump_edge(true),
            KeyCode::PageDown => step(&mut self.review_idx, 10, self.review.len()),
            KeyCode::PageUp => step(&mut self.review_idx, -10, self.review.len()),
            KeyCode::Char('{') => self.jump_review_section(-1),
            KeyCode::Char('}') => self.jump_review_section(1),
            KeyCode::Char('v') | KeyCode::Char('V') => {
                if self.visual_anchor.is_some() {
                    self.visual_anchor = None;
                } else if !self.review.is_empty() {
                    self.visual_anchor = Some(self.review_idx);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(anchor) = self.visual_anchor.take() {
                    let indices = range_indices(Some(anchor), self.review_idx);
                    let mut n = 0usize;
                    for i in indices {
                        if let Some(item) = self.review.get(i) {
                            if self.review_marked.insert(item.id.clone()) {
                                n += 1;
                            }
                        }
                    }
                    self.set_status(format!("Marked {n} item(s)"));
                } else if let Some(item) = self.review.get(self.review_idx) {
                    let id = item.id.clone();
                    if !self.review_marked.remove(&id) {
                        self.review_marked.insert(id);
                    }
                    step(&mut self.review_idx, 1, self.review.len());
                }
            }
            KeyCode::Char('a') => {
                if let Some(anchor) = self.visual_anchor.take() {
                    let items: Vec<ReviewItem> = range_indices(Some(anchor), self.review_idx)
                        .into_iter()
                        .filter_map(|i| self.review.get(i).cloned())
                        .collect();
                    let n = items.len();
                    let ops: Vec<StoreOp> = items.into_iter().map(|i| i.keep_op).collect();
                    self.mutate(ChangeKind::Kept, ops, format!("Kept {n} items"));
                } else if let Some(item) = self.review.get(self.review_idx).cloned() {
                    self.mutate(
                        ChangeKind::Kept,
                        vec![item.keep_op],
                        format!("Kept {}", item.title),
                    );
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(anchor) = self.visual_anchor.take() {
                    let items: Vec<ReviewItem> = range_indices(Some(anchor), self.review_idx)
                        .into_iter()
                        .filter_map(|i| self.review.get(i).cloned())
                        .collect();
                    let n = items.len();
                    if n == 1 {
                        let item = items.into_iter().next().unwrap();
                        self.mutate(
                            ChangeKind::Removed,
                            vec![item.remove_op],
                            format!("Removed {}", item.title),
                        );
                    } else if n > 1 {
                        let ops: Vec<StoreOp> = items.into_iter().map(|i| i.remove_op).collect();
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Remove {n} selected items?"),
                            kind: ChangeKind::Removed,
                            ops,
                            msg: format!("Removed {n} items"),
                            clear_marks: false,
                        });
                    }
                } else if let Some(item) = self.review.get(self.review_idx).cloned() {
                    self.mutate(
                        ChangeKind::Removed,
                        vec![item.remove_op],
                        format!("Removed {}", item.title),
                    );
                }
            }
            KeyCode::Char('A') => {
                self.visual_anchor = None;
                let ops: Vec<StoreOp> = self.marked_review().map(|i| i.keep_op.clone()).collect();
                if ops.is_empty() {
                    self.set_status("Mark items with Space or v first");
                } else {
                    let n = ops.len();
                    self.review_marked.clear();
                    self.mutate(ChangeKind::Kept, ops, format!("Kept {n} items"));
                }
            }
            KeyCode::Char('D') => {
                self.visual_anchor = None;
                let ops: Vec<StoreOp> = self.marked_review().map(|i| i.remove_op.clone()).collect();
                if ops.is_empty() {
                    self.set_status("Mark items with Space or v first");
                } else {
                    let n = ops.len();
                    self.modal = Some(Modal::Confirm {
                        prompt: format!("Remove {n} marked items?"),
                        kind: ChangeKind::Removed,
                        ops,
                        msg: format!("Removed {n} items"),
                        clear_marks: true,
                    });
                }
            }
            _ => {}
        }
    }

    fn jump_review_section(&mut self, dir: isize) {
        if self.review.is_empty() {
            return;
        }
        let cur = self.review_idx.min(self.review.len() - 1);
        let cur_sec = self.review[cur].section;
        if dir > 0 {
            if let Some(pos) =
                (cur + 1..self.review.len()).find(|&i| self.review[i].section != cur_sec)
            {
                self.review_idx = pos;
            } else {
                self.review_idx = self.review.len() - 1;
            }
        } else {
            let section_start = (0..=cur)
                .rev()
                .take_while(|&i| self.review[i].section == cur_sec)
                .last()
                .unwrap_or(0);
            if cur > section_start {
                self.review_idx = section_start;
            } else if section_start > 0 {
                let prev_sec = self.review[section_start - 1].section;
                self.review_idx = (0..section_start)
                    .rev()
                    .take_while(|&i| self.review[i].section == prev_sec)
                    .last()
                    .unwrap_or(0);
            }
        }
    }

    fn marked_review(&self) -> impl Iterator<Item = &ReviewItem> {
        self.review
            .iter()
            .filter(|i| self.review_marked.contains(&i.id))
    }

    fn handle_dictionary_key(&mut self, key: KeyEvent) {
        let switch = |app: &mut Self, delta: isize| {
            let n = DictSection::ALL.len() as isize;
            let i = (app.dict_section.index() as isize + delta).rem_euclid(n) as usize;
            app.dict_section = DictSection::ALL[i];
            app.dict_idx = 0;
            app.visual_anchor = None;
            app.rebuild_dict_view();
        };
        match key.code {
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(']') => switch(self, 1),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('[') => switch(self, -1),
            KeyCode::Down | KeyCode::Char('j') => step(&mut self.dict_idx, 1, self.dict_view.len()),
            KeyCode::Up | KeyCode::Char('k') => step(&mut self.dict_idx, -1, self.dict_view.len()),
            KeyCode::Char('G') => self.jump_edge(true),
            KeyCode::PageDown => step(&mut self.dict_idx, 10, self.dict_view.len()),
            KeyCode::PageUp => step(&mut self.dict_idx, -10, self.dict_view.len()),
            KeyCode::Char('v') | KeyCode::Char('V') => {
                if self.visual_anchor.is_some() {
                    self.visual_anchor = None;
                } else if !self.dict_view.is_empty() {
                    self.visual_anchor = Some(self.dict_idx);
                }
            }
            KeyCode::Char('/') => {
                self.visual_anchor = None;
                self.filter_editing = true;
            }
            KeyCode::Esc if !self.dict_filter.is_empty() => {
                self.dict_filter.clear();
                self.rebuild_dict_view();
            }
            KeyCode::Char('n') => {
                self.visual_anchor = None;
                self.modal = Some(Modal::Form(Form::new(self.dict_section)));
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(anchor) = self.visual_anchor.take() {
                    let picked: Vec<DictRow> = range_indices(Some(anchor), self.dict_idx)
                        .into_iter()
                        .filter_map(|vi| {
                            self.dict_view
                                .get(vi)
                                .and_then(|&di| self.dict.get(di))
                                .cloned()
                        })
                        .collect();
                    let n = picked.len();
                    if n == 1 {
                        let row = picked.into_iter().next().unwrap();
                        self.mutate(
                            ChangeKind::Removed,
                            vec![row.delete_op],
                            format!("Removed {}", row.label),
                        );
                    } else if n > 1 {
                        let ops: Vec<StoreOp> = picked.into_iter().map(|r| r.delete_op).collect();
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Remove {n} selected entries?"),
                            kind: ChangeKind::Removed,
                            ops,
                            msg: format!("Removed {n} entries"),
                            clear_marks: false,
                        });
                    }
                } else if let Some(row) = self
                    .dict_view
                    .get(self.dict_idx)
                    .and_then(|&i| self.dict.get(i))
                    .cloned()
                {
                    self.mutate(
                        ChangeKind::Removed,
                        vec![row.delete_op],
                        format!("Removed {}", row.label),
                    );
                }
            }
            _ => {}
        }
    }

    pub(super) fn move_settings(&mut self, delta: isize) {
        let len = self.settings_rows.len();
        if len == 0 {
            return;
        }
        let mut i = self.settings_idx as isize;
        loop {
            let next = i + delta;
            if next < 0 || next >= len as isize {
                break;
            }
            i = next;
            if !matches!(self.settings_rows[i as usize], SettingsRow::Header(_)) {
                self.settings_idx = i as usize;
                return;
            }
        }
        if matches!(
            self.settings_rows.get(self.settings_idx),
            Some(SettingsRow::Header(_))
        ) && delta != 0
        {
            self.move_settings(-delta.signum());
        }
    }

    fn jump_settings_section(&mut self, dir: isize) {
        if self.settings_rows.is_empty() {
            return;
        }
        let cur = self.settings_idx.min(self.settings_rows.len() - 1);
        if dir > 0 {
            if let Some(h) = (cur + 1..self.settings_rows.len())
                .find(|&i| matches!(self.settings_rows[i], SettingsRow::Header(_)))
            {
                if let Some(first) = (h + 1..self.settings_rows.len())
                    .find(|&i| !matches!(self.settings_rows[i], SettingsRow::Header(_)))
                {
                    self.settings_idx = first;
                }
            }
        } else {
            let cur_header = (0..=cur)
                .rev()
                .find(|&i| matches!(self.settings_rows[i], SettingsRow::Header(_)))
                .unwrap_or(0);
            let cur_first = (cur_header + 1..self.settings_rows.len())
                .find(|&i| !matches!(self.settings_rows[i], SettingsRow::Header(_)))
                .unwrap_or(cur);
            if cur > cur_first {
                self.settings_idx = cur_first;
            } else if cur_header > 0 {
                let prev_header = (0..cur_header)
                    .rev()
                    .find(|&i| matches!(self.settings_rows[i], SettingsRow::Header(_)))
                    .unwrap_or(0);
                if let Some(first) = (prev_header + 1..cur_header)
                    .find(|&i| !matches!(self.settings_rows[i], SettingsRow::Header(_)))
                {
                    self.settings_idx = first;
                }
            }
        }
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_settings(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_settings(-1),
            KeyCode::Char('G') => self.jump_edge(true),
            KeyCode::PageDown => self.move_settings(10),
            KeyCode::PageUp => self.move_settings(-10),
            KeyCode::Char('{') => self.jump_settings_section(-1),
            KeyCode::Char('}') => self.jump_settings_section(1),
            KeyCode::Right | KeyCode::Char('l') => self.change_setting(1),
            KeyCode::Left | KeyCode::Char('h') => self.change_setting(-1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                match self.settings_rows.get(self.settings_idx).cloned() {
                    Some(SettingsRow::Setting(spec)) => match spec.control {
                        super::types::Control::Number { .. } => {
                            let current = self
                                .settings_values
                                .get(spec.key)
                                .cloned()
                                .unwrap_or_default();
                            self.modal = Some(Modal::EditNumber {
                                spec,
                                current,
                                input: String::new(),
                            });
                        }
                        _ => self.change_setting(1),
                    },
                    Some(SettingsRow::App { app, chosen, .. }) => {
                        let selected = APP_CHOICES
                            .iter()
                            .position(|(p, _, _)| *p == chosen)
                            .unwrap_or(0);
                        self.modal = Some(Modal::AppPicker { app, selected });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn change_setting(&mut self, delta: i32) {
        use super::theme::{display_setting, format_number};
        use super::types::Control;
        match self.settings_rows.get(self.settings_idx).cloned() {
            Some(SettingsRow::Setting(spec)) => {
                let current = self
                    .settings_values
                    .get(spec.key)
                    .cloned()
                    .unwrap_or_default();
                let next = match spec.control {
                    Control::Toggle => (current != "true").to_string(),
                    Control::Choice(opts) => {
                        let pos = opts.iter().position(|(v, _)| *v == current).unwrap_or(0) as i32;
                        let n = opts.len() as i32;
                        opts[(pos + delta).rem_euclid(n) as usize].0.to_string()
                    }
                    Control::Number {
                        step,
                        min,
                        max,
                        decimals,
                        ..
                    } => {
                        let cur: f64 = current.parse().unwrap_or(min);
                        let v = (cur + step * f64::from(delta)).clamp(min, max);
                        format_number(v, decimals)
                    }
                };
                let shown = display_setting(&spec, &next);
                self.mutate(
                    ChangeKind::Changed,
                    vec![StoreOp::SetSetting {
                        key: spec.key.to_string(),
                        value: next,
                    }],
                    format!("{}: {shown}", spec.label),
                );
            }
            Some(SettingsRow::App { app, chosen, .. }) => {
                let pos = APP_CHOICES
                    .iter()
                    .position(|(p, _, _)| *p == chosen)
                    .unwrap_or(0) as i32;
                let next = APP_CHOICES[(pos + delta).rem_euclid(APP_CHOICES.len() as i32) as usize];
                self.mutate(
                    ChangeKind::Changed,
                    vec![StoreOp::SetAppProfile {
                        app: app.clone(),
                        profile: next.0,
                    }],
                    format!("{app}: {}", next.1.to_lowercase()),
                );
            }
            _ => {}
        }
    }

    pub(super) fn run_action(&mut self, action: RowAction) {
        match action {
            RowAction::Ops { kind, ops, msg } => self.mutate(kind, ops, msg),
            RowAction::Explain { lang, prev, token } => {
                let lines = self.explain(&lang, prev.as_deref(), &token);
                self.modal = Some(Modal::Info {
                    title: format!(
                        "Why: {}",
                        match &prev {
                            Some(p) => format!("{p} {token}"),
                            None => token.clone(),
                        }
                    ),
                    lines,
                });
            }
            RowAction::PickApp { app } => {
                let app = normalize_program_id(&app);
                let chosen = load_rule_store_readonly(&self.config_dir, Preset::Balanced)
                    .ok()
                    .and_then(|(s, _)| s.compiled().app_profiles.classify_explicit_override(&app));
                let selected = APP_CHOICES
                    .iter()
                    .position(|(p, _, _)| *p == chosen)
                    .unwrap_or(0);
                self.modal = Some(Modal::AppPicker { app, selected });
            }
            RowAction::GoReview => self.switch_screen(Screen::Review),
        }
    }

    pub(super) fn explain(&self, lang: &LangId, prev: Option<&str>, token: &str) -> Vec<String> {
        let (mut store, _) = match load_rule_store_readonly(&self.config_dir, Preset::Balanced) {
            Ok(pair) => pair,
            Err(err) => return vec![format!("Could not load settings: {}", cli_error_text(&err))],
        };
        store.set_active_lang(lang.clone());
        let engine = SmartKeyEngine::new_auto(store);
        let ctx = EvalContext {
            prev_word: prev,
            ..EvalContext::default()
        };
        let exp = engine.explain_token(token, &ctx);
        let aggr = engine.store().scorer_config().aggressiveness;
        let (min_score, min_margin) = LatinScorer::thresholds(aggr);

        let mut lines = Vec::new();
        match &exp.outcome {
            EvalOutcome::Autocorrect {
                replacement,
                kind,
                score,
                margin,
            } => {
                lines.push(format!("SmartKey changes “{token}” → “{replacement}”."));
                lines.push(format!(
                    "Reason: {} · score {score:.2} (needs {min_score:.2}) · lead {margin:.2} (needs {min_margin:.2})",
                    super::theme::kind_label(*kind)
                ));
            }
            EvalOutcome::Unchanged { reason, .. } => {
                lines.push(format!("SmartKey leaves “{token}” as typed."));
                lines.push(format!(
                    "Reason: {} · a fix needs score {min_score:.2} and lead {min_margin:.2}",
                    super::theme::kind_label(*reason)
                ));
            }
        }
        if !exp.top_candidates.is_empty() {
            lines.push(String::new());
            lines.push("Best guesses:".to_string());
            for c in exp.top_candidates.iter().take(5) {
                lines.push(format!(
                    "  {:<18} score {:.2}  edits {:.1}  {}",
                    c.replacement,
                    c.score,
                    c.edit_cost,
                    super::theme::kind_label(c.kind)
                ));
            }
        }
        lines
    }

    // -- mouse -----------------------------------------------------------------

    pub(super) fn handle_mouse(&mut self, ev: MouseEvent) {
        if self.modal.is_some() || self.filter_editing {
            return;
        }
        let r = self.rects.get();
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if r.sidebar.width > 0
                    && near_x(ev.column, r.sidebar.right().saturating_sub(1))
                    && ev.row >= r.sidebar.y
                    && ev.row < r.sidebar.bottom()
                {
                    self.dragging = Some(DragTarget::Sidebar);
                    return;
                }
                if r.list_box.width > 0
                    && r.list_box.right() < r.main.right()
                    && near_x(ev.column, r.list_box.right().saturating_sub(1))
                    && ev.row >= r.list_box.y
                    && ev.row < r.list_box.bottom()
                {
                    self.dragging = Some(DragTarget::Split);
                    return;
                }
                if self.screen == Screen::Review
                    && self.review_tab == ReviewTab::Queue
                    && r.review_inspect.height > 0
                    && near_y(ev.row, r.review_inspect.bottom().saturating_sub(1))
                    && ev.column >= r.review_inspect.x
                    && ev.column < r.review_inspect.right()
                {
                    self.dragging = Some(DragTarget::ReviewVertical);
                    return;
                }
                if point_in(ev.column, ev.row, r.nav_inner) {
                    let row = (ev.row - r.nav_inner.y) as usize;
                    if row < Screen::ALL.len() {
                        self.switch_screen(Screen::from_index(row));
                    }
                    return;
                }
                if point_in(ev.column, ev.row, r.header_strip) {
                    let rel = (ev.column - r.header_strip.x) as usize;
                    let idx = if rel > 10 {
                        ((rel - 10) / 10).min(3)
                    } else {
                        0
                    };
                    self.switch_screen(Screen::from_index(idx));
                    return;
                }
                if point_in(ev.column, ev.row, r.review_title) {
                    let mid = r.review_title.x + 18;
                    let next = if ev.column < mid {
                        ReviewTab::Queue
                    } else {
                        ReviewTab::Changes
                    };
                    self.review_tab = next;
                    self.visual_anchor = None;
                    self.focus_sidebar = false;
                    return;
                }
                if point_in(ev.column, ev.row, r.dict_title) {
                    let rel = ev.column.saturating_sub(r.dict_title.x);
                    let approx = (rel / 14).min(4) as usize;
                    self.dict_section = DictSection::ALL[approx];
                    self.dict_idx = 0;
                    self.visual_anchor = None;
                    self.focus_sidebar = false;
                    self.rebuild_dict_view();
                    return;
                }
                if point_in(ev.column, ev.row, r.list_inner) {
                    self.focus_sidebar = false;
                    let rel_y = (ev.row - r.list_inner.y) as usize;
                    if self.screen == Screen::Review && self.review_tab == ReviewTab::Queue {
                        if let Some(&Some(item_idx)) = r.review_line_map.get(rel_y) {
                            if item_idx < self.review.len() {
                                self.review_idx = item_idx;
                            }
                        }
                        return;
                    }
                    let row_offset = usize::from(r.list_has_header);
                    if rel_y < row_offset {
                        return;
                    }
                    let target = r.list_offset + (rel_y - row_offset);
                    match self.screen {
                        Screen::Activity if target < self.activity_view.len() => {
                            self.activity_idx = target;
                        }
                        Screen::Review
                            if self.review_tab == ReviewTab::Changes
                                && target < self.history.len() =>
                        {
                            self.changes_idx = target;
                        }
                        Screen::Dictionary if target < self.dict_view.len() => {
                            self.dict_idx = target;
                        }
                        Screen::Settings
                            if target < self.settings_rows.len()
                                && !matches!(
                                    self.settings_rows[target],
                                    SettingsRow::Header(_)
                                ) =>
                        {
                            self.settings_idx = target;
                        }
                        _ => {}
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.dragging {
                Some(DragTarget::Sidebar) if r.body.width > 0 => {
                    let width = ev
                        .column
                        .saturating_sub(r.body.x)
                        .saturating_add(1)
                        .clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
                    self.layout.sidebar_width = width;
                }
                Some(DragTarget::Split) if r.main.width > 0 => {
                    let rel = ev.column.saturating_sub(r.main.x).saturating_add(1);
                    let pct = ((u32::from(rel) * 100) / u32::from(r.main.width.max(1))) as u16;
                    self.layout.split_pct = pct.clamp(SPLIT_PCT_MIN, SPLIT_PCT_MAX);
                }
                Some(DragTarget::ReviewVertical) if r.main.height > 0 => {
                    let rel = ev.row.saturating_sub(r.main.y).saturating_add(1);
                    let pct = ((u32::from(rel) * 100) / u32::from(r.main.height.max(1))) as u16;
                    self.layout.review_inspect_pct = pct.clamp(INSPECT_PCT_MIN, INSPECT_PCT_MAX);
                }
                _ => {}
            },
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = None;
            }
            MouseEventKind::ScrollUp => {
                if point_in(ev.column, ev.row, r.nav_inner) {
                    self.switch_screen(Screen::from_index(self.screen.index() + 3));
                } else {
                    self.move_active(-3);
                }
            }
            MouseEventKind::ScrollDown => {
                if point_in(ev.column, ev.row, r.nav_inner) {
                    self.switch_screen(Screen::from_index(self.screen.index() + 1));
                } else {
                    self.move_active(3);
                }
            }
            _ => {}
        }
    }

    // -- render coordinator ----------------------------------------------------

    pub(crate) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 24 || area.height < 8 {
            self.rects.set(LayoutRects::default());
            frame.render_widget(
                Paragraph::new("Window too small for SmartKey. Make it bigger or press q.")
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                area,
            );
            return;
        }

        let mut rects = LayoutRects::default();
        let banners = self.banner_lines();
        let compact = area.width < SIDEBAR_MIN_TOTAL || self.layout.zoomed;
        let mut cons = Vec::new();
        if compact {
            cons.push(Constraint::Length(1));
        }
        if !banners.is_empty() {
            cons.push(Constraint::Length(banners.len() as u16));
        }
        cons.extend([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ]);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(cons)
            .split(area);

        let mut i = 0;
        if compact {
            rects.header_strip = chunks[0];
            self.render_header(frame, chunks[0]);
            i = 1;
        }
        if !banners.is_empty() {
            frame.render_widget(Paragraph::new(banners), chunks[i]);
            i += 1;
        }
        let body = chunks[i];
        rects.body = body;
        let main = if compact {
            body
        } else {
            let sw = self
                .layout
                .sidebar_width
                .min(body.width.saturating_sub(24))
                .max(SIDEBAR_WIDTH_MIN);
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(sw), Constraint::Min(20)])
                .split(body);
            rects.sidebar = cols[0];
            rects.nav_inner = self.render_sidebar(frame, cols[0]);
            cols[1]
        };
        rects.main = main;
        match self.screen {
            Screen::Activity => self.render_activity(frame, main, &mut rects),
            Screen::Review => self.render_review(frame, main, &mut rects),
            Screen::Dictionary => self.render_dictionary(frame, main, &mut rects),
            Screen::Settings => self.render_settings(frame, main, &mut rects),
        }
        self.render_toast(frame, chunks[i + 1]);
        self.render_hints(frame, chunks[i + 2]);
        self.rects.set(rects);

        if let Some(modal) = &self.modal {
            self.render_modal(frame, area, modal);
        }
    }

    pub(super) fn split_list_detail(&self, area: Rect) -> (Rect, Option<Rect>) {
        if !self.layout.zoomed && area.width >= DETAIL_MIN_WIDTH {
            let left = self.layout.split_pct.clamp(SPLIT_PCT_MIN, SPLIT_PCT_MAX);
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(left),
                    Constraint::Percentage(100 - left),
                ])
                .split(area);
            (cols[0], Some(cols[1]))
        } else {
            (area, None)
        }
    }
}

fn build_mode_summary(values: &BTreeMap<String, String>) -> String {
    let aggr = match values.get("settings.aggressiveness").map(String::as_str) {
        Some("conservative") => "careful",
        Some("aggressive") => "eager",
        _ => "balanced",
    };
    let dialect = match values.get("settings.dialect").map(String::as_str) {
        Some("en_US") => "US",
        Some("en_GB") => "UK",
        _ => "US+UK",
    };
    format!("{aggr} {dialect}")
}

fn build_undo_summary(values: &BTreeMap<String, String>) -> String {
    let mode = match values.get("settings.undo_mode").map(String::as_str) {
        Some("immediate") => "instant",
        Some("off") => "off",
        _ => "step",
    };
    if mode == "off" {
        return "off".to_string();
    }
    let ms = values
        .get("settings.undo_window_ms")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(5000);
    if ms.is_multiple_of(1000) {
        format!("{mode} · {}s", ms / 1000)
    } else {
        format!("{mode} · {:.1}s", (ms as f64) / 1000.0)
    }
}
