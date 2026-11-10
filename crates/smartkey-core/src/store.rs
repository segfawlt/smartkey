use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::event_log::{current_epoch_ms, date_string, DiagnosticsConfig, EventLog, EventLogLevel};
use crate::scorer::UserRuleView;
use crate::types::{Aggressiveness, AppProfile, Dialect, LangId, MatchKind, ScorerConfig};
use crate::vi_telex::LotusOptions;

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
expand_shorthand = true         # pls -> please, rn -> right now, ko -> không, dc -> được

# Modular language & input-method bridge plugins (`smartkey plugin list|enable|disable|toggle`).
[plugins]
vi_lotus = true                 # Zero-fork Vietnamese Telex typo rescue & bilingual bridge for fcitx5-lotus

# Durable decision & learning provenance log (`~/.local/state/smartkey/logs/events-YYYY-MM-DD.jsonl`).
[diagnostics]
event_log = true                # Durable decision log on/off
log_level = "interesting"       # "interesting" | "all" ("all" = every committed word)
keep_days = 14                  # Prune logs older than N days (0 = keep forever)
near_miss_band = 0.5            # Log declined unknowns scoring >= band * threshold
annotate_rules = true           # Provenance comments on machine-learned rules.toml entries

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
    RealWordDeclined {
        word: String,
        prev: String,
        candidate: String,
        candidate_score: f32,
        typed_score: f32,
    },
    AppBypass {
        app: String,
        profile: AppProfile,
        reason: String,
    },
    RuleLearned {
        rule: String,
        stage: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        previous: Option<String>,
    },
    WordUnchanged {
        word: String,
        reason: MatchKind,
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

/// Optional user overrides in `[app_profiles]` (`rules.toml`) for custom Flatpak/Wayland app IDs
/// and compositors that omit foreign-toplevel metadata (`unknown_app_policy = "prose" | "bypass"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AppProfileConfig {
    pub unknown_app_policy: Option<AppProfile>,
    pub bypass: BTreeSet<String>,
    pub browser: BTreeSet<String>,
    pub chat: BTreeSet<String>,
    pub prose: BTreeSet<String>,
}

impl AppProfileConfig {
    fn matches_set(prog: &str, set: &BTreeSet<String>) -> bool {
        if set.contains(prog) {
            return true;
        }
        if prog.contains('.') {
            for seg in prog.split('.').skip(1) {
                if seg.len() >= 3 && set.contains(seg) {
                    return true;
                }
            }
        }
        false
    }

    pub fn classify_override(&self, normalized_app: &str) -> Option<AppProfile> {
        let app = normalized_app.trim().to_ascii_lowercase();
        if app.is_empty() {
            return self.unknown_app_policy;
        }
        if Self::matches_set(&app, &self.bypass) {
            return Some(AppProfile::Bypass);
        }
        if Self::matches_set(&app, &self.browser) {
            return Some(AppProfile::Browser);
        }
        if Self::matches_set(&app, &self.chat) {
            return Some(AppProfile::Chat);
        }
        if Self::matches_set(&app, &self.prose) {
            return Some(AppProfile::Prose);
        }
        None
    }
}

/// Parsed in-memory fast lookup mirrors synchronized with `toml_edit::DocumentMut`.
#[derive(Debug, Clone, Default)]
pub struct CompiledRules {
    pub enabled: bool,
    pub vi_lotus_enabled: bool,
    pub undo_window_ms: u64,
    pub scorer_config: ScorerConfig,
    pub diagnostics: DiagnosticsConfig,
    pub app_profiles: AppProfileConfig,
    pub personal_dict: BTreeSet<String>,
    pub personal_dict_canonical: BTreeMap<String, String>,
    pub lang_personal_dict: BTreeMap<String, BTreeSet<String>>,
    pub lang_personal_dict_canonical: BTreeMap<String, BTreeMap<String, String>>,
    pub telex_passthrough: BTreeSet<String>,
    pub blocklist: BTreeSet<(String, String)>,
    pub corrections: BTreeMap<String, String>,
    pub lang_corrections: BTreeMap<String, BTreeMap<String, String>>,
    pub context_corrections: BTreeMap<(String, String), String>,
    pub snippets: BTreeMap<String, String>,
}

static TMP_NONCE: AtomicU64 = AtomicU64::new(1);

/// High-resolution file identity stamp used to detect external writes across processes,
/// coarse-mtime filesystems, and mtime-preserving sync tools (e.g. Syncthing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    mtime: Option<SystemTime>,
    len: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
}

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Some(Self {
                mtime: meta.modified().ok(),
                len: meta.len(),
                ino: meta.ino(),
                ctime: meta.ctime(),
                ctime_nsec: meta.ctime_nsec(),
            })
        }
        #[cfg(not(unix))]
        {
            Some(Self {
                mtime: meta.modified().ok(),
                len: meta.len(),
            })
        }
    }
}

/// Writes `bytes` to `path` atomically via a unique per-process/per-call dotfile temp path,
/// flushing file data (`sync_all`) before `rename` and syncing the parent directory afterwards.
pub(crate) fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent_dir)?;

    let base = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("smartkey");
    let pid = std::process::id();

    let mut last_err = None;
    let mut opened = None;
    for _ in 0..16 {
        let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent_dir.join(format!(".{base}.tmp.{pid}.{nonce}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => {
                opened = Some((candidate, f));
                break;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    let Some((tmp_path, mut file)) = opened else {
        return Err(last_err.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "failed to allocate temp file")
        }));
    };

    if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    drop(file);

    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }

    if let Ok(dir_file) = fs::File::open(parent_dir) {
        let _ = dir_file.sync_all();
    }
    Ok(())
}

/// Preserves a corrupt `rules.toml` or `learner_state.json` snapshot as `<file>.corrupt-<epoch_ms>`
/// (deduplicated by exact byte contents so repeated loads while an editor has a syntax error never
/// spam backup files). When `remove_original` is true, moves the corrupt file out of the way.
fn quarantine_corrupt_file(path: &Path, raw_bytes: &[u8], remove_original: bool) -> Option<PathBuf> {
    let parent_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let base = path.file_name()?.to_str()?;
    let prefix = format!("{base}.corrupt-");

    if let Ok(entries) = fs::read_dir(parent_dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if name.starts_with(&prefix) {
                    if let Ok(existing) = fs::read(entry.path()) {
                        if existing == raw_bytes {
                            if remove_original {
                                let _ = fs::remove_file(path);
                            }
                            return Some(entry.path());
                        }
                    }
                }
            }
        }
    }

    let ts = current_epoch_ms();
    let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut quarantine_path = parent_dir.join(format!("{base}.corrupt-{ts}"));
    if quarantine_path.exists() {
        quarantine_path = parent_dir.join(format!("{base}.corrupt-{ts}-{nonce}"));
    }

    if remove_original {
        if fs::rename(path, &quarantine_path).is_ok() {
            return Some(quarantine_path);
        }
        if atomic_write_bytes(&quarantine_path, raw_bytes).is_ok() {
            let _ = fs::remove_file(path);
            return Some(quarantine_path);
        }
        None
    } else if atomic_write_bytes(&quarantine_path, raw_bytes).is_ok() {
        Some(quarantine_path)
    } else {
        None
    }
}

/// Best-effort line scanner for broken `rules.toml` files so an explicit `enabled = false` or
/// `vi_lotus = false` is never silently overridden back to `true` when the user makes a TOML typo.
fn salvage_safety_flags_from_broken_toml(raw: &str, compiled: &mut CompiledRules) {
    let mut section = String::new();
    for raw_line in raw.lines() {
        let mut in_quotes = false;
        let mut comment_idx = None;
        for (idx, ch) in raw_line.char_indices() {
            if ch == '"' {
                in_quotes = !in_quotes;
            } else if ch == '#' && !in_quotes {
                comment_idx = Some(idx);
                break;
            }
        }
        let line = match comment_idx {
            Some(i) => raw_line[..i].trim(),
            None => raw_line.trim(),
        };
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && !line.starts_with("[[") {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().trim_matches('"').trim_matches('\'');
        let val = v.trim();
        if (section == "settings" || section.is_empty()) && key == "enabled" {
            if val == "false" {
                compiled.enabled = false;
            } else if val == "true" {
                compiled.enabled = true;
            }
        } else if section == "plugins" && (key == "vi_lotus" || key == "vi-lotus") {
            if val == "false" {
                compiled.vi_lotus_enabled = false;
            } else if val == "true" {
                compiled.vi_lotus_enabled = true;
            }
        }
    }
}

/// Comment-preserving `rules.toml` + `learner_state.json` store.
/// Never reads `$HOME` implicitly; paths are always injected by the host or kept in-memory.
pub struct RuleStore {
    rules_path: Option<PathBuf>,
    state_path: Option<PathBuf>,
    lotus_conf_path: Option<PathBuf>,
    last_rules_stamp: Option<FileStamp>,
    last_state_stamp: Option<FileStamp>,
    last_lotus_conf_stamp: Option<FileStamp>,
    lotus_options: LotusOptions,
    parse_locked: bool,
    parse_error: Option<String>,
    doc: DocumentMut,
    compiled: CompiledRules,
    state: LearnerState,
    active_lang: LangId,
    event_log: EventLog,
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
            lotus_conf_path: None,
            last_rules_stamp: None,
            last_state_stamp: None,
            last_lotus_conf_stamp: None,
            lotus_options: LotusOptions::default(),
            parse_locked: false,
            parse_error: None,
            doc,
            compiled,
            state: LearnerState::default(),
            active_lang: LangId::en(),
            event_log: EventLog::new(),
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
            lotus_conf_path: None,
            last_rules_stamp: None,
            last_state_stamp: None,
            last_lotus_conf_stamp: None,
            lotus_options: LotusOptions::default(),
            parse_locked: false,
            parse_error: None,
            doc,
            compiled,
            state: LearnerState::default(),
            active_lang: LangId::en(),
            event_log: EventLog::new(),
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
            let init = preset.initial_toml();
            atomic_write_bytes(&rules_path, init.as_bytes())?;
            init
        };

        let last_rules_stamp = FileStamp::of(&rules_path);
        let (doc, parse_locked, parse_error) = match raw_toml.parse::<DocumentMut>() {
            Ok(d) => (d, false, None),
            Err(err) => {
                // Keep user's broken `rules.toml` untouched in their editor AND save a deduplicated
                // `.corrupt-<epoch>` backup snapshot for recovery.
                let _ = quarantine_corrupt_file(&rules_path, raw_toml.as_bytes(), false);
                let fallback: DocumentMut = preset.initial_toml().parse().unwrap();
                (fallback, true, Some(err.to_string()))
            }
        };

        let mut compiled = Self::extract_compiled(&doc);
        if parse_locked {
            salvage_safety_flags_from_broken_toml(&raw_toml, &mut compiled);
        }

        let state: LearnerState = if state_path.exists() {
            match fs::read(&state_path) {
                Ok(raw_bytes) if !raw_bytes.iter().all(|b| b.is_ascii_whitespace()) => {
                    match serde_json::from_slice::<LearnerState>(&raw_bytes) {
                        Ok(parsed) => parsed,
                        Err(_) => {
                            let _ = quarantine_corrupt_file(&state_path, &raw_bytes, true);
                            LearnerState::default()
                        }
                    }
                }
                _ => LearnerState::default(),
            }
        } else {
            LearnerState::default()
        };
        let last_state_stamp = FileStamp::of(&state_path);

        // Discover sibling `$XDG_CONFIG_HOME/fcitx5/conf/lotus.conf` relative to `rules_path`
        // (`$XDG_CONFIG_HOME/smartkey/rules.toml`) without reading `$HOME` implicitly.
        let lotus_conf_path = rules_path
            .parent()
            .and_then(|p| p.parent())
            .map(|cfg_root| cfg_root.join("fcitx5/conf/lotus.conf"));
        let (lotus_options, last_lotus_conf_stamp) = match lotus_conf_path.as_deref() {
            Some(lp) if lp.exists() => {
                let opts = fs::read_to_string(lp)
                    .map(|s| LotusOptions::from_fcitx5_conf_str(&s))
                    .unwrap_or_default();
                (opts, FileStamp::of(lp))
            }
            _ => (LotusOptions::default(), None),
        };

        Ok(Self {
            rules_path: Some(rules_path),
            state_path: Some(state_path),
            lotus_conf_path,
            last_rules_stamp,
            last_state_stamp,
            last_lotus_conf_stamp,
            lotus_options,
            parse_locked,
            parse_error,
            doc,
            compiled,
            state,
            active_lang: LangId::en(),
            event_log: EventLog::new(),
            session_shielded_words: BTreeSet::new(),
            session_blocked_pairs: BTreeSet::new(),
        })
    }

    pub fn lotus_options(&self) -> LotusOptions {
        self.lotus_options
    }

    pub fn set_lotus_options(&mut self, opts: LotusOptions) {
        self.lotus_options = opts;
    }

    pub fn is_parse_locked(&self) -> bool {
        self.parse_locked
    }

    pub fn parse_error(&self) -> Option<&str> {
        self.parse_error.as_deref()
    }

    pub fn set_log_dir(&mut self, dir: Option<PathBuf>) {
        self.event_log.set_dir(dir);
    }

    pub fn log_dir(&self) -> Option<&Path> {
        self.event_log.dir()
    }

    pub fn set_current_app(&mut self, app: Option<&str>) {
        self.event_log.set_app(app);
    }

    pub fn current_app(&self) -> Option<&str> {
        self.event_log.app()
    }

    pub fn diagnostics(&self) -> &DiagnosticsConfig {
        &self.compiled.diagnostics
    }

    pub fn record_event_only(&mut self, ev: &DiagnosticEvent) {
        if self.compiled.diagnostics.event_log {
            self.event_log.record(
                &self.compiled.diagnostics,
                self.active_lang.as_str(),
                ev,
            );
        }
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

    /// Reloads `rules.toml` and `learner_state.json` (plus sibling `lotus.conf`) from disk if modified externally.
    pub fn reload_if_modified(&mut self) {
        self.sync_external_edits_before_write();
        self.sync_external_state_before_write();
        self.sync_external_lotus_conf();
    }

    fn sync_external_lotus_conf(&mut self) {
        let Some(lp) = self.lotus_conf_path.as_ref() else {
            return;
        };
        let cur = FileStamp::of(lp);
        if cur != self.last_lotus_conf_stamp {
            self.lotus_options = match cur {
                Some(_) => fs::read_to_string(lp)
                    .map(|s| LotusOptions::from_fcitx5_conf_str(&s))
                    .unwrap_or_default(),
                None => LotusOptions::default(),
            };
            self.last_lotus_conf_stamp = cur;
        }
    }

    pub fn state_path(&self) -> Option<&Path> {
        self.state_path.as_deref()
    }

    /// Reloads `rules.toml` from disk if modified externally (e.g. Syncthing or user editor) before writing.
    fn sync_external_edits_before_write(&mut self) {
        let Some(path) = self.rules_path.as_ref().cloned() else {
            return;
        };
        let current_stamp = FileStamp::of(&path);
        if current_stamp != self.last_rules_stamp {
            if let Ok(raw) = fs::read_to_string(&path) {
                match raw.parse::<DocumentMut>() {
                    Ok(fresh_doc) => {
                        self.doc = fresh_doc;
                        self.compiled = Self::extract_compiled(&self.doc);
                        self.parse_locked = false;
                        self.parse_error = None;
                        self.last_rules_stamp = current_stamp;
                    }
                    Err(err) => {
                        let _ = quarantine_corrupt_file(&path, raw.as_bytes(), false);
                        salvage_safety_flags_from_broken_toml(&raw, &mut self.compiled);
                        self.parse_locked = true;
                        self.parse_error = Some(err.to_string());
                        self.last_rules_stamp = current_stamp;
                    }
                }
            }
        }
    }

    pub(crate) fn sync_state_before_write(&mut self) {
        self.sync_external_state_before_write();
    }

    fn sync_external_state_before_write(&mut self) {
        let Some(path) = self.state_path.as_ref().cloned() else {
            return;
        };
        let current_stamp = FileStamp::of(&path);
        if current_stamp != self.last_state_stamp {
            match fs::read(&path) {
                Ok(raw_bytes) if !raw_bytes.iter().all(|b| b.is_ascii_whitespace()) => {
                    match serde_json::from_slice::<LearnerState>(&raw_bytes) {
                        Ok(fresh_state) => {
                            self.state = fresh_state;
                            self.last_state_stamp = current_stamp;
                        }
                        Err(_) => {
                            let _ = quarantine_corrupt_file(&path, &raw_bytes, true);
                            self.last_state_stamp = FileStamp::of(&path);
                        }
                    }
                }
                Ok(_) => {
                    self.last_state_stamp = current_stamp;
                }
                Err(_) if current_stamp.is_none() => {
                    // External deletion of `learner_state.json`: reset in-memory state cleanly.
                    self.state = LearnerState::default();
                    self.last_state_stamp = None;
                }
                Err(_) => {}
            }
        }
    }

    pub fn flush_rules(&mut self) -> io::Result<()> {
        if self.parse_locked {
            let detail = self
                .parse_error
                .as_deref()
                .unwrap_or("unparseable TOML");
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "rules.toml has a syntax error and is write-locked to prevent data loss ({detail})"
                ),
            ));
        }
        if let Some(path) = self.rules_path.as_ref().cloned() {
            let rendered = self.doc.to_string();
            atomic_write_bytes(&path, rendered.as_bytes())?;
            self.last_rules_stamp = FileStamp::of(&path);
        }
        Ok(())
    }

    /// Updates `[settings].enabled` in `rules.toml` while preserving all comments.
    pub fn set_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.sync_external_edits_before_write();
        if self.parse_locked {
            return self.flush_rules();
        }
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

    /// Normalizes a plugin name (`"vi-lotus"`, `"vi_lotus"`, `"lotus"`, `"vi"`) to its canonical key.
    pub fn normalize_plugin_key(name: &str) -> Option<&'static str> {
        match name.trim().to_ascii_lowercase().as_str() {
            "vi-lotus" | "vi_lotus" | "lotus" | "fcitx5-lotus" | "vi" => Some("vi_lotus"),
            _ => None,
        }
    }

    pub fn is_plugin_enabled(&self, name: &str) -> Option<bool> {
        match Self::normalize_plugin_key(name)? {
            "vi_lotus" => Some(self.compiled.vi_lotus_enabled),
            _ => None,
        }
    }

    /// Updates `[plugins].<key>` in `rules.toml` while preserving all comments.
    pub fn set_plugin_enabled(&mut self, name: &str, enabled: bool) -> io::Result<bool> {
        let Some(key) = Self::normalize_plugin_key(name) else {
            return Ok(false);
        };
        self.sync_external_edits_before_write();
        if self.parse_locked {
            self.flush_rules()?;
        }
        if key == "vi_lotus" {
            self.compiled.vi_lotus_enabled = enabled;
        }
        let plugins = self
            .doc
            .entry("plugins")
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .unwrap();
        plugins[key] = toml_edit::value(enabled);
        self.flush_rules()?;
        Ok(true)
    }

    pub fn flush_state(&mut self) -> io::Result<()> {
        if let Some(path) = self.state_path.as_ref().cloned() {
            let json = serde_json::to_string_pretty(&self.state).map_err(io::Error::other)?;
            atomic_write_bytes(&path, json.as_bytes())?;
            self.last_state_stamp = FileStamp::of(&path);
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
        let ev = DiagnosticEvent::AutocorrectApplied {
            typed: typed.to_string(),
            applied: applied.to_string(),
            kind,
            total_count,
        };
        self.record_event_only(&ev);
        self.state.push_event(ev);
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
        let is_triggered = matches!(resolution, UndoResolution::Triggered);
        let ev = DiagnosticEvent::BackspaceUndo {
            typed: typed.to_string(),
            rejected: rejected.to_string(),
            resolution,
            undo_count,
        };
        if !is_triggered {
            self.record_event_only(&ev);
        }
        self.state.push_event(ev);
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
        let mut resolved_count = None;
        if let Some(DiagnosticEvent::BackspaceUndo {
            typed: t,
            rejected: r,
            resolution: res_slot,
            undo_count,
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
                *res_slot = resolution.clone();
                resolved_count = Some(*undo_count);
            }
        }
        if let Some(undo_count) = resolved_count {
            let _ = self.flush_state();
            let ev = DiagnosticEvent::BackspaceUndo {
                typed: typed.to_string(),
                rejected: rejected.to_string(),
                resolution,
                undo_count,
            };
            self.record_event_only(&ev);
            return;
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
        let ev = DiagnosticEvent::ManualBackspaceFix {
            orig: orig.trim().to_string(),
            fixed: fixed.trim().to_string(),
            outcome: outcome.to_string(),
            attempt_count,
        };
        self.record_event_only(&ev);
        self.state.push_event(ev);
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
        let ev = DiagnosticEvent::UnfixedUnknown {
            word: word.trim().to_string(),
            best_candidate: best_candidate.map(str::to_string),
            best_score,
            threshold,
            total_count,
        };
        self.record_event_only(&ev);
        self.state.push_event(ev);
        let _ = self.flush_state();
    }

    /// Reverts the tentative `log_unfixed_unknown` counter and trailing diagnostic event when the user
    /// immediately backspaces into or deletes a just-committed unknown word.
    pub fn cancel_pending_unfixed_unknown(&mut self, word: &str) {
        self.sync_external_state_before_write();
        let word_trimmed = word.trim();
        let word_l = word_trimmed.to_lowercase();
        if word_l.is_empty() {
            return;
        }
        if let Some(cnt) = self.state.untouched_unknowns.get_mut(&word_l) {
            if *cnt <= 1 {
                self.state.untouched_unknowns.remove(&word_l);
            } else {
                *cnt -= 1;
            }
        }
        if let Some(pos) = self.state.recent_events.iter().rposition(|ev| {
            matches!(
                ev,
                DiagnosticEvent::UnfixedUnknown { word: w, .. }
                    if w.eq_ignore_ascii_case(word_trimmed) || w.to_lowercase() == word_l
            )
        }) {
            self.state.recent_events.remove(pos);
        }
        let _ = self.flush_state();
    }

    pub fn log_real_word_declined(
        &mut self,
        word: &str,
        prev: &str,
        candidate: &str,
        candidate_score: f32,
        typed_score: f32,
    ) {
        let ev = DiagnosticEvent::RealWordDeclined {
            word: word.to_string(),
            prev: prev.to_string(),
            candidate: candidate.to_string(),
            candidate_score,
            typed_score,
        };
        self.record_event_only(&ev);
    }

    pub fn log_app_bypass(&mut self, app: &str, profile: AppProfile, reason: &str) {
        let ev = DiagnosticEvent::AppBypass {
            app: app.to_string(),
            profile,
            reason: reason.to_string(),
        };
        self.record_event_only(&ev);
    }

    pub fn log_word_unchanged(&mut self, word: &str, reason: MatchKind) {
        let ev = DiagnosticEvent::WordUnchanged {
            word: word.to_string(),
            reason,
        };
        self.record_event_only(&ev);
    }

    pub fn log_rule_learned(&mut self, rule: &str, stage: &str, previous: Option<String>) {
        let ev = DiagnosticEvent::RuleLearned {
            rule: rule.to_string(),
            stage: stage.to_string(),
            previous,
        };
        self.record_event_only(&ev);
    }

    fn format_provenance_comment(&self, label: &str, previous: Option<&str>) -> String {
        let d = date_string(current_epoch_ms());
        let app_part = self
            .event_log
            .app()
            .filter(|a| !a.is_empty())
            .map(|a| format!(" ({a})"))
            .unwrap_or_default();
        let was_part = previous
            .map(|old| format!(" — was: {old:?}"))
            .unwrap_or_default();
        format!(" # {label} {d}{app_part}{was_part}")
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

    /// Permanently adds `(typed, rejected)` to `blocklist` in `rules.toml` (comment-preserving),
    /// and removes any learned `[corrections]` entry that mapped `typed_lower -> rejected_lower`.
    pub fn add_permanent_block(&mut self, typed_lower: &str, rejected_lower: &str) {
        self.sync_external_edits_before_write();
        let mut modified = false;

        if self
            .compiled
            .corrections
            .get(typed_lower)
            .is_some_and(|t| t.eq_ignore_ascii_case(rejected_lower) || t.to_lowercase() == rejected_lower)
        {
            self.compiled.corrections.remove(typed_lower);
            if let Some(tbl) = self.doc.get_mut("corrections").and_then(Item::as_table_mut) {
                tbl.remove(typed_lower);
            }
            modified = true;
        }

        for (lang_key, map) in &mut self.compiled.lang_corrections {
            if map
                .get(typed_lower)
                .is_some_and(|t| t.to_lowercase() == rejected_lower)
            {
                map.remove(typed_lower);
                if let Some(corr_tbl) = self
                    .doc
                    .get_mut("lang")
                    .and_then(Item::as_table_mut)
                    .and_then(|t| t.get_mut(lang_key.as_str()))
                    .and_then(Item::as_table_mut)
                    .and_then(|t| t.get_mut("corrections"))
                    .and_then(Item::as_table_mut)
                {
                    corr_tbl.remove(typed_lower);
                }
                modified = true;
            }
        }

        let pair = (typed_lower.to_string(), rejected_lower.to_string());
        if self.compiled.blocklist.insert(pair) {
            if !self.doc.contains_key("blocklist") {
                self.doc["blocklist"] = Item::Value(Value::Array(Array::new()));
            }
            if let Some(arr) = self.doc["blocklist"].as_array_mut() {
                let mut tbl = InlineTable::new();
                tbl.insert("typed", Value::from(typed_lower));
                tbl.insert("rejected", Value::from(rejected_lower));
                arr.push_formatted(Value::InlineTable(tbl));
            }
            modified = true;
        }
        if modified {
            let _ = self.flush_rules();
        }
    }

    /// Returns true if `lower_word` is explicitly stored in `[lang.<id>].personal_dict`
    /// (without falling through to global English `personal_dict`), preventing English homographs
    /// from blocking Vietnamese Telex typo rescue unless explicitly added under `[lang.vi]`.
    pub fn is_lang_only_personal_word(&self, lang: &str, lower_word: &str) -> bool {
        if self.session_shielded_words.contains(lower_word) {
            return true;
        }
        self.compiled
            .lang_personal_dict
            .get(lang)
            .is_some_and(|s| s.contains(lower_word))
    }

    /// Returns true if `lower_word` is explicitly stored in `personal_dict` (excluding session shields).
    pub fn is_explicit_personal_word(&self, lang: &LangId, lower_word: &str) -> bool {
        if lang.as_str() != "en"
            && self
                .compiled
                .lang_personal_dict
                .get(lang.as_str())
                .is_some_and(|s| s.contains(lower_word))
        {
            return true;
        }
        self.compiled.personal_dict.contains(lower_word)
    }

    /// Permanently adds `word` to `personal_dict` (or `[lang.<id>].personal_dict` for non-English),
    /// preserving canonical casing (`"Hyprland"`, `"PyTorch"`, `"FastAPI"`).
    pub fn add_personal_word(&mut self, lang: &LangId, word: &str) {
        self.add_personal_word_with_stage(lang, word, "personal_graduated");
    }

    pub fn add_personal_word_with_stage(&mut self, lang: &LangId, word: &str, stage: &str) {
        let word_trimmed = word.trim();
        let word_lower = word_trimmed.to_lowercase();
        if word_lower.is_empty() {
            return;
        }
        self.sync_external_edits_before_write();
        if lang.as_str() == "en" {
            let is_new = self.compiled.personal_dict.insert(word_lower.clone());
            if !is_new {
                // Upgrade canonical casing if previously lowercase and now explicitly cased
                if word_trimmed.chars().any(char::is_uppercase)
                    && self
                        .compiled
                        .personal_dict_canonical
                        .get(&word_lower)
                        .is_none_or(|c| c != word_trimmed)
                {
                    self.compiled
                        .personal_dict_canonical
                        .insert(word_lower.clone(), word_trimmed.to_string());
                    if let Some(arr) = self.doc.get_mut("personal_dict").and_then(Item::as_array_mut) {
                        for item in arr.iter_mut() {
                            if item
                                .as_str()
                                .is_some_and(|s| s.to_lowercase() == word_lower)
                            {
                                *item = Value::from(word_trimmed);
                            }
                        }
                    }
                    let _ = self.flush_rules();
                }
                return;
            }
            self.compiled
                .personal_dict_canonical
                .insert(word_lower.clone(), word_trimmed.to_string());
            if !self.doc.contains_key("personal_dict") {
                self.doc["personal_dict"] = Item::Value(Value::Array(Array::new()));
            }
            if let Some(arr) = self.doc["personal_dict"].as_array_mut() {
                arr.push(word_trimmed);
            }
        } else {
            let set = self
                .compiled
                .lang_personal_dict
                .entry(lang.as_str().to_string())
                .or_default();
            let is_new = set.insert(word_lower.clone());
            let canon_map = self
                .compiled
                .lang_personal_dict_canonical
                .entry(lang.as_str().to_string())
                .or_default();
            if !is_new {
                if word_trimmed.chars().any(char::is_uppercase)
                    && canon_map.get(&word_lower).is_none_or(|c| c != word_trimmed)
                {
                    canon_map.insert(word_lower.clone(), word_trimmed.to_string());
                    if let Some(arr) = self
                        .doc
                        .get_mut("lang")
                        .and_then(Item::as_table_mut)
                        .and_then(|t| t.get_mut(lang.as_str()))
                        .and_then(Item::as_table_mut)
                        .and_then(|t| t.get_mut("personal_dict"))
                        .and_then(Item::as_array_mut)
                    {
                        for item in arr.iter_mut() {
                            if item
                                .as_str()
                                .is_some_and(|s| s.to_lowercase() == word_lower)
                            {
                                *item = Value::from(word_trimmed);
                            }
                        }
                    }
                    let _ = self.flush_rules();
                }
                return;
            }
            canon_map.insert(word_lower.clone(), word_trimmed.to_string());
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
                arr.push(word_trimmed);
            }
        }
        let _ = self.flush_rules();
        self.log_rule_learned(&format!("personal_dict:{word_trimmed}"), stage, None);
    }

    /// Clears any staged `untouched_unknowns` counters for `word_lower` (both raw and lang-prefixed).
    pub fn clear_untouched_unknown(&mut self, lang: &LangId, word_lower: &str) {
        self.state.untouched_unknowns.remove(word_lower);
        let key = format!("{}:{word_lower}", lang.as_str());
        self.state.untouched_unknowns.remove(&key);
    }

    /// Clears any `1x` undo-keep shield and untouched-unknown staging for `word_lower` when the user
    /// manually edits `word_lower` into a different word.
    pub fn clear_undo_shield(&mut self, lang: &LangId, word_lower: &str) {
        self.session_shielded_words.remove(word_lower);
        let prefix = format!("{}:{word_lower}->", lang.as_str());
        self.state
            .staged_undo_keep
            .retain(|k, _| !k.starts_with(&prefix));
        self.clear_untouched_unknown(lang, word_lower);
    }

    /// Removes `word_lower` from `personal_dict` (used when a user repeatedly corrects or rejects
    /// a mistaken `personal_dict` entry — automatic "un-dictionary").
    pub fn remove_personal_word(&mut self, lang: &LangId, word_lower: &str) -> bool {
        self.sync_external_edits_before_write();
        self.sync_external_state_before_write();
        let word_l = word_lower.trim().to_lowercase();
        self.clear_undo_shield(lang, &word_l);
        let removed = if lang.as_str() == "en" {
            self.compiled.personal_dict_canonical.remove(&word_l);
            let did_remove = self.compiled.personal_dict.remove(&word_l);
            if did_remove {
                if let Some(arr) = self.doc.get_mut("personal_dict").and_then(Item::as_array_mut) {
                    arr.retain(|v| {
                        !v.as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&word_l) || s.to_lowercase() == word_l)
                    });
                }
            }
            did_remove
        } else {
            if let Some(m) = self.compiled.lang_personal_dict_canonical.get_mut(lang.as_str()) {
                m.remove(&word_l);
            }
            let did_remove = self
                .compiled
                .lang_personal_dict
                .get_mut(lang.as_str())
                .is_some_and(|s| s.remove(&word_l));
            if did_remove {
                if let Some(arr) = self
                    .doc
                    .get_mut("lang")
                    .and_then(Item::as_table_mut)
                    .and_then(|t| t.get_mut(lang.as_str()))
                    .and_then(Item::as_table_mut)
                    .and_then(|t| t.get_mut("personal_dict"))
                    .and_then(Item::as_array_mut)
                {
                    arr.retain(|v| {
                        !v.as_str()
                            .is_some_and(|s| s.eq_ignore_ascii_case(&word_l) || s.to_lowercase() == word_l)
                    });
                }
            }
            did_remove
        };
        if removed {
            let _ = self.flush_rules();
            let _ = self.flush_state();
            self.log_rule_learned(
                &format!("personal_dict:{word_l}"),
                "personal_demoted_2x",
                Some(word_l),
            );
        }
        removed
    }

    /// Permanently adds `typed_lower = fixed` to `[corrections]` (or `[lang.<id>.corrections]`).
    pub fn add_correction(&mut self, lang: &LangId, typed_lower: &str, fixed: &str) {
        self.sync_external_edits_before_write();
        let prev_target = if lang.as_str() == "en" {
            self.compiled
                .corrections
                .insert(typed_lower.to_string(), fixed.to_string())
        } else {
            self.compiled
                .lang_corrections
                .entry(lang.as_str().to_string())
                .or_default()
                .insert(typed_lower.to_string(), fixed.to_string())
        };
        let superseded = prev_target.as_deref().filter(|old| *old != fixed);

        let mut item_val = Value::from(fixed);
        if self.compiled.diagnostics.annotate_rules {
            let comment = self.format_provenance_comment("learned 2x", superseded);
            item_val.decor_mut().set_suffix(comment);
        }

        if lang.as_str() == "en" {
            let tbl = self
                .doc
                .entry("corrections")
                .or_insert(Item::Table(Table::new()))
                .as_table_mut()
                .unwrap();
            tbl[typed_lower] = Item::Value(item_val);
        } else {
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
            corr[typed_lower] = Item::Value(item_val);
        }
        let _ = self.flush_rules();

        let rule_str = format!("{typed_lower}->{fixed}");
        if let Some(old) = superseded {
            self.log_rule_learned(&rule_str, "superseded", Some(old.to_string()));
        } else if prev_target.is_none() {
            self.log_rule_learned(&rule_str, "graduated_2x", None);
        }
    }

    /// Permanently adds `"prev typed" = "fixed"` to `[context_corrections]`.
    pub fn add_context_correction(&mut self, prev_lower: &str, typed_lower: &str, fixed: &str) {
        self.sync_external_edits_before_write();
        let prev_target = self.compiled.context_corrections.insert(
            (prev_lower.to_string(), typed_lower.to_string()),
            fixed.to_string(),
        );
        let superseded = prev_target.as_deref().filter(|old| *old != fixed);

        let mut item_val = Value::from(fixed);
        if self.compiled.diagnostics.annotate_rules {
            let comment = self.format_provenance_comment("learned 2x", superseded);
            item_val.decor_mut().set_suffix(comment);
        }

        let key = format!("{prev_lower} {typed_lower}");
        let tbl = self
            .doc
            .entry("context_corrections")
            .or_insert(Item::Table(Table::new()))
            .as_table_mut()
            .unwrap();
        tbl[&key] = Item::Value(item_val);
        let _ = self.flush_rules();

        let rule_str = format!("{prev_lower} {typed_lower}->{fixed}");
        if let Some(old) = superseded {
            self.log_rule_learned(&rule_str, "superseded", Some(old.to_string()));
        } else if prev_target.is_none() {
            self.log_rule_learned(&rule_str, "graduated_2x", None);
        }
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
            vi_lotus_enabled: true,
            undo_window_ms: 5000,
            scorer_config: ScorerConfig::default(),
            ..CompiledRules::default()
        };

        if let Some(plugins) = doc.get("plugins").and_then(Item::as_table) {
            if let Some(b) = plugins
                .get("vi_lotus")
                .or_else(|| plugins.get("vi-lotus"))
                .and_then(Item::as_bool)
            {
                out.vi_lotus_enabled = b;
            }
        }

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
            if let Some(b) = settings.get("expand_shorthand").and_then(Item::as_bool) {
                out.scorer_config.expand_shorthand = b;
            }
        }

        if let Some(diag) = doc.get("diagnostics").and_then(Item::as_table) {
            if let Some(b) = diag.get("event_log").and_then(Item::as_bool) {
                out.diagnostics.event_log = b;
            }
            if let Some(lvl) = diag.get("log_level").and_then(Item::as_str) {
                out.diagnostics.log_level = EventLogLevel::parse(lvl);
            }
            if let Some(days) = diag.get("keep_days").and_then(Item::as_integer) {
                if days >= 0 {
                    out.diagnostics.keep_days = days as u32;
                }
            }
            if let Some(band) = diag
                .get("near_miss_band")
                .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            {
                out.diagnostics.near_miss_band = (band as f32).clamp(0.0, 1.0);
            }
            if let Some(b) = diag.get("annotate_rules").and_then(Item::as_bool) {
                out.diagnostics.annotate_rules = b;
            }
        }

        if let Some(apps) = doc.get("app_profiles").and_then(Item::as_table) {
            if let Some(pol) = apps.get("unknown_app_policy").and_then(Item::as_str) {
                out.app_profiles.unknown_app_policy = match pol.trim().to_ascii_lowercase().as_str()
                {
                    "bypass" | "off" | "disable" => Some(AppProfile::Bypass),
                    "browser" => Some(AppProfile::Browser),
                    "chat" => Some(AppProfile::Chat),
                    "prose" | "default" => Some(AppProfile::Prose),
                    _ => None,
                };
            }
            let load_set = |key: &str, dst: &mut BTreeSet<String>| {
                if let Some(arr) = apps.get(key).and_then(Item::as_array) {
                    for v in arr {
                        if let Some(s) = v.as_str() {
                            let norm = s.trim().to_ascii_lowercase();
                            if !norm.is_empty() {
                                dst.insert(norm);
                            }
                        }
                    }
                }
            };
            load_set("bypass", &mut out.app_profiles.bypass);
            load_set("browser", &mut out.app_profiles.browser);
            load_set("chat", &mut out.app_profiles.chat);
            load_set("prose", &mut out.app_profiles.prose);
        }

        if let Some(arr) = doc.get("personal_dict").and_then(Item::as_array) {
            for val in arr {
                if let Some(s) = val.as_str() {
                    let trimmed = s.trim();
                    let lower = trimmed.to_lowercase();
                    if !lower.is_empty() {
                        out.personal_dict.insert(lower.clone());
                        out.personal_dict_canonical.insert(lower, trimmed.to_string());
                    }
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
                        let canon_map = out
                            .lang_personal_dict_canonical
                            .entry(lang_key.to_string())
                            .or_default();
                        for val in arr {
                            if let Some(s) = val.as_str() {
                                let trimmed = s.trim();
                                let lower = trimmed.to_lowercase();
                                if !lower.is_empty() {
                                    set.insert(lower.clone());
                                    canon_map.insert(lower, trimmed.to_string());
                                }
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

    fn lookup_personal_canonical(&self, lower_word: &str) -> Option<&str> {
        let lang = self.active_lang.as_str();
        if lang != "en" {
            if let Some(c) = self
                .compiled
                .lang_personal_dict_canonical
                .get(lang)
                .and_then(|m| m.get(lower_word))
            {
                return Some(c.as_str());
            }
        }
        self.compiled
            .personal_dict_canonical
            .get(lower_word)
            .map(String::as_str)
    }

    fn is_undo_shielded(&self, lower_word: &str) -> bool {
        if self.session_shielded_words.contains(lower_word) {
            return true;
        }
        let prefix = format!("{}:{lower_word}->", self.active_lang.as_str());
        self.state
            .staged_undo_keep
            .keys()
            .any(|k| k.starts_with(&prefix))
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

    fn personal_words(&self) -> Option<&BTreeSet<String>> {
        Some(&self.compiled.personal_dict)
    }

    fn lang_personal_words(&self) -> Option<&BTreeSet<String>> {
        let lang = self.active_lang.as_str();
        if lang != "en" {
            self.compiled.lang_personal_dict.get(lang)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{KeyInput, SmartKeyEngine, SmartKeySession};

    fn temp_store_dir(tag: &str) -> PathBuf {
        let nonce = TMP_NONCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "smartkey_store_test_{tag}_{}_{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn atomic_writes_leave_no_tmp_files_and_quarantine_corrupt_state() {
        let dir = temp_store_dir("atomic_quarantine");
        let rules_path = dir.join("rules.toml");
        let state_path = dir.join("learner_state.json");

        let mut store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        store.add_correction(&LangId::en(), "fomr", "from");
        store.log_autocorrect_applied("fomr", "from", MatchKind::UserCorrectionRule);

        let leftovers: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "leftover tmp files: {leftovers:?}");

        // Write truncated JSON to `learner_state.json` -> reload quarantines it instead of silent loss
        let broken_json = b"{\"staged_corrections\": {\"en:fomr->from\": ";
        fs::write(&state_path, broken_json).unwrap();

        let reloaded =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        assert!(reloaded.state().applied_counts.is_empty());

        let quarantined: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.starts_with("learner_state.json.corrupt-"))
            })
            .collect();
        assert_eq!(quarantined.len(), 1);
        assert_eq!(fs::read(&quarantined[0]).unwrap(), broken_json);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_rules_toml_preserves_disabled_flags_and_surfaces_parse_lock_error() {
        let dir = temp_store_dir("parse_locked");
        let rules_path = dir.join("rules.toml");
        let state_path = dir.join("learner_state.json");

        let broken_toml = "\
version = 1
[settings]
enabled = false
[plugins]
vi_lotus = false
[corrections
fomr = \"from\"
";
        fs::write(&rules_path, broken_toml).unwrap();

        let mut store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        assert!(store.is_parse_locked());
        assert!(store.parse_error().is_some());
        assert!(
            !store.compiled().enabled,
            "broken rules.toml with enabled = false must NOT silently re-enable SmartKey"
        );
        assert!(
            !store.compiled().vi_lotus_enabled,
            "broken rules.toml with vi_lotus = false must NOT silently re-enable vi_lotus"
        );

        // Mutating setters must return InvalidData error instead of claiming success while no-oping
        let err = store.set_enabled(true).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read_to_string(&rules_path).unwrap(),
            broken_toml,
            "user's broken rules.toml must remain untouched in place"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_reset_and_learner_sync_external_edits_before_flushing() {
        let dir = temp_store_dir("sync_ordering");
        let rules_path = dir.join("rules.toml");
        let state_path = dir.join("learner_state.json");

        let store_addon =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store_addon).unwrap();
        let mut session = SmartKeySession::new();

        // Addon types an unknown word ("qwfp ") and applies a fix so `applied_counts` is populated
        for ch in "teh qwfp ".chars() {
            session.process_key(&mut engine, KeyInput::Char(ch), 100);
        }
        assert_eq!(
            engine.store().state().applied_counts.get("teh->the"),
            Some(&1)
        );

        // Simulate CLI running `smartkey disable` + `smartkey log --clear` in another process
        let mut store_cli =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        store_cli.set_enabled(false).unwrap();
        store_cli.clear_diagnostic_logs().unwrap();

        // Focus loss triggers `session.reset(&mut engine)`: must reload external edits BEFORE
        // flushing `pending_unknown`, so `applied_counts` stays cleared and `enabled` becomes false!
        let _ = session.reset(&mut engine);
        assert!(!engine.store().compiled().enabled);
        assert!(
            engine.store().state().applied_counts.is_empty(),
            "session.reset must not clobber external CLI log --clear"
        );

        let on_disk: LearnerState =
            serde_json::from_str(&fs::read_to_string(&state_path).unwrap()).unwrap();
        assert!(on_disk.applied_counts.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }
}
