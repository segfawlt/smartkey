use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::scorer::UserRuleView;
use crate::types::{Aggressiveness, Dialect, LangId, MatchKind, ScorerConfig};

/// Built-in onboarding presets (`smartkey init --preset ...`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Preset {
    #[default]
    Balanced,
    ChatCasual,
    DeveloperStrict,
}

impl Preset {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "balanced" | "default" => Some(Self::Balanced),
            "chat" | "chat_casual" | "casual" => Some(Self::ChatCasual),
            "dev" | "developer" | "developer_strict" | "strict" => Some(Self::DeveloperStrict),
            _ => None,
        }
    }

    pub fn initial_toml(self) -> String {
        let (aggr, auto_cap_sent, split_spaces, fix_proper) = match self {
            Self::Balanced => ("balanced", false, true, true),
            Self::ChatCasual => ("balanced", false, true, true),
            Self::DeveloperStrict => ("conservative", false, false, false),
        };
        format!(
            r#"# SmartKey Configuration & Personal Rule Brain (v1)
# Editing this file directly edits SmartKey's behavior; comments are preserved automatically.
version = 1

# Vocabulary kept immune from statistical autocorrect (graduated at 2x undo+keep or 3x untouched use).
personal_dict = []

# ASCII tokens kept immune from Vietnamese Telex composition in fcitx5-lotus (Phase 2).
telex_passthrough = []

# Pair-level rejections ({{ typed = "...", rejected = "..." }}).
blocklist = []

[settings]
enabled = true
dialect = "mixed"               # "mixed" | "en_US" | "en_GB"
aggressiveness = "{aggr}"       # "conservative" | "balanced" | "aggressive"
undo_window_ms = 5000
auto_capitalize_i = true
auto_capitalize_sentence = {auto_cap_sent}
fix_shift_slips = true          # THat's -> That's, situatioN: -> situation:, tHIS -> This
fix_proper_nouns = {fix_proper}         # english -> English, monday -> Monday
enable_contractions = true
enable_missed_space_split = {split_spaces}

# Learned unigram typo repairs (graduated after 2x manual fixes).
[corrections]

# Context-scoped repairs ("prev typed" = "fixed", guarded against valid-word hijacking).
[context_corrections]

# Intentional expansions (1x BackSpace reverts to trigger WITHOUT blocklisting).
[snippets]
"#
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockPair {
    pub typed: String,
    pub rejected: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkippedEditEntry {
    pub lang: String,
    pub prev_word: Option<String>,
    pub orig: String,
    pub fixed: String,
    pub reason: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoResolution {
    /// `1x BackSpace` just snapped the word back; user has not finished re-committing yet.
    Triggered,
    /// User kept their raw word verbatim (`1x` session shield, `2x` permanent).
    KeptVerbatim,
    /// User edited the restored word into a third word (`1x` permanent block + `2x` edit stage).
    EditedToThird { final_word: String },
    /// User retyped the exact suggestion they just undid (`Self-Confirm Cancel`).
    SelfConfirmCancelled,
    /// User held `BackSpace` past index 0 to erase the clause (`Clause-Erase Cancel`).
    ClauseErased,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DiagnosticEvent {
    AutocorrectApplied {
        typed: String,
        applied: String,
        kind: MatchKind,
        total_count: u32,
    },
    BackspaceUndo {
        typed: String,
        rejected: String,
        resolution: UndoResolution,
        undo_count: u32,
    },
    ManualBackspaceFix {
        orig: String,
        fixed: String,
        outcome: String,
        attempt_count: u32,
    },
    UnfixedUnknown {
        word: String,
        best_candidate: Option<String>,
        best_score: f32,
        threshold: f32,
        total_count: u32,
    },
}

/// High-churn staging counters and diagnostic log stored in `learner_state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnerState {
    pub staged_corrections: BTreeMap<String, u32>,
    pub staged_context: BTreeMap<String, u32>,
    pub staged_undo_keep: BTreeMap<String, u32>,
    pub untouched_unknowns: BTreeMap<String, u32>,
    pub staged_telex_passthrough: BTreeMap<String, u32>,
    pub trouble_words: BTreeMap<String, u32>,
    pub skipped_edits: Vec<SkippedEditEntry>,
    /// Cumulative `1x, 2x, 3x+` counts per `typed->applied` autocorrect pair.
    pub applied_counts: BTreeMap<String, u32>,
    /// Cumulative `1x, 2x, 3x+` counts per `typed->rejected` backspace undo.
    pub undo_counts: BTreeMap<String, u32>,
    /// Cumulative `1x, 2x, 3x+` counts per `orig->fixed` manual backspace edit.
    pub manual_fix_counts: BTreeMap<String, u32>,
    /// Bounded ring buffer (`<= 256`) of recent autocorrects, backspace undos, and manual fixes.
    pub recent_events: Vec<DiagnosticEvent>,
}

const MAX_RECENT_EVENTS: usize = 256;

impl LearnerState {
    pub fn push_event(&mut self, ev: DiagnosticEvent) {
        if self.recent_events.len() >= MAX_RECENT_EVENTS {
            self.recent_events.remove(0);
        }
        self.recent_events.push(ev);
    }

    pub fn record_skipped(
        &mut self,
        lang: &str,
        prev_word: Option<&str>,
        orig: &str,
        fixed: &str,
        reason: &str,
    ) {
        if let Some(existing) = self.skipped_edits.iter_mut().find(|e| {
            e.lang == lang
                && e.prev_word.as_deref() == prev_word
                && e.orig == orig
                && e.fixed == fixed
        }) {
            existing.count = existing.count.saturating_add(1);
            return;
        }
        if self.skipped_edits.len() >= 128 {
            self.skipped_edits.remove(0);
        }
        self.skipped_edits.push(SkippedEditEntry {
            lang: lang.to_string(),
            prev_word: prev_word.map(str::to_string),
            orig: orig.to_string(),
            fixed: fixed.to_string(),
            reason: reason.to_string(),
            count: 1,
        });
    }
}

/// Parsed in-memory fast lookup mirrors synchronized with `toml_edit::DocumentMut`.
#[derive(Debug, Clone, Default)]
pub struct CompiledRules {
    pub enabled: bool,
    pub undo_window_ms: u64,
    pub scorer_config: ScorerConfig,
    pub personal_dict: BTreeSet<String>,
    pub lang_personal_dict: BTreeMap<String, BTreeSet<String>>,
    pub telex_passthrough: BTreeSet<String>,
    pub blocklist: BTreeSet<(String, String)>,
    pub corrections: BTreeMap<String, String>,
    pub lang_corrections: BTreeMap<String, BTreeMap<String, String>>,
    pub context_corrections: BTreeMap<(String, String), String>,
    pub snippets: BTreeMap<String, String>,
}

/// Comment-preserving `rules.toml` + `learner_state.json` store.
/// Never reads `$HOME` implicitly; paths are always injected by the host or kept in-memory.
pub struct RuleStore {
    rules_path: Option<PathBuf>,
    state_path: Option<PathBuf>,
    last_mtime: Option<SystemTime>,
    last_state_mtime: Option<SystemTime>,
    parse_locked: bool,
    doc: DocumentMut,
    compiled: CompiledRules,
    state: LearnerState,
    active_lang: LangId,
    /// Session-scoped instant shields (e.g. `1x` Undo + Keep Verbatim before `2x` permanent graduation).
    session_shielded_words: BTreeSet<String>,
    session_blocked_pairs: BTreeSet<(String, String)>,
}

impl RuleStore {
    pub fn in_memory(preset: Preset) -> Self {
        let raw = preset.initial_toml();
        let doc: DocumentMut = raw.parse().expect("valid preset TOML");
        let compiled = Self::extract_compiled(&doc);
        Self {
            rules_path: None,
            state_path: None,
            last_mtime: None,
            last_state_mtime: None,
            parse_locked: false,
            doc,
            compiled,
            state: LearnerState::default(),
            active_lang: LangId::en(),
            session_shielded_words: BTreeSet::new(),
            session_blocked_pairs: BTreeSet::new(),
        }
    }

    pub fn from_toml_str(toml_content: &str) -> Result<Self, String> {
        let doc: DocumentMut = toml_content
            .parse()
            .map_err(|e| format!("TOML error: {e}"))?;
        let compiled = Self::extract_compiled(&doc);
        Ok(Self {
            rules_path: None,
            state_path: None,
            last_mtime: None,
            last_state_mtime: None,
            parse_locked: false,
            doc,
            compiled,
            state: LearnerState::default(),
            active_lang: LangId::en(),
            session_shielded_words: BTreeSet::new(),
            session_blocked_pairs: BTreeSet::new(),
        })
    }

    pub fn load_or_init(
        rules_path: PathBuf,
        state_path: PathBuf,
        preset: Preset,
    ) -> io::Result<Self> {
        let raw_toml = if rules_path.exists() {
            fs::read_to_string(&rules_path)?
        } else {
            if let Some(parent) = rules_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let init = preset.initial_toml();
            fs::write(&rules_path, &init)?;
            init
        };

        let last_mtime = fs::metadata(&rules_path).and_then(|m| m.modified()).ok();
        let (doc, parse_locked) = match raw_toml.parse::<DocumentMut>() {
            Ok(d) => (d, false),
            Err(_) => {
                // User has a temporary syntax error in their editor: do NOT clobber their file!
                let fallback: DocumentMut = preset.initial_toml().parse().unwrap();
                (fallback, true)
            }
        };

        let last_state_mtime = fs::metadata(&state_path).and_then(|m| m.modified()).ok();
        let state: LearnerState = if state_path.exists() {
            fs::read_to_string(&state_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default()
        } else {
            LearnerState::default()
        };

        let compiled = Self::extract_compiled(&doc);
        Ok(Self {
            rules_path: Some(rules_path),
            state_path: Some(state_path),
            last_mtime,
            last_state_mtime,
            parse_locked,
            doc,
            compiled,
            state,
            active_lang: LangId::en(),
            session_shielded_words: BTreeSet::new(),
            session_blocked_pairs: BTreeSet::new(),
        })
    }

    pub fn set_active_lang(&mut self, lang: LangId) {
        self.active_lang = lang;
    }

    pub fn active_lang(&self) -> &LangId {
        &self.active_lang
    }

    pub fn compiled(&self) -> &CompiledRules {
        &self.compiled
    }

    pub fn scorer_config(&self) -> &ScorerConfig {
        &self.compiled.scorer_config
    }

    pub fn scorer_config_mut(&mut self) -> &mut ScorerConfig {
        &mut self.compiled.scorer_config
    }

    pub fn state(&self) -> &LearnerState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut LearnerState {
        &mut self.state
    }

    pub fn to_toml_string(&self) -> String {
        self.doc.to_string()
    }

    pub fn rules_path(&self) -> Option<&Path> {
        self.rules_path.as_deref()
    }

    /// Adds an immediate session-level word shield (e.g. on `1x` Undo + Keep Verbatim).
    pub fn add_session_word_shield(&mut self, lower_word: &str) {
        self.session_shielded_words.insert(lower_word.to_string());
    }

    /// Adds an immediate session-level pair block.
    pub fn add_session_pair_block(&mut self, typed_lower: &str, rejected_lower: &str) {
        self.session_blocked_pairs
            .insert((typed_lower.to_string(), rejected_lower.to_string()));
    }

    /// Reloads `rules.toml` and `learner_state.json` from disk if modified externally
    /// (e.g. user edited `rules.toml`, Syncthing synced, or `smartkey log --clear` ran).
    pub fn reload_if_modified(&mut self) {
        self.sync_external_edits_before_write();
        self.sync_external_state_before_write();
    }

    pub fn state_path(&self) -> Option<&Path> {
        self.state_path.as_deref()
    }

    /// Reloads `rules.toml` from disk if modified externally (e.g. Syncthing or user editor) before writing.
    fn sync_external_edits_before_write(&mut self) {
        let Some(path) = self.rules_path.as_ref() else {
            return;
        };
        let current_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        if current_mtime != self.last_mtime {
            if let Ok(raw) = fs::read_to_string(path) {
                match raw.parse::<DocumentMut>() {
                    Ok(fresh_doc) => {
                        self.doc = fresh_doc;
                        self.compiled = Self::extract_compiled(&self.doc);
                        self.parse_locked = false;
                        self.last_mtime = current_mtime;
                    }
                    Err(_) => {
                        self.parse_locked = true;
                    }
                }
            }
        }
    }

    fn sync_external_state_before_write(&mut self) {
        let Some(path) = self.state_path.as_ref() else {
            return;
        };
        let current_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        if current_mtime != self.last_state_mtime {
            if let Some(fresh_state) = fs::read_to_string(path)
                .ok()
                .and_then(|s| serde_json::from_str::<LearnerState>(&s).ok())
            {
                self.state = fresh_state;
                self.last_state_mtime = current_mtime;
            }
        }
    }

    pub fn flush_rules(&mut self) -> io::Result<()> {
        if self.parse_locked {
            return Ok(());
        }
        if let Some(path) = self.rules_path.as_ref() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(path, self.doc.to_string())?;
            self.last_mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
        }
        Ok(())
    }

    /// Updates `[settings].enabled` in `rules.toml` while preserving all comments.
    pub fn set_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.sync_external_edits_before_write();
        self.compiled.enabled = enabled;
        let settings = self
            .doc
            .entry("settings")
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .unwrap();
        settings["enabled"] = toml_edit::value(enabled);
        self.flush_rules()
    }

    pub fn flush_state(&mut self) -> io::Result<()> {
        if let Some(path) = self.state_path.as_ref().cloned() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let json = serde_json::to_string_pretty(&self.state).map_err(io::Error::other)?;
            let tmp_path = path.with_extension("json.tmp");
            fs::write(&tmp_path, json)?;
            fs::rename(&tmp_path, &path)?;
            self.last_state_mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        }
        Ok(())
    }

    pub fn log_autocorrect_applied(&mut self, typed: &str, applied: &str, kind: MatchKind) {
        self.sync_external_state_before_write();
        let key = format!("{}->{}", typed.to_lowercase(), applied.to_lowercase());
        let total_count = {
            let e = self.state.applied_counts.entry(key).or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        self.state.push_event(DiagnosticEvent::AutocorrectApplied {
            typed: typed.to_string(),
            applied: applied.to_string(),
            kind,
            total_count,
        });
        let _ = self.flush_state();
    }

    pub fn log_backspace_undo(&mut self, typed: &str, rejected: &str, resolution: UndoResolution) {
        self.sync_external_state_before_write();
        let key = format!("{}->{}", typed.to_lowercase(), rejected.to_lowercase());
        let undo_count = {
            let e = self.state.undo_counts.entry(key).or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        self.state.push_event(DiagnosticEvent::BackspaceUndo {
            typed: typed.to_string(),
            rejected: rejected.to_string(),
            resolution,
            undo_count,
        });
        let _ = self.flush_state();
    }

    /// Updates the most recent `Triggered` `BackspaceUndo` entry in place once the user finishes
    /// keeping, editing, self-confirming, or erasing the restored word.
    pub fn resolve_pending_undo_log(
        &mut self,
        typed: &str,
        rejected: &str,
        resolution: UndoResolution,
    ) {
        self.sync_external_state_before_write();
        if let Some(DiagnosticEvent::BackspaceUndo {
            typed: t,
            rejected: r,
            resolution: res_slot,
            ..
        }) = self.state.recent_events.iter_mut().rev().find(|ev| {
            matches!(
                ev,
                DiagnosticEvent::BackspaceUndo {
                    resolution: UndoResolution::Triggered,
                    ..
                }
            )
        }) {
            if t == typed && r == rejected {
                *res_slot = resolution;
                let _ = self.flush_state();
                return;
            }
        }
        self.log_backspace_undo(typed, rejected, resolution);
    }

    pub fn log_manual_backspace_fix(&mut self, orig: &str, fixed: &str, outcome: &str) {
        self.sync_external_state_before_write();
        let orig_l = orig.trim().to_lowercase();
        let fixed_l = fixed.trim().to_lowercase();
        if orig_l.is_empty() || fixed_l.is_empty() || orig_l == fixed_l {
            return;
        }
        let key = format!("{orig_l}->{fixed_l}");
        let attempt_count = {
            let e = self.state.manual_fix_counts.entry(key).or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        self.state.push_event(DiagnosticEvent::ManualBackspaceFix {
            orig: orig.trim().to_string(),
            fixed: fixed.trim().to_string(),
            outcome: outcome.to_string(),
            attempt_count,
        });
        let _ = self.flush_state();
    }

    pub fn log_unfixed_unknown(
        &mut self,
        word: &str,
        best_candidate: Option<&str>,
        best_score: f32,
        threshold: f32,
    ) {
        self.sync_external_state_before_write();
        let word_l = word.trim().to_lowercase();
        if word_l.is_empty() {
            return;
        }
        let total_count = {
            let e = self.state.untouched_unknowns.entry(word_l.clone()).or_insert(0);
            *e = e.saturating_add(1);
            *e
        };
        self.state.push_event(DiagnosticEvent::UnfixedUnknown {
            word: word.trim().to_string(),
            best_candidate: best_candidate.map(str::to_string),
            best_score,
            threshold,
            total_count,
        });
        let _ = self.flush_state();
    }

    pub fn clear_diagnostic_logs(&mut self) -> io::Result<()> {
        self.sync_external_state_before_write();
        self.state.applied_counts.clear();
        self.state.undo_counts.clear();
        self.state.manual_fix_counts.clear();
        self.state.untouched_unknowns.clear();
        self.state.recent_events.clear();
        self.state.skipped_edits.clear();
        self.flush_state()
    }

    /// Permanently adds `(typed, rejected)` to `blocklist` in `rules.toml` (comment-preserving).
    pub fn add_permanent_block(&mut self, typed_lower: &str, rejected_lower: &str) {
        self.sync_external_edits_before_write();
        let pair = (typed_lower.to_string(), rejected_lower.to_string());
        if !self.compiled.blocklist.insert(pair) {
            return;
        }
        if !self.doc.contains_key("blocklist") {
            self.doc["blocklist"] = Item::Value(Value::Array(Array::new()));
        }
        if let Some(arr) = self.doc["blocklist"].as_array_mut() {
            let mut tbl = InlineTable::new();
            tbl.insert("typed", Value::from(typed_lower));
            tbl.insert("rejected", Value::from(rejected_lower));
            arr.push_formatted(Value::InlineTable(tbl));
        }
        let _ = self.flush_rules();
    }

    /// Permanently adds `word_lower` to `personal_dict` (or `[lang.<id>].personal_dict` for non-English).
    pub fn add_personal_word(&mut self, lang: &LangId, word_lower: &str) {
        self.sync_external_edits_before_write();
        if lang.as_str() == "en" {
            if !self.compiled.personal_dict.insert(word_lower.to_string()) {
                return;
            }
            if !self.doc.contains_key("personal_dict") {
                self.doc["personal_dict"] = Item::Value(Value::Array(Array::new()));
            }
            if let Some(arr) = self.doc["personal_dict"].as_array_mut() {
                arr.push(word_lower);
            }
        } else {
            let set = self
                .compiled
                .lang_personal_dict
                .entry(lang.as_str().to_string())
                .or_default();
            if !set.insert(word_lower.to_string()) {
                return;
            }
            let lang_root = self
                .doc
                .entry("lang")
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            let sub = lang_root
                .entry(lang.as_str())
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            if !sub.contains_key("personal_dict") {
                sub["personal_dict"] = Item::Value(Value::Array(Array::new()));
            }
            if let Some(arr) = sub["personal_dict"].as_array_mut() {
                arr.push(word_lower);
            }
        }
        let _ = self.flush_rules();
    }

    /// Permanently adds `typed_lower = fixed` to `[corrections]` (or `[lang.<id>.corrections]`).
    pub fn add_correction(&mut self, lang: &LangId, typed_lower: &str, fixed: &str) {
        self.sync_external_edits_before_write();
        if lang.as_str() == "en" {
            self.compiled
                .corrections
                .insert(typed_lower.to_string(), fixed.to_string());
            let tbl = self
                .doc
                .entry("corrections")
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            tbl[typed_lower] = toml_edit::value(fixed);
        } else {
            self.compiled
                .lang_corrections
                .entry(lang.as_str().to_string())
                .or_default()
                .insert(typed_lower.to_string(), fixed.to_string());
            let lang_root = self
                .doc
                .entry("lang")
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            let sub = lang_root
                .entry(lang.as_str())
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            let corr = sub
                .entry("corrections")
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            corr[typed_lower] = toml_edit::value(fixed);
        }
        let _ = self.flush_rules();
    }

    /// Permanently adds `"prev typed" = "fixed"` to `[context_corrections]`.
    pub fn add_context_correction(&mut self, prev_lower: &str, typed_lower: &str, fixed: &str) {
        self.sync_external_edits_before_write();
        self.compiled.context_corrections.insert(
            (prev_lower.to_string(), typed_lower.to_string()),
            fixed.to_string(),
        );
        let key = format!("{prev_lower} {typed_lower}");
        let tbl = self
            .doc
            .entry("context_corrections")
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .unwrap();
        tbl[&key] = toml_edit::value(fixed);
        let _ = self.flush_rules();
    }

    /// Permanently adds an ASCII word to `telex_passthrough` (for `fcitx5-lotus` bilingual mode).
    pub fn add_telex_passthrough(&mut self, raw_ascii_lower: &str) {
        self.sync_external_edits_before_write();
        if !self
            .compiled
            .telex_passthrough
            .insert(raw_ascii_lower.to_string())
        {
            return;
        }
        if !self.doc.contains_key("telex_passthrough") {
            self.doc["telex_passthrough"] = Item::Value(Value::Array(Array::new()));
        }
        if let Some(arr) = self.doc["telex_passthrough"].as_array_mut() {
            arr.push(raw_ascii_lower);
        }
        let _ = self.flush_rules();
    }

    fn extract_compiled(doc: &DocumentMut) -> CompiledRules {
        let mut out = CompiledRules {
            enabled: true,
            undo_window_ms: 5000,
            scorer_config: ScorerConfig::default(),
            ..CompiledRules::default()
        };

        if let Some(settings) = doc.get("settings").and_then(Item::as_table) {
            if let Some(b) = settings.get("enabled").and_then(Item::as_bool) {
                out.enabled = b;
            }
            if let Some(ms) = settings.get("undo_window_ms").and_then(Item::as_integer) {
                if ms > 0 {
                    out.undo_window_ms = ms as u64;
                }
            }
            if let Some(d) = settings.get("dialect").and_then(Item::as_str) {
                out.scorer_config.dialect = match d {
                    "en_US" | "us" | "en_us" => Dialect::EnUs,
                    "en_GB" | "gb" | "uk" | "en_gb" => Dialect::EnGb,
                    _ => Dialect::Mixed,
                };
            }
            if let Some(a) = settings.get("aggressiveness").and_then(Item::as_str) {
                out.scorer_config.aggressiveness = match a {
                    "conservative" => Aggressiveness::Conservative,
                    "aggressive" => Aggressiveness::Aggressive,
                    _ => Aggressiveness::Balanced,
                };
            }
            if let Some(b) = settings.get("auto_capitalize_i").and_then(Item::as_bool) {
                out.scorer_config.auto_capitalize_i = b;
            }
            if let Some(b) = settings
                .get("auto_capitalize_sentence")
                .and_then(Item::as_bool)
            {
                out.scorer_config.auto_capitalize_sentence = b;
            }
            if let Some(b) = settings
                .get("fix_shift_slips")
                .or_else(|| settings.get("fix_double_cap"))
                .and_then(Item::as_bool)
            {
                out.scorer_config.fix_shift_slips = b;
            }
            if let Some(b) = settings.get("fix_proper_nouns").and_then(Item::as_bool) {
                out.scorer_config.fix_proper_nouns = b;
            }
            if let Some(b) = settings.get("enable_contractions").and_then(Item::as_bool) {
                out.scorer_config.enable_contractions = b;
            }
            if let Some(b) = settings
                .get("enable_missed_space_split")
                .and_then(Item::as_bool)
            {
                out.scorer_config.enable_missed_space_split = b;
            }
        }

        if let Some(arr) = doc.get("personal_dict").and_then(Item::as_array) {
            for val in arr {
                if let Some(s) = val.as_str() {
                    out.personal_dict.insert(s.to_lowercase());
                }
            }
        }

        if let Some(arr) = doc.get("telex_passthrough").and_then(Item::as_array) {
            for val in arr {
                if let Some(s) = val.as_str() {
                    out.telex_passthrough.insert(s.to_lowercase());
                }
            }
        }

        if let Some(arr) = doc.get("blocklist").and_then(Item::as_array) {
            for val in arr {
                if let Some(tbl) = val.as_inline_table() {
                    if let (Some(t), Some(r)) = (
                        tbl.get("typed").and_then(Value::as_str),
                        tbl.get("rejected").and_then(Value::as_str),
                    ) {
                        out.blocklist.insert((t.to_lowercase(), r.to_lowercase()));
                    }
                }
            }
        }

        if let Some(tbl) = doc.get("corrections").and_then(Item::as_table) {
            for (k, v) in tbl {
                if let Some(target) = v.as_str() {
                    out.corrections.insert(k.to_lowercase(), target.to_string());
                }
            }
        }

        if let Some(tbl) = doc.get("context_corrections").and_then(Item::as_table) {
            for (k, v) in tbl {
                if let (Some((prev, typed)), Some(target)) = (k.split_once(' '), v.as_str()) {
                    out.context_corrections.insert(
                        (prev.trim().to_lowercase(), typed.trim().to_lowercase()),
                        target.to_string(),
                    );
                }
            }
        }

        if let Some(tbl) = doc.get("snippets").and_then(Item::as_table) {
            for (k, v) in tbl {
                if let Some(exp) = v.as_str() {
                    out.snippets.insert(k.to_string(), exp.to_string());
                }
            }
        }

        if let Some(lang_tbl) = doc.get("lang").and_then(Item::as_table) {
            for (lang_key, sub_item) in lang_tbl {
                if let Some(sub) = sub_item.as_table() {
                    if let Some(arr) = sub.get("personal_dict").and_then(Item::as_array) {
                        let set = out
                            .lang_personal_dict
                            .entry(lang_key.to_string())
                            .or_default();
                        for val in arr {
                            if let Some(s) = val.as_str() {
                                set.insert(s.to_lowercase());
                            }
                        }
                    }
                    if let Some(corr) = sub.get("corrections").and_then(Item::as_table) {
                        let map = out
                            .lang_corrections
                            .entry(lang_key.to_string())
                            .or_default();
                        for (k, v) in corr {
                            if let Some(t) = v.as_str() {
                                map.insert(k.to_lowercase(), t.to_string());
                            }
                        }
                    }
                }
            }
        }

        out
    }
}

impl UserRuleView for RuleStore {
    fn lookup_snippet(&self, trigger: &str) -> Option<&str> {
        self.compiled.snippets.get(trigger).map(String::as_str)
    }

    fn lookup_context_rule(&self, prev_lower: &str, typed_lower: &str) -> Option<&str> {
        self.compiled
            .context_corrections
            .get(&(prev_lower.to_string(), typed_lower.to_string()))
            .map(String::as_str)
    }

    fn lookup_correction(&self, typed_lower: &str) -> Option<&str> {
        let lang = self.active_lang.as_str();
        if lang != "en" {
            if let Some(target) = self
                .compiled
                .lang_corrections
                .get(lang)
                .and_then(|m| m.get(typed_lower))
            {
                return Some(target.as_str());
            }
        }
        self.compiled
            .corrections
            .get(typed_lower)
            .map(String::as_str)
    }

    fn is_personal_word(&self, lower_word: &str) -> bool {
        if self.session_shielded_words.contains(lower_word) {
            return true;
        }
        let lang = self.active_lang.as_str();
        if lang != "en"
            && self
                .compiled
                .lang_personal_dict
                .get(lang)
                .is_some_and(|s| s.contains(lower_word))
        {
            return true;
        }
        self.compiled.personal_dict.contains(lower_word)
    }

    fn is_pair_blocked(&self, typed_lower: &str, candidate_lower: &str) -> bool {
        let key = (typed_lower.to_string(), candidate_lower.to_string());
        self.session_blocked_pairs.contains(&key) || self.compiled.blocklist.contains(&key)
    }

    fn trouble_word_boost(&self, candidate_lower: &str) -> f32 {
        match self.state.trouble_words.get(candidate_lower).copied() {
            Some(cnt) if cnt >= 2 => (0.06 * (cnt.min(4) as f32)).min(0.22),
            _ => 0.0,
        }
    }
}
