use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs, Wrap,
};
use ratatui::Frame;
use smartkey_core::event_log::{current_epoch_ms, date_string};
use smartkey_core::{
    AppProfile, ConfidenceTier, DiagnosticEvent, EvalContext, EvalOutcome, EventLog, LangId,
    Preset, PruneKinds, PruneOptions, RuleStore, SmartKeyEngine,
};

use crate::{
    app_profile_label, collect_staged_entries, default_config_dir, detect_fcitx5_module_so,
    detect_fcitx5_running, format_diagnostic_event_line, load_rule_store_mut,
    load_rule_store_readonly, match_kind_slug, CliError, CliResult, ConfidenceCountsJson,
    LogFilter, StagedEntryJson,
};

const TAB_TITLES: [&str; 5] = [
    "1 Overview",
    "2 Rules & Staged",
    "3 Settings",
    "4 Apps",
    "5 Logs & Explain",
];

const BOOLEAN_SETTINGS: &[&str] = &[
    "settings.enabled",
    "settings.auto_capitalize_i",
    "settings.auto_capitalize_sentence",
    "settings.fix_shift_slips",
    "settings.fix_proper_nouns",
    "settings.enable_contractions",
    "settings.enable_missed_space_split",
    "settings.expand_shorthand",
    "plugins.vi_lotus",
    "diagnostics.event_log",
    "diagnostics.trace_log",
    "diagnostics.annotate_rules",
];

const DIALECT_OPTIONS: &[&str] = &["mixed", "en_US", "en_GB"];
const AGGR_OPTIONS: &[&str] = &["conservative", "balanced", "aggressive"];
const UNDO_MODE_OPTIONS: &[&str] = &["step", "immediate", "off"];
const LOG_LEVEL_OPTIONS: &[&str] = &["interesting", "all"];
const UNKNOWN_POLICY_OPTIONS: &[&str] = &["default", "prose", "bypass", "browser", "chat"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Rules,
    Settings,
    Apps,
    Logs,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Rules => 1,
            Self::Settings => 2,
            Self::Apps => 3,
            Self::Logs => 4,
        }
    }

    fn from_index(idx: usize) -> Self {
        match idx % 5 {
            0 => Self::Overview,
            1 => Self::Rules,
            2 => Self::Settings,
            3 => Self::Apps,
            _ => Self::Logs,
        }
    }

    fn next(self) -> Self {
        Self::from_index(self.index() + 1)
    }

    fn prev(self) -> Self {
        Self::from_index(self.index() + 4)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RulesSubView {
    Active,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddRuleKind {
    Fix,
    Context,
    Snippet,
    Word,
    Block,
    Passthrough,
}

impl AddRuleKind {
    fn next(self) -> Self {
        match self {
            Self::Fix => Self::Context,
            Self::Context => Self::Snippet,
            Self::Snippet => Self::Word,
            Self::Word => Self::Block,
            Self::Block => Self::Passthrough,
            Self::Passthrough => Self::Fix,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Fix => "Fix (typed=fixed or vi:typed=fixed)",
            Self::Context => "Context (prev typed=fixed)",
            Self::Snippet => "Snippet (trigger=expansion)",
            Self::Word => "Word (word or vi:word)",
            Self::Block => "Block (typed=rejected)",
            Self::Passthrough => "Telex passthrough (raw_ascii)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuleDeleteTarget {
    Fix { lang: LangId, typed: String },
    Context { prev: String, typed: String },
    Snippet { trigger: String },
    Word { lang: LangId, word: String },
    Block { typed: String, rejected: String },
    Passthrough { raw: String },
}

#[derive(Debug, Clone)]
struct ActiveRuleRow {
    kind: &'static str,
    display: String,
    tier: ConfidenceTier,
    origin: String,
    hits: u32,
    strikes: u32,
    delete_target: RuleDeleteTarget,
}

#[derive(Debug, Clone)]
struct AppOverrideRow {
    profile: AppProfile,
    app_id: String,
}

#[derive(Debug, Clone)]
struct LogRow {
    timestamp: String,
    app: Option<String>,
    lang: String,
    summary: String,
    event: DiagnosticEvent,
}

#[derive(Debug, Clone)]
enum ConfirmAction {
    DeleteActiveRule {
        display: String,
        target: RuleDeleteTarget,
    },
    DismissStaged {
        queue: String,
        key: String,
        display: String,
    },
    PruneRules,
    DeleteAppOverride {
        profile: AppProfile,
        app_id: String,
    },
    ClearLogs,
}

#[derive(Debug, Clone)]
enum Modal {
    Help,
    Confirm {
        prompt: String,
        action: ConfirmAction,
    },
    FilterInput {
        input: String,
    },
    AddRule {
        kind: AddRuleKind,
        input: String,
    },
    EditSetting {
        key: String,
        input: String,
    },
    AddApp {
        profile: AppProfile,
        input: String,
    },
    ExplainInput {
        input: String,
    },
}

struct OverviewSnapshot {
    enabled: bool,
    vi_lotus_enabled: bool,
    fcitx5_running: bool,
    lotus_so_path: Option<PathBuf>,
    counts: ConfidenceCountsJson,
    active_total: usize,
    staged_total: usize,
    applied_total: u64,
    undo_total: u64,
    manual_total: u64,
    recent_events: Vec<String>,
}

impl Default for OverviewSnapshot {
    fn default() -> Self {
        Self {
            enabled: true,
            vi_lotus_enabled: false,
            fcitx5_running: false,
            lotus_so_path: None,
            counts: ConfidenceCountsJson {
                manual: 0,
                confirmed: 0,
                provisional: 0,
                suspect: 0,
            },
            active_total: 0,
            staged_total: 0,
            applied_total: 0,
            undo_total: 0,
            manual_total: 0,
            recent_events: Vec::new(),
        }
    }
}

pub(crate) struct App {
    config_dir: PathBuf,
    log_dir: PathBuf,
    notify_reload: fn() -> bool,
    active_tab: Tab,
    rules_view: RulesSubView,
    parse_locked: bool,
    parse_error: Option<String>,
    status_msg: String,
    status_is_error: bool,
    modal: Option<Modal>,
    overview: OverviewSnapshot,
    active_rules: Vec<ActiveRuleRow>,
    filtered_rule_indices: Vec<usize>,
    rules_filter: String,
    active_rule_idx: usize,
    staged_entries: Vec<StagedEntryJson>,
    staged_idx: usize,
    settings: Vec<(&'static str, String)>,
    setting_idx: usize,
    unknown_app_policy: String,
    app_overrides: Vec<AppOverrideRow>,
    app_idx: usize,
    logs: Vec<LogRow>,
    filtered_log_indices: Vec<usize>,
    log_filter: LogFilter,
    log_idx: usize,
    explain_lines: Vec<String>,
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        ratatui::restore();
    }
}

/// Entrypoint for `smartkey tui` / `smartkey ui`.
pub(crate) fn cmd_tui(args: &[String]) -> CliResult {
    let mut dir: Option<PathBuf> = None;
    let mut log_dir: Option<PathBuf> = None;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "-h" | "--help" => {
                print_tui_help();
                return Ok(());
            }
            "--dir" => {
                idx += 1;
                let Some(val) = args.get(idx) else {
                    return Err(CliError::Usage("--dir requires a path".to_string()));
                };
                dir = Some(PathBuf::from(val));
            }
            "--log-dir" => {
                idx += 1;
                let Some(val) = args.get(idx) else {
                    return Err(CliError::Usage("--log-dir requires a path".to_string()));
                };
                log_dir = Some(PathBuf::from(val));
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument for tui: {other:?} (see `smartkey tui --help`)"
                )));
            }
        }
        idx += 1;
    }

    if !io::stdout().is_terminal() {
        return Err(CliError::Runtime(
            "smartkey tui requires an interactive terminal (stdout is not a TTY)".to_string(),
        ));
    }

    let cfg_dir = dir.unwrap_or_else(default_config_dir);
    let resolved_log_dir = log_dir.unwrap_or_else(smartkey_core::default_state_log_dir);
    let mut app = App::new(cfg_dir, resolved_log_dir, crate::notify_fcitx5_reload);

    let _guard = TerminalGuard;
    let mut terminal = ratatui::init();
    loop {
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| CliError::Runtime(format!("terminal draw failed: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| CliError::Runtime(format!("event poll failed: {e}")))?
        {
            if let Event::Key(key) = event::read()
                .map_err(|e| CliError::Runtime(format!("event read failed: {e}")))?
            {
                if key.kind == KeyEventKind::Press && app.handle_key(key) {
                    break;
                }
            }
        }
    }

    Ok(())
}

fn print_tui_help() {
    println!(
        "\
SmartKey Interactive TUI (`smartkey tui`)

USAGE:
  smartkey tui [--dir <PATH>] [--log-dir <PATH>]

OPTIONS:
  --dir <PATH>       Override config directory (default: ~/.config/smartkey)
  --log-dir <PATH>   Override event log directory (default: ~/.local/state/smartkey/logs)
  -h, --help         Show this help text

KEYBINDS:
  1..5 / Tab         Switch tabs (Overview, Rules & Staged, Settings, Apps, Logs & Explain)
  r                  Reload snapshot from disk
  ?                  Toggle keybind help modal
  q / Ctrl+C         Quit"
    );
}

impl App {
    pub(crate) fn new(config_dir: PathBuf, log_dir: PathBuf, notify_reload: fn() -> bool) -> Self {
        let mut app = Self {
            config_dir,
            log_dir,
            notify_reload,
            active_tab: Tab::Overview,
            rules_view: RulesSubView::Active,
            parse_locked: false,
            parse_error: None,
            status_msg: "Ready (? for help)".to_string(),
            status_is_error: false,
            modal: None,
            overview: OverviewSnapshot::default(),
            active_rules: Vec::new(),
            filtered_rule_indices: Vec::new(),
            rules_filter: String::new(),
            active_rule_idx: 0,
            staged_entries: Vec::new(),
            staged_idx: 0,
            settings: Vec::new(),
            setting_idx: 0,
            unknown_app_policy: "default".to_string(),
            app_overrides: Vec::new(),
            app_idx: 0,
            logs: Vec::new(),
            filtered_log_indices: Vec::new(),
            log_filter: LogFilter::All,
            log_idx: 0,
            explain_lines: vec!["Press 'x' to run live explain on a token.".to_string()],
        };
        app.refresh();
        app
    }

    pub(crate) fn refresh(&mut self) {
        let store = match load_rule_store_readonly(&self.config_dir, Preset::Balanced) {
            Ok((s, _exists)) => s,
            Err(err) => {
                self.set_error(format!("Failed to load rules: {}", format_cli_error(&err)));
                return;
            }
        };

        self.parse_locked = store.is_parse_locked();
        self.parse_error = store.parse_error().map(str::to_string);

        let c = store.compiled();
        let st = store.state();
        let (counts, active_rules) = collect_active_rule_rows(&store);
        let staged_entries = collect_staged_entries(st);

        self.overview = OverviewSnapshot {
            enabled: c.enabled,
            vi_lotus_enabled: c.vi_lotus_enabled,
            fcitx5_running: detect_fcitx5_running(),
            lotus_so_path: detect_fcitx5_module_so("liblotus.so"),
            counts,
            active_total: active_rules.len(),
            staged_total: staged_entries.len(),
            applied_total: st.applied_counts.values().map(|&v| u64::from(v)).sum(),
            undo_total: st.undo_counts.values().map(|&v| u64::from(v)).sum(),
            manual_total: st.manual_fix_counts.values().map(|&v| u64::from(v)).sum(),
            recent_events: st
                .recent_events
                .iter()
                .rev()
                .take(5)
                .map(format_diagnostic_event_line)
                .collect(),
        };

        self.active_rules = active_rules;
        self.rebuild_rule_filter();
        self.staged_entries = staged_entries;
        clamp_index(&mut self.staged_idx, self.staged_entries.len());

        self.settings = store.list_settings();
        clamp_index(&mut self.setting_idx, self.settings.len());

        self.unknown_app_policy = store
            .get_setting("app_profiles.unknown_app_policy")
            .unwrap_or_else(|| "default".to_string());
        self.app_overrides = collect_app_overrides(c);
        clamp_index(&mut self.app_idx, self.app_overrides.len());

        self.logs = load_logs(&self.log_dir, &st.recent_events);
        self.rebuild_log_filter();
    }

    fn set_status(&mut self, msg: impl Into<String>) {
        self.status_msg = msg.into();
        self.status_is_error = false;
    }

    fn set_error(&mut self, msg: impl Into<String>) {
        self.status_msg = msg.into();
        self.status_is_error = true;
    }

    fn rebuild_rule_filter(&mut self) {
        let needle = self.rules_filter.trim().to_ascii_lowercase();
        self.filtered_rule_indices = self
            .active_rules
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty()
                    || row.kind.to_ascii_lowercase().contains(&needle)
                    || row.display.to_ascii_lowercase().contains(&needle)
                    || row.tier.as_str().contains(&needle)
            })
            .map(|(i, _)| i)
            .collect();
        clamp_index(&mut self.active_rule_idx, self.filtered_rule_indices.len());
    }

    fn rebuild_log_filter(&mut self) {
        self.filtered_log_indices = self
            .logs
            .iter()
            .enumerate()
            .filter(|(_, row)| self.log_filter.matches(&row.event))
            .map(|(i, _)| i)
            .collect();
        clamp_index(&mut self.log_idx, self.filtered_log_indices.len());
    }

    fn mutate_store<F>(&mut self, op: F)
    where
        F: FnOnce(&mut RuleStore) -> Result<String, String>,
    {
        if self.parse_locked {
            self.set_error("rules.toml has syntax errors; fix it before modifying settings");
            return;
        }
        let mut store = match load_rule_store_mut(&self.config_dir, Preset::Balanced) {
            Ok(s) => s,
            Err(err) => {
                self.set_error(format!("Load failed: {}", format_cli_error(&err)));
                return;
            }
        };
        if store.is_parse_locked() {
            self.parse_locked = true;
            self.parse_error = store.parse_error().map(str::to_string);
            self.set_error("rules.toml has syntax errors; fix it before modifying settings");
            return;
        }
        match op(&mut store) {
            Ok(msg) => {
                let reloaded = (self.notify_reload)();
                self.refresh();
                if reloaded {
                    self.set_status(format!("{msg} (Fcitx5 reloaded)"));
                } else {
                    self.set_status(format!("{msg} (saved; Fcitx5 not signaled)"));
                }
            }
            Err(err) => {
                self.refresh();
                self.set_error(err);
            }
        }
    }

    /// Processes a single key press. Returns `true` when the event loop should exit.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return false;
        }
        if self.handle_global_key(key) {
            return matches!(key.code, KeyCode::Char('q'));
        }
        match self.active_tab {
            Tab::Overview => self.handle_overview_key(key),
            Tab::Rules => self.handle_rules_key(key),
            Tab::Settings => self.handle_settings_key(key),
            Tab::Apps => self.handle_apps_key(key),
            Tab::Logs => self.handle_logs_key(key),
        }
        false
    }

    fn handle_global_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') => true,
            KeyCode::Char('1') => {
                self.active_tab = Tab::Overview;
                true
            }
            KeyCode::Char('2') => {
                self.active_tab = Tab::Rules;
                true
            }
            KeyCode::Char('3') => {
                self.active_tab = Tab::Settings;
                true
            }
            KeyCode::Char('4') => {
                self.active_tab = Tab::Apps;
                true
            }
            KeyCode::Char('5') => {
                self.active_tab = Tab::Logs;
                true
            }
            KeyCode::Tab => {
                self.active_tab = self.active_tab.next();
                true
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.prev();
                true
            }
            KeyCode::Char('r') => {
                self.refresh();
                self.set_status("Refreshed from disk");
                true
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                true
            }
            _ => false,
        }
    }

    fn handle_overview_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('e') => {
                let next = !self.overview.enabled;
                self.mutate_store(|store| {
                    store
                        .set_enabled(next)
                        .map_err(|e| format!("Toggle enabled failed: {e}"))?;
                    Ok(format!(
                        "SmartKey {}",
                        if next { "enabled" } else { "disabled" }
                    ))
                });
            }
            KeyCode::Char('l') => {
                let next = !self.overview.vi_lotus_enabled;
                self.mutate_store(|store| {
                    store
                        .set_plugin_enabled("vi-lotus", next)
                        .map_err(|e| format!("Toggle vi-lotus failed: {e}"))?;
                    Ok(format!(
                        "vi-lotus {}",
                        if next { "enabled" } else { "disabled" }
                    ))
                });
            }
            KeyCode::Char('R') => {
                if (self.notify_reload)() {
                    self.set_status("Signaled Fcitx5 addon reload");
                } else {
                    self.set_error("Fcitx5 reload signal not acknowledged (daemon offline?)");
                }
            }
            _ => {}
        }
    }

    fn handle_rules_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('v') | KeyCode::Left | KeyCode::Right => {
                self.rules_view = match self.rules_view {
                    RulesSubView::Active => RulesSubView::Staged,
                    RulesSubView::Staged => RulesSubView::Active,
                };
            }
            KeyCode::Down | KeyCode::Char('j') => match self.rules_view {
                RulesSubView::Active => {
                    move_selection(&mut self.active_rule_idx, 1, self.filtered_rule_indices.len());
                }
                RulesSubView::Staged => {
                    move_selection(&mut self.staged_idx, 1, self.staged_entries.len());
                }
            },
            KeyCode::Up | KeyCode::Char('k') => match self.rules_view {
                RulesSubView::Active => {
                    move_selection(
                        &mut self.active_rule_idx,
                        -1,
                        self.filtered_rule_indices.len(),
                    );
                }
                RulesSubView::Staged => {
                    move_selection(&mut self.staged_idx, -1, self.staged_entries.len());
                }
            },
            KeyCode::Char('/') if self.rules_view == RulesSubView::Active => {
                self.modal = Some(Modal::FilterInput {
                    input: self.rules_filter.clone(),
                });
            }
            KeyCode::Esc if self.rules_view == RulesSubView::Active && !self.rules_filter.is_empty() =>
            {
                self.rules_filter.clear();
                self.rebuild_rule_filter();
                self.set_status("Cleared rule filter");
            }
            KeyCode::Char('a') if self.rules_view == RulesSubView::Active => {
                self.modal = Some(Modal::AddRule {
                    kind: AddRuleKind::Fix,
                    input: String::new(),
                });
            }
            KeyCode::Char('P') if self.rules_view == RulesSubView::Active => {
                self.modal = Some(Modal::Confirm {
                    prompt: "Prune unused auto-learned fixes older than 14 days (0 hits)?".to_string(),
                    action: ConfirmAction::PruneRules,
                });
            }
            KeyCode::Char('p') if self.rules_view == RulesSubView::Staged => {
                self.promote_selected_staged();
            }
            KeyCode::Char('d') | KeyCode::Delete => match self.rules_view {
                RulesSubView::Active => {
                    if let Some(&row_idx) = self.filtered_rule_indices.get(self.active_rule_idx) {
                        let row = &self.active_rules[row_idx];
                        self.modal = Some(Modal::Confirm {
                            prompt: format!("Delete {} rule '{}'?", row.kind, row.display),
                            action: ConfirmAction::DeleteActiveRule {
                                display: row.display.clone(),
                                target: row.delete_target.clone(),
                            },
                        });
                    }
                }
                RulesSubView::Staged => {
                    if let Some(entry) = self.staged_entries.get(self.staged_idx) {
                        self.modal = Some(Modal::Confirm {
                            prompt: format!(
                                "Dismiss staged [{}] '{}'?",
                                entry.queue, entry.display
                            ),
                            action: ConfirmAction::DismissStaged {
                                queue: entry.queue.to_string(),
                                key: entry.key.clone(),
                                display: entry.display.clone(),
                            },
                        });
                    }
                }
            },
            _ => {}
        }
    }

    fn promote_selected_staged(&mut self) {
        let Some((queue, key, display, canonical)) =
            self.staged_entries.get(self.staged_idx).map(|entry| {
                (
                    entry.queue,
                    entry.key.clone(),
                    entry.display.clone(),
                    entry.canonical.clone(),
                )
            })
        else {
            return;
        };

        if !matches!(
            queue,
            "fix" | "case_fix" | "context" | "untouched_word" | "telex_passthrough"
        ) {
            self.set_status(format!(
                "Cannot promote queue '{queue}'; press d to dismiss"
            ));
            return;
        }

        self.mutate_store(|store| {
            promote_staged_entry(store, queue, &key, canonical.as_deref())?;
            store
                .dismiss_staged_entry(queue, &key)
                .map_err(|e| format!("Dismiss after promote failed: {e}"))?;
            Ok(format!("Promoted [{queue}] {display}"))
        });
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                move_selection(&mut self.setting_idx, 1, self.settings.len());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_selection(&mut self.setting_idx, -1, self.settings.len());
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                let Some((setting_key, current_val)) = self.settings.get(self.setting_idx).cloned()
                else {
                    return;
                };
                if BOOLEAN_SETTINGS.contains(&setting_key) {
                    let next = if current_val == "true" {
                        "false"
                    } else {
                        "true"
                    };
                    self.apply_setting(setting_key, next);
                } else if let Some(options) = enum_setting_options(setting_key) {
                    let cur_pos = options.iter().position(|&v| v == current_val).unwrap_or(0);
                    let next = options[(cur_pos + 1) % options.len()];
                    self.apply_setting(setting_key, next);
                } else {
                    self.modal = Some(Modal::EditSetting {
                        key: setting_key.to_string(),
                        input: current_val,
                    });
                }
            }
            _ => {}
        }
    }

    fn apply_setting(&mut self, key: &str, value: &str) {
        let key_owned = key.to_string();
        let val_owned = value.to_string();
        self.mutate_store(|store| {
            let (canon, saved) = store
                .set_setting(&key_owned, &val_owned)
                .map_err(|e| format!("Update {key_owned} failed: {e}"))?;
            Ok(format!("{canon} = {saved}"))
        });
    }

    fn handle_apps_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                move_selection(&mut self.app_idx, 1, self.app_overrides.len());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_selection(&mut self.app_idx, -1, self.app_overrides.len());
            }
            KeyCode::Char('u') => {
                let cur_pos = UNKNOWN_POLICY_OPTIONS
                    .iter()
                    .position(|&v| v == self.unknown_app_policy)
                    .unwrap_or(0);
                let next = UNKNOWN_POLICY_OPTIONS[(cur_pos + 1) % UNKNOWN_POLICY_OPTIONS.len()];
                self.apply_setting("app_profiles.unknown_app_policy", next);
            }
            KeyCode::Char('a') => {
                self.modal = Some(Modal::AddApp {
                    profile: AppProfile::Bypass,
                    input: String::new(),
                });
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(row) = self.app_overrides.get(self.app_idx) {
                    self.modal = Some(Modal::Confirm {
                        prompt: format!(
                            "Remove {} override for '{}'?",
                            app_profile_label(row.profile),
                            row.app_id
                        ),
                        action: ConfirmAction::DeleteAppOverride {
                            profile: row.profile,
                            app_id: row.app_id.clone(),
                        },
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_logs_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                move_selection(&mut self.log_idx, 1, self.filtered_log_indices.len());
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_selection(&mut self.log_idx, -1, self.filtered_log_indices.len());
            }
            KeyCode::Char('f') => {
                self.log_filter = match self.log_filter {
                    LogFilter::All => LogFilter::FixedOnly,
                    LogFilter::FixedOnly => LogFilter::ManualOnly,
                    LogFilter::ManualOnly => LogFilter::UnfixedOnly,
                    LogFilter::UnfixedOnly => LogFilter::All,
                };
                self.rebuild_log_filter();
                self.set_status(format!("Log filter: {}", log_filter_label(self.log_filter)));
            }
            KeyCode::Char('x') => {
                self.modal = Some(Modal::ExplainInput {
                    input: String::new(),
                });
            }
            KeyCode::Char('c') => {
                self.modal = Some(Modal::Confirm {
                    prompt: "Clear all durable event logs and in-memory history?".to_string(),
                    action: ConfirmAction::ClearLogs,
                });
            }
            _ => {}
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Help => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {}
                _ => self.modal = Some(Modal::Help),
            },
            Modal::Confirm { prompt, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.execute_confirm(action);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.set_status("Cancelled");
                }
                _ => self.modal = Some(Modal::Confirm { prompt, action }),
            },
            Modal::FilterInput { mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    self.rules_filter = input;
                    self.rebuild_rule_filter();
                    let note = if self.rules_filter.trim().is_empty() {
                        "Cleared rule filter".to_string()
                    } else {
                        format!(
                            "Filter '{}' ({} matches)",
                            self.rules_filter.trim(),
                            self.filtered_rule_indices.len()
                        )
                    };
                    self.set_status(note);
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::FilterInput { input });
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::FilterInput { input });
                }
                _ => self.modal = Some(Modal::FilterInput { input }),
            },
            Modal::AddRule { kind, mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Tab => {
                    self.modal = Some(Modal::AddRule {
                        kind: kind.next(),
                        input,
                    });
                }
                KeyCode::Enter => {
                    let trimmed = input.trim().to_string();
                    if trimmed.is_empty() {
                        self.set_error("Rule input cannot be empty");
                    } else {
                        self.execute_add_rule(kind, &trimmed);
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::AddRule { kind, input });
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::AddRule { kind, input });
                }
                _ => self.modal = Some(Modal::AddRule { kind, input }),
            },
            Modal::EditSetting { key: skey, mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let val = input.trim().to_string();
                    self.apply_setting(&skey, &val);
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::EditSetting { key: skey, input });
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::EditSetting { key: skey, input });
                }
                _ => self.modal = Some(Modal::EditSetting { key: skey, input }),
            },
            Modal::AddApp {
                profile,
                mut input,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Tab => {
                    self.modal = Some(Modal::AddApp {
                        profile: next_app_profile(profile),
                        input,
                    });
                }
                KeyCode::Enter => {
                    let app_id = input.trim().to_string();
                    if app_id.is_empty() {
                        self.set_error("App ID cannot be empty");
                    } else {
                        self.mutate_store(|store| {
                            let added = store
                                .add_app_profile_override(profile, &app_id)
                                .map_err(|e| format!("Add app override failed: {e}"))?;
                            if added {
                                Ok(format!(
                                    "Added {} override '{}'",
                                    app_profile_label(profile),
                                    app_id
                                ))
                            } else {
                                Ok(format!("Override '{app_id}' already present"))
                            }
                        });
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::AddApp { profile, input });
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::AddApp { profile, input });
                }
                _ => self.modal = Some(Modal::AddApp { profile, input }),
            },
            Modal::ExplainInput { mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let query = input.trim().to_string();
                    if !query.is_empty() {
                        self.run_explain_query(&query);
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::ExplainInput { input });
                }
                KeyCode::Char(c) => {
                    input.push(c);
                    self.modal = Some(Modal::ExplainInput { input });
                }
                _ => self.modal = Some(Modal::ExplainInput { input }),
            },
        }
    }

    fn execute_confirm(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::DeleteActiveRule { display, target } => {
                self.mutate_store(|store| {
                    let removed = match &target {
                        RuleDeleteTarget::Fix { lang, typed } => store.remove_correction(lang, typed),
                        RuleDeleteTarget::Context { prev, typed } => {
                            store.remove_context_correction(prev, typed)
                        }
                        RuleDeleteTarget::Snippet { trigger } => store.remove_snippet(trigger),
                        RuleDeleteTarget::Word { lang, word } => {
                            store.remove_user_personal_word(lang, word)
                        }
                        RuleDeleteTarget::Block { typed, rejected } => {
                            store.remove_blocklist_pair(typed, rejected)
                        }
                        RuleDeleteTarget::Passthrough { raw } => {
                            store.remove_telex_passthrough(raw)
                        }
                    }
                    .map_err(|e| format!("Delete rule failed: {e}"))?;
                    if removed {
                        Ok(format!("Deleted '{display}'"))
                    } else {
                        Ok(format!("Rule '{display}' was not found"))
                    }
                });
            }
            ConfirmAction::DismissStaged {
                queue,
                key,
                display,
            } => {
                self.mutate_store(|store| {
                    let removed = store
                        .dismiss_staged_entry(&queue, &key)
                        .map_err(|e| format!("Dismiss staged failed: {e}"))?;
                    if removed {
                        Ok(format!("Dismissed [{queue}] '{display}'"))
                    } else {
                        Ok(format!("Staged entry '{display}' already gone"))
                    }
                });
            }
            ConfirmAction::PruneRules => {
                let now_ms = current_epoch_ms();
                let opts = PruneOptions {
                    older_than_ms: 14 * 86_400_000,
                    max_hits: 0,
                    kinds: PruneKinds::default(),
                    include_manual: false,
                    dry_run: false,
                };
                self.mutate_store(|store| {
                    let report = store
                        .prune_rules(now_ms, &opts)
                        .map_err(|e| format!("Prune failed: {e}"))?;
                    Ok(format!("Pruned {} stale rule(s)", report.removed_count))
                });
            }
            ConfirmAction::DeleteAppOverride { profile, app_id } => {
                self.mutate_store(|store| {
                    let removed = store
                        .remove_app_profile_override(Some(profile), &app_id)
                        .map_err(|e| format!("Remove app override failed: {e}"))?;
                    if removed {
                        Ok(format!(
                            "Removed {} override '{app_id}'",
                            app_profile_label(profile)
                        ))
                    } else {
                        Ok(format!("Override '{app_id}' not found"))
                    }
                });
            }
            ConfirmAction::ClearLogs => {
                let log_dir = self.log_dir.clone();
                self.mutate_store(|store| {
                    store
                        .clear_diagnostic_logs()
                        .map_err(|e| format!("Clear learner logs failed: {e}"))?;
                    let files = EventLog::clear_all_files(&log_dir)
                        .map_err(|e| format!("Clear log files failed: {e}"))?;
                    Ok(format!("Cleared diagnostic logs ({files} file(s))"))
                });
            }
        }
    }

    fn execute_add_rule(&mut self, kind: AddRuleKind, raw: &str) {
        let input = raw.to_string();
        self.mutate_store(|store| match kind {
            AddRuleKind::Fix => {
                let (lang, rest) = split_optional_lang(&input);
                let Some((typed, fixed)) = split_pair(rest, &["->", "="]) else {
                    return Err("Expected typed=fixed or vi:typed=fixed".to_string());
                };
                store
                    .set_user_correction(&lang, typed, fixed)
                    .map_err(|e| format!("Add fix failed: {e}"))?;
                Ok(format!("Saved fix [{}] {typed} -> {fixed}", lang.as_str()))
            }
            AddRuleKind::Context => {
                let Some((lhs, fixed)) = split_pair(&input, &["->", "="]) else {
                    return Err("Expected 'prev typed=fixed'".to_string());
                };
                let Some((prev, typed)) = split_context_lhs(lhs) else {
                    return Err("Expected two words before '=': 'prev typed=fixed'".to_string());
                };
                store
                    .set_user_context_correction(prev, typed, fixed)
                    .map_err(|e| format!("Add context failed: {e}"))?;
                Ok(format!("Saved context '{prev} {typed}' -> {fixed}"))
            }
            AddRuleKind::Snippet => {
                let Some((trigger, expansion)) = split_pair(&input, &["->", "="]) else {
                    return Err("Expected trigger=expansion".to_string());
                };
                store
                    .set_snippet(trigger, expansion)
                    .map_err(|e| format!("Add snippet failed: {e}"))?;
                Ok(format!("Saved snippet {trigger} -> {expansion}"))
            }
            AddRuleKind::Word => {
                let (lang, word) = split_optional_lang(&input);
                let trimmed = word.trim();
                if trimmed.is_empty() {
                    return Err("Word cannot be empty".to_string());
                }
                store
                    .add_user_personal_word(&lang, trimmed)
                    .map_err(|e| format!("Add word failed: {e}"))?;
                Ok(format!("Added personal word [{}] {trimmed}", lang.as_str()))
            }
            AddRuleKind::Block => {
                let Some((typed, rejected)) = split_pair(&input, &["!->", "!=", "="]) else {
                    return Err("Expected typed=rejected".to_string());
                };
                store
                    .add_user_blocklist_pair(typed, rejected)
                    .map_err(|e| format!("Add block failed: {e}"))?;
                Ok(format!("Blocked {typed} !-> {rejected}"))
            }
            AddRuleKind::Passthrough => {
                let raw_ascii = input.trim();
                if raw_ascii.is_empty() {
                    return Err("Passthrough token cannot be empty".to_string());
                }
                store
                    .add_user_telex_passthrough(raw_ascii)
                    .map_err(|e| format!("Add passthrough failed: {e}"))?;
                Ok(format!("Added Telex passthrough '{raw_ascii}'"))
            }
        });
    }

    fn run_explain_query(&mut self, query: &str) {
        let (lang, rest) = split_optional_lang(query);
        let trimmed = rest.trim();
        let (prev, token) = match trimmed.rsplit_once(char::is_whitespace) {
            Some((p, t)) => (Some(p.trim()), t.trim()),
            None => (None, trimmed),
        };
        if token.is_empty() {
            self.set_error("Explain query token cannot be empty");
            return;
        }
        let (mut store, _) = match load_rule_store_readonly(&self.config_dir, Preset::Balanced) {
            Ok(pair) => pair,
            Err(err) => {
                self.set_error(format!("Explain load failed: {}", format_cli_error(&err)));
                return;
            }
        };
        store.set_active_lang(lang.clone());
        let engine = SmartKeyEngine::new_auto(store);
        let ctx = EvalContext {
            prev_word: prev,
            ..EvalContext::default()
        };
        let exp = engine.explain_token(token, &ctx);

        let mut lines = Vec::new();
        let verdict = match &exp.outcome {
            EvalOutcome::Autocorrect {
                replacement,
                kind,
                score,
                margin,
            } => format!(
                "[{lang_s}] {token:?} -> {replacement:?} ({}, score={score:.2}, margin={margin:.2})",
                match_kind_slug(*kind),
                lang_s = lang.as_str(),
            ),
            EvalOutcome::Unchanged {
                reason,
                do_not_learn,
            } => format!(
                "[{lang_s}] {token:?} -> UNCHANGED ({}, do_not_learn={do_not_learn})",
                match_kind_slug(*reason),
                lang_s = lang.as_str(),
            ),
        };
        lines.push(verdict);
        if exp.top_candidates.is_empty() {
            lines.push("No candidates ranked.".to_string());
        } else {
            for (idx, c) in exp.top_candidates.iter().take(5).enumerate() {
                lines.push(format!(
                    "  #{:<2} {:<16} score={:<6.2} edit={:<5.2} freq={:<3} [{}]",
                    idx + 1,
                    c.replacement,
                    c.score,
                    c.edit_cost,
                    c.freq,
                    match_kind_slug(c.kind)
                ));
            }
        }
        self.explain_lines = lines;
        self.set_status(format!("Explained '{token}'"));
    }

    pub(crate) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 4 || area.height < 4 {
            return;
        }

        let mut constraints = vec![Constraint::Length(2)];
        if self.parse_locked {
            constraints.push(Constraint::Length(3));
        }
        constraints.push(Constraint::Min(3));
        constraints.push(Constraint::Length(2));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let mut cursor = 0usize;
        self.render_header(frame, chunks[cursor]);
        cursor += 1;

        if self.parse_locked {
            self.render_parse_lock_banner(frame, chunks[cursor]);
            cursor += 1;
        }

        let body_area = chunks[cursor];
        let footer_area = chunks[cursor + 1];

        match self.active_tab {
            Tab::Overview => self.render_overview(frame, body_area),
            Tab::Rules => self.render_rules(frame, body_area),
            Tab::Settings => self.render_settings(frame, body_area),
            Tab::Apps => self.render_apps(frame, body_area),
            Tab::Logs => self.render_logs(frame, body_area),
        }

        self.render_footer(frame, footer_area);

        if let Some(modal) = &self.modal {
            self.render_modal(frame, area, modal);
        }
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let titles: Vec<Line> = TAB_TITLES.iter().map(|t| Line::from(*t)).collect();
        let state_badge = if self.overview.enabled {
            Span::styled(" enabled ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(" disabled ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        };
        let title_line = Line::from(vec![
            Span::styled(" SmartKey ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Span::raw("| "),
            state_badge,
        ]);
        let tabs = Tabs::new(titles)
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(Color::DarkGray))
                    .title(title_line),
            )
            .select(self.active_tab.index())
            .style(Style::default().fg(Color::Gray))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
            );
        frame.render_widget(tabs, area);
    }

    fn render_parse_lock_banner(&self, frame: &mut Frame, area: Rect) {
        let detail = self
            .parse_error
            .as_deref()
            .unwrap_or("TOML syntax error in rules.toml");
        let text = format!(
            "Parse lock active: edits are blocked until rules.toml is valid.\nError: {detail}"
        );
        let banner = Paragraph::new(text)
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red))
                    .title(" rules.toml Parse Lock "),
            );
        frame.render_widget(banner, area);
    }

    fn render_overview(&self, frame: &mut Frame, area: Rect) {
        let cols = if area.width >= 72 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(7), Constraint::Min(3)])
                .split(area)
        };

        let left_lines = vec![
            kv_line(
                "Autocorrect",
                if self.overview.enabled { "enabled" } else { "disabled" },
                if self.overview.enabled { Color::Green } else { Color::Red },
            ),
            kv_line(
                "Fcitx5 daemon",
                if self.overview.fcitx5_running { "running" } else { "offline" },
                if self.overview.fcitx5_running { Color::Green } else { Color::Yellow },
            ),
            kv_line(
                "vi-lotus hook",
                if self.overview.vi_lotus_enabled { "enabled" } else { "disabled" },
                if self.overview.vi_lotus_enabled { Color::Green } else { Color::Yellow },
            ),
            Line::from(vec![
                Span::styled("Lotus module   : ", Style::default().fg(Color::Gray)),
                Span::raw(
                    self.overview
                        .lotus_so_path
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "not detected".to_string()),
                ),
            ]),
            Line::from(vec![
                Span::styled("Config dir     : ", Style::default().fg(Color::Gray)),
                Span::raw(self.config_dir.display().to_string()),
            ]),
            Line::from(vec![
                Span::styled("Staged queue   : ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!("{} pending", self.overview.staged_total),
                    Style::default().fg(if self.overview.staged_total > 0 {
                        Color::Yellow
                    } else {
                        Color::Gray
                    }),
                ),
            ]),
        ];
        let left_block = Paragraph::new(left_lines)
            .wrap(Wrap { trim: true })
            .block(neutral_block(" Status & Plugins (e: toggle, l: vi-lotus, R: reload) "));
        frame.render_widget(left_block, cols[0]);

        let c = &self.overview.counts;
        let mut right_lines = vec![
            Line::from(vec![
                Span::styled("Active rules   : ", Style::default().fg(Color::Gray)),
                Span::styled(
                    self.overview.active_total.to_string(),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("  manual       : ", Style::default().fg(Color::Gray)),
                Span::styled(c.manual.to_string(), Style::default().fg(Color::Green)),
                Span::raw("   confirmed: "),
                Span::styled(c.confirmed.to_string(), Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::styled("  provisional  : ", Style::default().fg(Color::Gray)),
                Span::styled(c.provisional.to_string(), Style::default().fg(Color::Yellow)),
                Span::raw("   suspect  : "),
                Span::styled(c.suspect.to_string(), Style::default().fg(Color::Red)),
            ]),
            Line::from(vec![
                Span::styled("Counters       : ", Style::default().fg(Color::Gray)),
                Span::raw(format!(
                    "applied={}  undo={}  manual={}",
                    self.overview.applied_total,
                    self.overview.undo_total,
                    self.overview.manual_total
                )),
            ]),
            Line::from(Span::styled("Recent events:", Style::default().fg(Color::Cyan))),
        ];
        if self.overview.recent_events.is_empty() {
            right_lines.push(Line::from(Span::styled(
                "  (no recent events)",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for ev in &self.overview.recent_events {
                right_lines.push(Line::from(format!("  {ev}")));
            }
        }
        let right_block = Paragraph::new(right_lines)
            .wrap(Wrap { trim: true })
            .block(neutral_block(" Telemetry & Rules "));
        frame.render_widget(right_block, cols[1]);
    }

    fn render_rules(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(area);

        let sub_tabs = Tabs::new(vec![
            Line::from(format!("Active Rules ({})", self.filtered_rule_indices.len())),
            Line::from(format!("Staged Queue ({})", self.staged_entries.len())),
        ])
        .select(match self.rules_view {
            RulesSubView::Active => 0,
            RulesSubView::Staged => 1,
        })
        .style(Style::default().fg(Color::Gray))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(neutral_block(match self.rules_view {
            RulesSubView::Active => " Rules & Staged (v: switch, /: filter, a: add, d: delete, P: prune) ",
            RulesSubView::Staged => " Rules & Staged (v: switch, p: promote, d: dismiss) ",
        }));
        frame.render_widget(sub_tabs, chunks[0]);

        match self.rules_view {
            RulesSubView::Active => self.render_active_rules_table(frame, chunks[1]),
            RulesSubView::Staged => self.render_staged_table(frame, chunks[1]),
        }
    }

    fn render_active_rules_table(&self, frame: &mut Frame, area: Rect) {
        let header = Row::new(vec!["Kind", "Rule", "Tier", "Origin", "Hits", "Strikes"])
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));

        let rows: Vec<Row> = self
            .filtered_rule_indices
            .iter()
            .map(|&idx| {
                let r = &self.active_rules[idx];
                Row::new(vec![
                    Cell::from(r.kind),
                    Cell::from(r.display.clone()),
                    Cell::from(Span::styled(r.tier.as_str(), tier_style(r.tier))),
                    Cell::from(r.origin.clone()),
                    Cell::from(r.hits.to_string()),
                    Cell::from(r.strikes.to_string()),
                ])
            })
            .collect();

        let title = if self.rules_filter.trim().is_empty() {
            " Active Rules ".to_string()
        } else {
            format!(" Active Rules [filter: {}] ", self.rules_filter.trim())
        };
        let table = Table::new(
            rows,
            [
                Constraint::Length(11),
                Constraint::Percentage(46),
                Constraint::Length(12),
                Constraint::Length(12),
                Constraint::Length(6),
                Constraint::Length(7),
            ],
        )
        .header(header)
        .block(neutral_block(&title))
        .row_highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );

        let mut state = TableState::default();
        if !self.filtered_rule_indices.is_empty() {
            state.select(Some(self.active_rule_idx));
        }
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn render_staged_table(&self, frame: &mut Frame, area: Rect) {
        let header = Row::new(vec!["Queue", "Entry", "Progress", "Canonical"])
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = self
            .staged_entries
            .iter()
            .map(|e| {
                let prog_style = if e.count + 1 >= e.threshold {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::Gray)
                };
                Row::new(vec![
                    Cell::from(e.queue),
                    Cell::from(e.display.clone()),
                    Cell::from(Span::styled(
                        format!("{}/{}", e.count, e.threshold),
                        prog_style,
                    )),
                    Cell::from(e.canonical.as_deref().unwrap_or("-")),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(18),
                Constraint::Percentage(46),
                Constraint::Length(10),
                Constraint::Percentage(26),
            ],
        )
        .header(header)
        .block(neutral_block(" Staged Learning Queue "))
        .row_highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );

        let mut state = TableState::default();
        if !self.staged_entries.is_empty() {
            state.select(Some(self.staged_idx));
        }
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn render_settings(&self, frame: &mut Frame, area: Rect) {
        let header = Row::new(vec!["Key", "Value", "Control"])
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = self
            .settings
            .iter()
            .map(|(k, v)| {
                let control = if BOOLEAN_SETTINGS.contains(k) {
                    "toggle"
                } else if enum_setting_options(k).is_some() {
                    "cycle"
                } else {
                    "edit"
                };
                let val_style = match v.as_str() {
                    "true" => Style::default().fg(Color::Green),
                    "false" => Style::default().fg(Color::Yellow),
                    _ => Style::default().fg(Color::Reset),
                };
                Row::new(vec![
                    Cell::from(*k),
                    Cell::from(Span::styled(v.clone(), val_style)),
                    Cell::from(Span::styled(control, Style::default().fg(Color::DarkGray))),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Percentage(55),
                Constraint::Percentage(30),
                Constraint::Length(10),
            ],
        )
        .header(header)
        .block(neutral_block(" Settings (Space/Enter: toggle, cycle, or edit) "))
        .row_highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        );

        let mut state = TableState::default();
        if !self.settings.is_empty() {
            state.select(Some(self.setting_idx));
        }
        frame.render_stateful_widget(table, area, &mut state);
    }

    fn render_apps(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(3)])
            .split(area);

        let policy_line = Line::from(vec![
            Span::styled("Unknown app fallback policy: ", Style::default().fg(Color::Gray)),
            Span::styled(
                self.unknown_app_policy.clone(),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  (press u to cycle)", Style::default().fg(Color::DarkGray)),
        ]);
        let policy_widget = Paragraph::new(policy_line)
            .block(neutral_block(" App Profile Policy "));
        frame.render_widget(policy_widget, chunks[0]);

        let header = Row::new(vec!["Profile", "App ID"])
            .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = self
            .app_overrides
            .iter()
            .map(|r| {
                let pstyle = match r.profile {
                    AppProfile::Bypass => Style::default().fg(Color::Red),
                    AppProfile::Browser => Style::default().fg(Color::Yellow),
                    AppProfile::Chat => Style::default().fg(Color::Cyan),
                    AppProfile::Prose => Style::default().fg(Color::Green),
                };
                Row::new(vec![
                    Cell::from(Span::styled(app_profile_label(r.profile), pstyle)),
                    Cell::from(r.app_id.clone()),
                ])
            })
            .collect();

        let table = Table::new(rows, [Constraint::Length(14), Constraint::Min(10)])
            .header(header)
            .block(neutral_block(" App Overrides (a: add override, d: delete) "))
            .row_highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            );
        let mut state = TableState::default();
        if !self.app_overrides.is_empty() {
            state.select(Some(self.app_idx));
        }
        frame.render_stateful_widget(table, chunks[1], &mut state);
    }

    fn render_logs(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(8)])
            .split(area);

        let items: Vec<ListItem> = self
            .filtered_log_indices
            .iter()
            .map(|&idx| {
                let row = &self.logs[idx];
                let app_tag = row.app.as_deref().unwrap_or("-");
                ListItem::new(format!(
                    "[{}] [{}] ({}) {}",
                    row.timestamp, row.lang, app_tag, row.summary
                ))
            })
            .collect();

        let title = format!(
            " Diagnostic Logs [filter: {}] (f: filter, x: explain, c: clear) ",
            log_filter_label(self.log_filter)
        );
        let list = List::new(items)
            .block(neutral_block(&title))
            .highlight_style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED),
            );
        let mut state = ListState::default();
        if !self.filtered_log_indices.is_empty() {
            state.select(Some(self.log_idx));
        }
        frame.render_stateful_widget(list, chunks[0], &mut state);

        let explain_text: Vec<Line> = self
            .explain_lines
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect();
        let explain_widget = Paragraph::new(explain_text)
            .wrap(Wrap { trim: true })
            .block(neutral_block(" Explain Output (x: explain token, e.g. 'teh', 'over ther', 'vi:dduowcj') "));
        frame.render_widget(explain_widget, chunks[1]);
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let msg_style = if self.status_is_error {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let line = Line::from(vec![
            Span::styled(&self.status_msg, msg_style),
            Span::styled(
                "   | 1-5 tabs  r refresh  ? help  q quit",
                Style::default().fg(Color::DarkGray),
            ),
        ]);
        let footer = Paragraph::new(line).block(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(footer, area);
    }

    fn render_modal(&self, frame: &mut Frame, area: Rect, modal: &Modal) {
        let popup = centered_rect(area, 76, 12);
        frame.render_widget(Clear, popup);

        let (title, lines) = match modal {
            Modal::Help => (
                " Keybinds Help (Esc/Enter to close) ",
                vec![
                    Line::from("Global  : 1-5 / Tab switch tab | r refresh | q / Ctrl+C quit"),
                    Line::from("Overview: e toggle SmartKey | l toggle vi-lotus | R signal reload"),
                    Line::from("Rules   : v switch active/staged | / filter | a add | d delete"),
                    Line::from("          P prune unused auto fixes (Active) | p promote (Staged)"),
                    Line::from("Settings: Space/Enter toggle bool, cycle enum, or edit number"),
                    Line::from("Apps    : u cycle unknown policy | a add override | d delete"),
                    Line::from("Logs    : f cycle filter | x explain token | c clear logs"),
                ],
            ),
            Modal::Confirm { prompt, .. } => (
                " Confirm Action (y/Enter: yes, n/Esc: cancel) ",
                vec![
                    Line::from(""),
                    Line::from(Span::styled(
                        prompt.clone(),
                        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                    )),
                ],
            ),
            Modal::FilterInput { input } => (
                " Filter Active Rules (Enter: apply, Esc: cancel) ",
                vec![
                    Line::from("Case-insensitive substring (empty clears filter):"),
                    Line::from(Span::styled(format!("> {input}_"), Style::default().fg(Color::Cyan))),
                ],
            ),
            Modal::AddRule { kind, input } => (
                " Add Rule (Tab: kind, Enter: save, Esc: cancel) ",
                vec![
                    Line::from(vec![
                        Span::styled("Kind : ", Style::default().fg(Color::Gray)),
                        Span::styled(kind.label(), Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(format!("> {input}_"), Style::default().fg(Color::Cyan))),
                ],
            ),
            Modal::EditSetting { key, input } => (
                " Edit Setting (Enter: save, Esc: cancel) ",
                vec![
                    Line::from(vec![
                        Span::styled("Key  : ", Style::default().fg(Color::Gray)),
                        Span::raw(key.clone()),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(format!("> {input}_"), Style::default().fg(Color::Cyan))),
                ],
            ),
            Modal::AddApp { profile, input } => (
                " Add App Override (Tab: profile, Enter: save, Esc: cancel) ",
                vec![
                    Line::from(vec![
                        Span::styled("Profile: ", Style::default().fg(Color::Gray)),
                        Span::styled(app_profile_label(*profile), Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from("Enter window class or Wayland app_id (e.g. kitty, com.obsproject.Studio):"),
                    Line::from(Span::styled(format!("> {input}_"), Style::default().fg(Color::Cyan))),
                ],
            ),
            Modal::ExplainInput { input } => (
                " Explain Token (Enter: run, Esc: cancel) ",
                vec![
                    Line::from("Input ('teh', 'over ther', or 'vi:dduowcj'):"),
                    Line::from(Span::styled(format!("> {input}_"), Style::default().fg(Color::Cyan))),
                ],
            ),
        };

        let box_widget = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(title),
            );
        frame.render_widget(box_widget, popup);
    }
}

fn neutral_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(title, Style::default().fg(Color::Gray)))
}

fn kv_line<'a>(label: &'a str, val: &'a str, color: Color) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<15}: "), Style::default().fg(Color::Gray)),
        Span::styled(val, Style::default().fg(color).add_modifier(Modifier::BOLD)),
    ])
}

fn tier_style(tier: ConfidenceTier) -> Style {
    match tier {
        ConfidenceTier::Manual | ConfidenceTier::Confirmed => Style::default().fg(Color::Green),
        ConfidenceTier::Provisional => Style::default().fg(Color::Yellow),
        ConfidenceTier::Suspect => Style::default().fg(Color::Red),
    }
}

fn enum_setting_options(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "settings.dialect" => Some(DIALECT_OPTIONS),
        "settings.aggressiveness" => Some(AGGR_OPTIONS),
        "settings.undo_mode" => Some(UNDO_MODE_OPTIONS),
        "diagnostics.log_level" => Some(LOG_LEVEL_OPTIONS),
        "app_profiles.unknown_app_policy" => Some(UNKNOWN_POLICY_OPTIONS),
        _ => None,
    }
}

fn next_app_profile(profile: AppProfile) -> AppProfile {
    match profile {
        AppProfile::Bypass => AppProfile::Browser,
        AppProfile::Browser => AppProfile::Chat,
        AppProfile::Chat => AppProfile::Prose,
        AppProfile::Prose => AppProfile::Bypass,
    }
}

fn log_filter_label(filter: LogFilter) -> &'static str {
    match filter {
        LogFilter::All => "all",
        LogFilter::FixedOnly => "fixed",
        LogFilter::ManualOnly => "manual",
        LogFilter::UnfixedOnly => "unfixed",
    }
}

fn move_selection(index: &mut usize, delta: isize, len: usize) {
    if len == 0 {
        *index = 0;
        return;
    }
    let cur = (*index).min(len - 1) as isize;
    *index = (cur + delta).clamp(0, (len - 1) as isize) as usize;
}

fn clamp_index(index: &mut usize, len: usize) {
    if len == 0 {
        *index = 0;
    } else if *index >= len {
        *index = len - 1;
    }
}

fn format_cli_error(err: &CliError) -> &str {
    match err {
        CliError::Usage(msg) | CliError::Runtime(msg) => msg.as_str(),
    }
}

fn centered_rect(area: Rect, max_pct_w: u16, target_h: u16) -> Rect {
    let w = area
        .width
        .min((area.width as u32 * max_pct_w as u32 / 100).max(20) as u16)
        .max(1);
    let h = area.height.min(target_h).max(1);
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    Rect::new(x, y, w, h)
}

fn split_optional_lang(input: &str) -> (LangId, &str) {
    if let Some((prefix, rest)) = input.split_once(':') {
        let lang = prefix.trim();
        if !lang.is_empty() && lang.len() <= 8 && lang.chars().all(|c| c.is_ascii_alphabetic()) {
            return (LangId(lang.to_ascii_lowercase()), rest);
        }
    }
    (LangId::en(), input)
}

fn split_pair<'a>(input: &'a str, seps: &[&str]) -> Option<(&'a str, &'a str)> {
    for sep in seps {
        if let Some((a, b)) = input.split_once(sep) {
            let lhs = a.trim();
            let rhs = b.trim();
            if !lhs.is_empty() && !rhs.is_empty() {
                return Some((lhs, rhs));
            }
        }
    }
    None
}

fn split_context_lhs(lhs: &str) -> Option<(&str, &str)> {
    let trimmed = lhs.trim();
    if let Some((prev, typed)) = trimmed.split_once('|') {
        let p = prev.trim();
        let t = typed.trim();
        if !p.is_empty() && !t.is_empty() {
            return Some((p, t));
        }
    }
    let (prev, typed) = trimmed.rsplit_once(char::is_whitespace)?;
    let p = prev.trim();
    let t = typed.trim();
    if p.is_empty() || t.is_empty() {
        None
    } else {
        Some((p, t))
    }
}

fn collect_active_rule_rows(store: &RuleStore) -> (ConfidenceCountsJson, Vec<ActiveRuleRow>) {
    let c = store.compiled();
    let mut raw: Vec<(&'static str, String, String, bool, RuleDeleteTarget)> = Vec::new();

    for (typed, fixed) in &c.corrections {
        raw.push((
            "fix",
            RuleStore::stat_key_fix("en", typed),
            format!("{typed} -> {fixed}"),
            false,
            RuleDeleteTarget::Fix {
                lang: LangId::en(),
                typed: typed.clone(),
            },
        ));
    }
    for (lk, map) in &c.lang_corrections {
        for (typed, fixed) in map {
            let display = if lk == "en" {
                format!("{typed} -> {fixed}")
            } else {
                format!("[{lk}] {typed} -> {fixed}")
            };
            raw.push((
                "fix",
                RuleStore::stat_key_fix(lk, typed),
                display,
                false,
                RuleDeleteTarget::Fix {
                    lang: LangId(lk.clone()),
                    typed: typed.clone(),
                },
            ));
        }
    }
    for ((prev, typed), fixed) in &c.context_corrections {
        raw.push((
            "context",
            RuleStore::stat_key_ctx(prev, typed),
            format!("{prev} {typed} -> {fixed}"),
            false,
            RuleDeleteTarget::Context {
                prev: prev.clone(),
                typed: typed.clone(),
            },
        ));
    }
    for (trigger, expansion) in &c.snippets {
        raw.push((
            "snippet",
            format!("snippet:{trigger}"),
            format!("{trigger} -> {expansion}"),
            false,
            RuleDeleteTarget::Snippet {
                trigger: trigger.clone(),
            },
        ));
    }
    for w in &c.personal_dict {
        let canon = c
            .personal_dict_canonical
            .get(w)
            .map(String::as_str)
            .unwrap_or(w.as_str());
        raw.push((
            "word",
            RuleStore::stat_key_word("en", w),
            canon.to_string(),
            c.pinned.contains(w),
            RuleDeleteTarget::Word {
                lang: LangId::en(),
                word: w.clone(),
            },
        ));
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
                .unwrap_or(w.as_str());
            raw.push((
                "word",
                RuleStore::stat_key_word(lk, w),
                format!("[{lk}] {canon}"),
                false,
                RuleDeleteTarget::Word {
                    lang: LangId(lk.clone()),
                    word: w.clone(),
                },
            ));
        }
    }
    for (t, r) in &c.blocklist {
        raw.push((
            "block",
            RuleStore::stat_key_block(t, r),
            format!("{t} !-> {r}"),
            false,
            RuleDeleteTarget::Block {
                typed: t.clone(),
                rejected: r.clone(),
            },
        ));
    }
    for w in &c.telex_passthrough {
        raw.push((
            "passthrough",
            RuleStore::stat_key_pt(w),
            w.clone(),
            false,
            RuleDeleteTarget::Passthrough { raw: w.clone() },
        ));
    }

    let mut counts = ConfidenceCountsJson {
        manual: 0,
        confirmed: 0,
        provisional: 0,
        suspect: 0,
    };
    let mut rows = Vec::with_capacity(raw.len());
    for (kind, stat_key, display, is_pinned, delete_target) in raw {
        let (tier, origin, hits, strikes) = if kind == "snippet" {
            counts.manual += 1;
            (ConfidenceTier::Manual, "manual".to_string(), 0, 0)
        } else {
            let (tier, stat, hits, strikes) = store.classify_rule_confidence(&stat_key);
            match tier {
                ConfidenceTier::Manual => counts.manual += 1,
                ConfidenceTier::Confirmed => counts.confirmed += 1,
                ConfidenceTier::Provisional => counts.provisional += 1,
                ConfidenceTier::Suspect => counts.suspect += 1,
            }
            let origin = if is_pinned {
                "pinned".to_string()
            } else {
                stat.map(|s| s.origin.as_str().to_string())
                    .unwrap_or_else(|| "untracked".to_string())
            };
            (tier, origin, hits, strikes)
        };
        rows.push(ActiveRuleRow {
            kind,
            display,
            tier,
            origin,
            hits,
            strikes,
            delete_target,
        });
    }

    rows.sort_by(|a, b| {
        tier_rank(a.tier)
            .cmp(&tier_rank(b.tier))
            .then_with(|| a.kind.cmp(b.kind))
            .then_with(|| a.display.cmp(&b.display))
    });
    (counts, rows)
}

fn tier_rank(tier: ConfidenceTier) -> u8 {
    match tier {
        ConfidenceTier::Suspect => 0,
        ConfidenceTier::Provisional => 1,
        ConfidenceTier::Confirmed => 2,
        ConfidenceTier::Manual => 3,
    }
}

fn collect_app_overrides(c: &smartkey_core::CompiledRules) -> Vec<AppOverrideRow> {
    let mut rows = Vec::new();
    for (profile, set) in [
        (AppProfile::Bypass, &c.app_profiles.bypass),
        (AppProfile::Browser, &c.app_profiles.browser),
        (AppProfile::Chat, &c.app_profiles.chat),
        (AppProfile::Prose, &c.app_profiles.prose),
    ] {
        for app_id in set {
            rows.push(AppOverrideRow {
                profile,
                app_id: app_id.clone(),
            });
        }
    }
    rows
}

fn load_logs(log_dir: &Path, fallback_events: &[DiagnosticEvent]) -> Vec<LogRow> {
    if let Ok(records) = EventLog::read_records_in_range(log_dir, None, None) {
        if !records.is_empty() {
            return records
                .into_iter()
                .rev()
                .map(|r| {
                    let secs = (r.ts_ms / 1000) % 86_400;
                    let ts = format!(
                        "{} {:02}:{:02}:{:02}Z",
                        date_string(r.ts_ms),
                        secs / 3600,
                        (secs % 3600) / 60,
                        secs % 60
                    );
                    let summary = format_diagnostic_event_line(&r.event);
                    LogRow {
                        timestamp: ts,
                        app: r.app,
                        lang: r.lang,
                        summary,
                        event: r.event,
                    }
                })
                .collect();
        }
    }
    fallback_events
        .iter()
        .rev()
        .map(|ev| LogRow {
            timestamp: "recent".to_string(),
            app: None,
            lang: "-".to_string(),
            summary: format_diagnostic_event_line(ev),
            event: ev.clone(),
        })
        .collect()
}

fn promote_staged_entry(
    store: &mut RuleStore,
    queue: &str,
    key: &str,
    canonical: Option<&str>,
) -> Result<(), String> {
    match queue {
        "fix" => {
            let (lang_str, pair) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed fix staged key '{key}'"))?;
            let (typed, fixed) = pair
                .split_once("->")
                .ok_or_else(|| format!("Malformed fix staged pair '{pair}'"))?;
            store
                .set_user_correction(&LangId(lang_str.to_string()), typed, fixed)
                .map_err(|e| format!("Promote fix failed: {e}"))?;
        }
        "case_fix" => {
            let rest = key.strip_prefix("case:").unwrap_or(key);
            let (lang_str, word) = rest
                .split_once(':')
                .ok_or_else(|| format!("Malformed case_fix key '{key}'"))?;
            let w = canonical.unwrap_or(word);
            store
                .add_user_personal_word(&LangId(lang_str.to_string()), w)
                .map_err(|e| format!("Promote case_fix failed: {e}"))?;
        }
        "context" => {
            let (lhs, fixed) = key
                .split_once("->")
                .ok_or_else(|| format!("Malformed context key '{key}'"))?;
            let (prev, typed) = lhs
                .split_once('|')
                .ok_or_else(|| format!("Malformed context pair '{lhs}'"))?;
            store
                .set_user_context_correction(prev, typed, fixed)
                .map_err(|e| format!("Promote context failed: {e}"))?;
        }
        "untouched_word" => {
            let (lang_str, word) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed word key '{key}'"))?;
            let w = canonical.unwrap_or(word);
            store
                .add_user_personal_word(&LangId(lang_str.to_string()), w)
                .map_err(|e| format!("Promote word failed: {e}"))?;
        }
        "telex_passthrough" => {
            store
                .add_user_telex_passthrough(key)
                .map_err(|e| format!("Promote passthrough failed: {e}"))?;
        }
        other => {
            return Err(format!("Unsupported staged promotion queue '{other}'"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use smartkey_core::event_log::DiagnosticsConfig;
    use smartkey_core::Preset;

    static TUI_NONCE: AtomicU64 = AtomicU64::new(1);
    static RELOAD_COUNT: AtomicUsize = AtomicUsize::new(0);

    fn test_reload() -> bool {
        RELOAD_COUNT.fetch_add(1, Ordering::SeqCst);
        true
    }

    fn temp_tui_dirs(name: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "smartkey_tui_{name}_{}_{}",
            std::process::id(),
            TUI_NONCE.fetch_add(1, Ordering::Relaxed)
        ));
        let cfg = base.join("config");
        let logs = base.join("logs");
        fs::create_dir_all(&cfg).unwrap();
        fs::create_dir_all(&logs).unwrap();
        RuleStore::load_or_init(
            cfg.join("rules.toml"),
            cfg.join("learner_state.json"),
            Preset::Balanced,
        )
        .unwrap();
        (base, cfg)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, s: &str) {
        for c in s.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn tui_renders_all_tabs_and_narrow_viewport_without_panic() {
        let (base, cfg) = temp_tui_dirs("render_tabs");
        let logs = base.join("logs");
        let mut app = App::new(cfg, logs, test_reload);

        for (width, height) in [(100u16, 30u16), (40, 12)] {
            let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
            for tab_key in ['1', '2', '3', '4', '5'] {
                app.handle_key(key(KeyCode::Char(tab_key)));
                term.draw(|f| app.render(f)).unwrap();
            }
            app.handle_key(key(KeyCode::Char('2')));
            app.handle_key(key(KeyCode::Char('v')));
            term.draw(|f| app.render(f)).unwrap();
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_overview_toggles_persist_and_signal_reload() {
        let (base, cfg) = temp_tui_dirs("overview_toggles");
        let logs = base.join("logs");
        let before_reloads = RELOAD_COUNT.load(Ordering::SeqCst);
        let mut app = App::new(cfg.clone(), logs, test_reload);

        assert!(app.overview.enabled);
        assert!(app.overview.vi_lotus_enabled);

        app.handle_key(key(KeyCode::Char('e')));
        assert!(!app.overview.enabled);
        app.handle_key(key(KeyCode::Char('l')));
        assert!(!app.overview.vi_lotus_enabled);
        assert!(RELOAD_COUNT.load(Ordering::SeqCst) >= before_reloads + 2);

        let (reloaded, _) = load_rule_store_readonly(&cfg, Preset::Balanced).unwrap();
        assert!(!reloaded.compiled().enabled);
        assert!(!reloaded.compiled().vi_lotus_enabled);

        app.handle_key(key(KeyCode::Char('l')));
        assert!(app.overview.vi_lotus_enabled);

        assert!(app.handle_key(key(KeyCode::Char('q'))));
        assert!(app.handle_key(ctrl('c')));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_rules_add_delete_staged_promote_and_dismiss() {
        let (base, cfg) = temp_tui_dirs("rules_staged");
        let logs = base.join("logs");

        {
            let mut store = load_rule_store_mut(&cfg, Preset::Balanced).unwrap();
            let st = store.state_mut();
            st.staged_corrections.insert("en:teh->the".to_string(), 1);
            st.untouched_unknowns.insert("en:hyprland".to_string(), 2);
            st.staged_personal_canonical
                .insert("en:hyprland".to_string(), "Hyprland".to_string());
            st.staged_telex_passthrough.insert("aur".to_string(), 1);
            store.flush_state().unwrap();
        }

        let mut app = App::new(cfg.clone(), logs, test_reload);
        app.handle_key(key(KeyCode::Char('2')));

        // Add active fix rule
        app.handle_key(key(KeyCode::Char('a')));
        type_text(&mut app, "recieve=receive");
        app.handle_key(key(KeyCode::Enter));
        assert!(app
            .active_rules
            .iter()
            .any(|r| r.display == "recieve -> receive"));

        // Delete active fix rule via filter + Confirm
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "recieve");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(!app
            .active_rules
            .iter()
            .any(|r| r.display == "recieve -> receive"));
        app.handle_key(key(KeyCode::Esc));

        // Switch to Staged view, promote "fix", promote "untouched_word", dismiss "telex_passthrough"
        app.handle_key(key(KeyCode::Char('v')));
        assert_eq!(app.staged_entries.len(), 3);
        app.handle_key(key(KeyCode::Char('p'))); // promotes "fix"
        app.handle_key(key(KeyCode::Char('p'))); // promotes "untouched_word"
        assert_eq!(app.staged_entries.len(), 1);
        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Char('y'))); // dismisses "telex_passthrough"
        assert!(app.staged_entries.is_empty());

        let (reloaded, _) = load_rule_store_readonly(&cfg, Preset::Balanced).unwrap();
        assert_eq!(
            reloaded.compiled().corrections.get("teh").map(String::as_str),
            Some("the")
        );
        assert!(reloaded.compiled().personal_dict.contains("hyprland"));
        assert!(!reloaded.compiled().telex_passthrough.contains("aur"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_settings_cycle_and_numeric_modal_persist() {
        let (base, cfg) = temp_tui_dirs("settings");
        let logs = base.join("logs");
        let mut app = App::new(cfg.clone(), logs, test_reload);
        app.handle_key(key(KeyCode::Char('3')));

        let aggr_idx = app
            .settings
            .iter()
            .position(|(k, _)| *k == "settings.aggressiveness")
            .unwrap();
        app.setting_idx = aggr_idx;
        app.handle_key(key(KeyCode::Char(' ')));

        let win_idx = app
            .settings
            .iter()
            .position(|(k, _)| *k == "settings.undo_window_ms")
            .unwrap();
        app.setting_idx = win_idx;
        app.handle_key(key(KeyCode::Enter));
        for _ in 0..8 {
            app.handle_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "4200");
        app.handle_key(key(KeyCode::Enter));

        let (reloaded, _) = load_rule_store_readonly(&cfg, Preset::Balanced).unwrap();
        assert_eq!(
            reloaded.get_setting("settings.aggressiveness").as_deref(),
            Some("aggressive")
        );
        assert_eq!(
            reloaded.get_setting("settings.undo_window_ms").as_deref(),
            Some("4200")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_apps_override_add_delete_and_policy_cycle() {
        let (base, cfg) = temp_tui_dirs("apps");
        let logs = base.join("logs");
        let mut app = App::new(cfg.clone(), logs, test_reload);
        app.handle_key(key(KeyCode::Char('4')));

        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.unknown_app_policy, "prose");

        app.handle_key(key(KeyCode::Char('a')));
        type_text(&mut app, "my-custom-term");
        app.handle_key(key(KeyCode::Enter));
        assert!(app
            .app_overrides
            .iter()
            .any(|r| r.profile == AppProfile::Bypass && r.app_id == "my-custom-term"));

        app.handle_key(key(KeyCode::Char('d')));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(!app
            .app_overrides
            .iter()
            .any(|r| r.app_id == "my-custom-term"));

        let (reloaded, _) = load_rule_store_readonly(&cfg, Preset::Balanced).unwrap();
        assert_eq!(
            reloaded
                .get_setting("app_profiles.unknown_app_policy")
                .as_deref(),
            Some("prose")
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_logs_explain_filter_and_clear() {
        let (base, cfg) = temp_tui_dirs("logs_explain");
        let logs = base.join("logs");
        {
            let mut store = load_rule_store_mut(&cfg, Preset::Balanced).unwrap();
            let ev = DiagnosticEvent::AutocorrectApplied {
                typed: "teh".to_string(),
                applied: "the".to_string(),
                kind: smartkey_core::MatchKind::KnownTypoDict,
                total_count: 1,
            };
            store.state_mut().push_event(ev.clone());
            store.flush_state().unwrap();

            let mut log_writer = EventLog::new();
            log_writer.set_dir(Some(logs.clone()));
            log_writer.record(&DiagnosticsConfig::default(), "en", &ev);
        }

        let mut app = App::new(cfg.clone(), logs.clone(), test_reload);
        app.handle_key(key(KeyCode::Char('5')));
        assert!(!app.logs.is_empty());

        app.handle_key(key(KeyCode::Char('x')));
        type_text(&mut app, "teh");
        app.handle_key(key(KeyCode::Enter));
        let joined = app.explain_lines.join("\n");
        assert!(joined.contains("\"the\""), "explain output: {joined}");

        app.handle_key(key(KeyCode::Char('f')));
        assert_eq!(app.log_filter, LogFilter::FixedOnly);

        app.handle_key(key(KeyCode::Char('c')));
        app.handle_key(key(KeyCode::Char('y')));
        assert!(app.logs.is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_parse_lock_renders_banner_and_refuses_edits() {
        let (base, cfg) = temp_tui_dirs("parse_lock");
        let logs = base.join("logs");
        let corrupt = "enabled = [broken toml\n";
        fs::write(cfg.join("rules.toml"), corrupt).unwrap();

        let mut app = App::new(cfg.clone(), logs, test_reload);
        assert!(app.parse_locked);

        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let screen = buffer_text(&term);
        assert!(
            screen.contains("Parse Lock") || screen.contains("Parse lock active"),
            "missing parse-lock banner in screen:\n{screen}"
        );

        app.handle_key(key(KeyCode::Char('e')));
        assert!(app.status_is_error);
        assert_eq!(fs::read_to_string(cfg.join("rules.toml")).unwrap(), corrupt);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn tui_cmd_args_help_and_unknown_flag() {
        assert!(cmd_tui(&["--help".to_string()]).is_ok());
        assert!(matches!(
            cmd_tui(&["--bogus".to_string()]),
            Err(CliError::Usage(_))
        ));
    }
}
