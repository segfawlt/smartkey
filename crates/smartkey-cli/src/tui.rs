use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use ratatui::widgets::{
    Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;
use smartkey_core::{
    normalize_program_id, AppProfile, ConfidenceTier, DiagnosticEvent, EvalContext, EvalOutcome,
    EventLog, LangId, LatinScorer, MatchKind, Preset, RuleOrigin, RuleStore, SmartKeyEngine,
    UndoResolution,
};

use crate::{
    collect_staged_entries, default_config_dir, detect_fcitx5_module_so, detect_fcitx5_running,
    format_age_ago, inspect_im_environment, load_rule_store_mut, load_rule_store_readonly,
    CliError, CliResult,
};

const ACCENT: Color = Color::Cyan;
const MUTED: Color = Color::Gray;
const RULE: Color = Color::DarkGray;

const ACTIVITY_WINDOW_MS: u64 = 3 * 86_400_000;
const ACTIVITY_MAX_ROWS: usize = 400;
const MAX_SEEN_APPS: usize = 15;
const MAX_ALMOST: usize = 20;
const SIDEBAR_WIDTH: u16 = 22;
const SIDEBAR_MIN_TOTAL: u16 = 70;
const DETAIL_MIN_WIDTH: u16 = 84;
const AUTO_REFRESH: Duration = Duration::from_secs(4);

// ---------------------------------------------------------------------------
// Screens and small enums
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Activity,
    Review,
    Dictionary,
    Settings,
}

impl Screen {
    const ALL: [Screen; 4] = [
        Screen::Activity,
        Screen::Review,
        Screen::Dictionary,
        Screen::Settings,
    ];

    fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Review => 1,
            Self::Dictionary => 2,
            Self::Settings => 3,
        }
    }

    fn from_index(i: usize) -> Self {
        Self::ALL[i % 4]
    }

    fn label(self, short: bool) -> &'static str {
        match (self, short) {
            (Self::Activity, false) => "Activity",
            (Self::Review, false) => "Review",
            (Self::Dictionary, false) => "Dictionary",
            (Self::Settings, false) => "Settings",
            (Self::Activity, true) => "Act",
            (Self::Review, true) => "Rev",
            (Self::Dictionary, true) => "Dict",
            (Self::Settings, true) => "Set",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityFilter {
    All,
    Fixes,
    ByHand,
    Missed,
}

impl ActivityFilter {
    fn next(self) -> Self {
        match self {
            Self::All => Self::Fixes,
            Self::Fixes => Self::ByHand,
            Self::ByHand => Self::Missed,
            Self::Missed => Self::All,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::All => "everything",
            Self::Fixes => "fixes",
            Self::ByHand => "undos and hand fixes",
            Self::Missed => "missed words",
        }
    }

    fn matches(self, ev: &DiagnosticEvent) -> bool {
        match self {
            Self::All => true,
            Self::Fixes => matches!(ev, DiagnosticEvent::AutocorrectApplied { .. }),
            Self::ByHand => matches!(
                ev,
                DiagnosticEvent::BackspaceUndo { .. } | DiagnosticEvent::ManualBackspaceFix { .. }
            ),
            Self::Missed => matches!(
                ev,
                DiagnosticEvent::UnfixedUnknown { .. } | DiagnosticEvent::RealWordDeclined { .. }
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DictSection {
    Words,
    Fixes,
    Snippets,
    NeverFix,
    TelexKeep,
}

impl DictSection {
    const ALL: [DictSection; 5] = [
        DictSection::Words,
        DictSection::Fixes,
        DictSection::Snippets,
        DictSection::NeverFix,
        DictSection::TelexKeep,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Words => "Words",
            Self::Fixes => "Fixes",
            Self::Snippets => "Snippets",
            Self::NeverFix => "Never fix",
            Self::TelexKeep => "Telex keep",
        }
    }

    fn headers(self) -> &'static [&'static str] {
        match self {
            Self::Words => &["Word", "Language", "Added by", "Used", "Last used"],
            Self::Fixes => &[
                "Typed",
                "Becomes",
                "Only after",
                "Language",
                "Added by",
                "Used",
            ],
            Self::Snippets => &["Trigger", "Expands to"],
            Self::NeverFix => &["Typed", "Never change to", "Added by"],
            Self::TelexKeep => &["Keys typed", "Added by"],
        }
    }

    fn widths(self) -> Vec<Constraint> {
        match self {
            Self::Words => vec![
                Constraint::Fill(3),
                Constraint::Length(9),
                Constraint::Fill(2),
                Constraint::Length(6),
                Constraint::Length(10),
            ],
            Self::Fixes => vec![
                Constraint::Fill(2),
                Constraint::Fill(2),
                Constraint::Length(11),
                Constraint::Length(9),
                Constraint::Fill(2),
                Constraint::Length(6),
            ],
            Self::Snippets => vec![Constraint::Fill(1), Constraint::Fill(3)],
            Self::NeverFix => vec![
                Constraint::Fill(2),
                Constraint::Fill(2),
                Constraint::Fill(2),
            ],
            Self::TelexKeep => vec![Constraint::Fill(1), Constraint::Fill(2)],
        }
    }

    fn empty_text(self) -> &'static str {
        match self {
            Self::Words => "No words yet. Press n to add a word SmartKey should never correct.",
            Self::Fixes => "No fixes yet. Press n to teach SmartKey a correction.",
            Self::Snippets => "No snippets yet. Press n to add a trigger that expands into text.",
            Self::NeverFix => "Nothing blocked. Fixes you undo twice show up here.",
            Self::TelexKeep => "No Telex exceptions. These keep English words like \"aur\" as typed in Vietnamese mode.",
        }
    }
}

// ---------------------------------------------------------------------------
// Store operations with computed inverses (undo)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum StoreOp {
    AddWord {
        lang: LangId,
        word: String,
    },
    RemoveWord {
        lang: LangId,
        word: String,
        canonical: String,
    },
    SetFix {
        lang: LangId,
        typed: String,
        fixed: String,
    },
    RemoveFix {
        lang: LangId,
        typed: String,
        fixed: String,
    },
    SetContext {
        prev: String,
        typed: String,
        fixed: String,
    },
    RemoveContext {
        prev: String,
        typed: String,
        fixed: String,
    },
    SetSnippet {
        trigger: String,
        expansion: String,
    },
    RemoveSnippet {
        trigger: String,
        expansion: String,
    },
    AddBlock {
        typed: String,
        rejected: String,
    },
    RemoveBlock {
        typed: String,
        rejected: String,
    },
    AddPassthrough {
        raw: String,
    },
    RemovePassthrough {
        raw: String,
    },
    SetSetting {
        key: String,
        value: String,
    },
    SetAppProfile {
        app: String,
        profile: Option<AppProfile>,
    },
    PromoteStaged {
        queue: String,
        key: String,
        canonical: Option<String>,
    },
    DismissStaged {
        queue: String,
        key: String,
    },
}

impl StoreOp {
    /// Applies the op and returns the op that reverses it, when the change can be undone.
    fn apply(&self, store: &mut RuleStore) -> Result<Option<StoreOp>, String> {
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
            Self::PromoteStaged {
                queue,
                key,
                canonical,
            } => {
                promote_staged_entry(store, queue, key, canonical.as_deref())?;
                store.dismiss_staged_entry(queue, key).map_err(e)?;
                None
            }
            Self::DismissStaged { queue, key } => {
                store.dismiss_staged_entry(queue, key).map_err(e)?;
                None
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Snapshot rows
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tone {
    Normal,
    Warn,
    Muted,
}

#[derive(Debug, Clone)]
struct ActivityRow {
    ts_ms: u64,
    app: Option<String>,
    lang: String,
    change: String,
    why: String,
    note: String,
    tone: Tone,
    repeat: u32,
    event: DiagnosticEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    Fix,
    Context,
    Snippet,
    Word,
    Block,
    Passthrough,
}

#[derive(Debug, Clone)]
struct RuleEntry {
    kind: RuleKind,
    lang: LangId,
    a: String,
    b: String,
    prev: String,
    tier: ConfidenceTier,
    origin: Option<RuleOrigin>,
    pinned: bool,
    hits: u32,
    strikes: u32,
    last_hit_ms: Option<u64>,
}

impl RuleEntry {
    fn delete_op(&self) -> StoreOp {
        match self.kind {
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
        }
    }

    /// Re-saves the rule as a user rule so its origin becomes manual.
    fn keep_op(&self) -> StoreOp {
        match self.kind {
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
        }
    }

    fn title(&self) -> String {
        let base = match self.kind {
            RuleKind::Fix | RuleKind::Block | RuleKind::Snippet => {
                format!("{} → {}", self.a, self.b)
            }
            RuleKind::Context => format!("{} {} → {}", self.prev, self.a, self.b),
            RuleKind::Word => self.b.clone(),
            RuleKind::Passthrough => self.a.clone(),
        };
        if self.lang.as_str() != "en" && matches!(self.kind, RuleKind::Fix | RuleKind::Word) {
            format!("{base}  ({})", self.lang.as_str())
        } else {
            base
        }
    }

    fn added_by(&self) -> &'static str {
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

    fn learned_detail(&self) -> String {
        let how = match self.origin {
            Some(RuleOrigin::ManualFix2x) => "learned from your edits",
            Some(RuleOrigin::UndoReject) => "added when you undid a fix",
            Some(RuleOrigin::Untouched3x) => "you typed it 3× without fixing",
            _ => "edited by hand in rules.toml",
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewSection {
    Learned,
    Blocked,
    Almost,
}

impl ReviewSection {
    fn actions_help(self) -> (&'static str, &'static str) {
        match self {
            Self::Learned => (
                "Marks this as your own rule so it is not flagged again.",
                "Deletes the rule.",
            ),
            Self::Blocked => (
                "Keeps SmartKey from making this fix, as your own choice.",
                "Lets SmartKey make this fix again.",
            ),
            Self::Almost => (
                "Learns it now instead of waiting for more repeats.",
                "Forgets the count; SmartKey starts over.",
            ),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Learned => "Learned rules to check",
            Self::Blocked => "Fixes blocked after you undid them",
            Self::Almost => "Almost learned (most repeated first)",
        }
    }
}

#[derive(Debug, Clone)]
struct ReviewItem {
    id: String,
    section: ReviewSection,
    title: String,
    detail: String,
    warning: Option<&'static str>,
    keep_op: StoreOp,
    remove_op: StoreOp,
}

#[derive(Debug, Clone)]
struct DictRow {
    section: DictSection,
    cols: Vec<String>,
    haystack: String,
    label: String,
    delete_op: StoreOp,
}

#[derive(Debug, Clone, Copy)]
enum Control {
    Toggle,
    Choice(&'static [(&'static str, &'static str)]),
    Number {
        step: f64,
        min: f64,
        max: f64,
        decimals: usize,
        unit: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
struct SettingSpec {
    key: &'static str,
    label: &'static str,
    help: &'static str,
    control: Control,
}

const STRENGTH: &[(&str, &str)] = &[
    ("conservative", "careful"),
    ("balanced", "balanced"),
    ("aggressive", "eager"),
];
const SPELLING: &[(&str, &str)] = &[("mixed", "US + UK"), ("en_US", "US"), ("en_GB", "UK")];
const UNDO_MODE: &[(&str, &str)] = &[
    ("step", "stepwise"),
    ("immediate", "instant"),
    ("off", "off"),
];
const LOG_DETAIL: &[(&str, &str)] = &[("interesting", "important only"), ("all", "every word")];
const UNLISTED: &[(&str, &str)] = &[
    ("default", "built-in"),
    ("prose", "prose"),
    ("bypass", "off"),
    ("browser", "browser"),
    ("chat", "chat"),
];

const CORRECTION_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "settings.aggressiveness",
        label: "Strength",
        help: "How sure SmartKey must be before it changes a word. Careful fixes fewer words but almost never guesses wrong; eager fixes more and guesses more.",
        control: Control::Choice(STRENGTH),
    },
    SettingSpec {
        key: "settings.dialect",
        label: "Spelling",
        help: "US + UK leaves both colour and color alone. US or UK also rewrites valid words into that spelling.",
        control: Control::Choice(SPELLING),
    },
    SettingSpec {
        key: "settings.enable_missed_space_split",
        label: "Fix missed spaces",
        help: "Splits run-together words such as \"ofthe\" into \"of the\" when both halves are common words.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.enable_contractions",
        label: "Add apostrophes",
        help: "Turns dont into don't, doesnt into doesn't, and similar contractions.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.fix_shift_slips",
        label: "Fix Shift slips",
        help: "Repairs capitals from holding Shift a beat too long, such as THe → The.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.fix_proper_nouns",
        label: "Capitalise names",
        help: "Capitalises known names and places from the language pack.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.expand_shorthand",
        label: "Expand shorthand",
        help: "Expands common shorthand listed in the language pack.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.auto_capitalize_i",
        label: "Capitalise \"i\"",
        help: "Changes a lone i into I.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "settings.auto_capitalize_sentence",
        label: "Capitalise sentences",
        help: "Capitalises the first word after a full stop, question mark, or exclamation mark.",
        control: Control::Toggle,
    },
];

const UNDO_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "settings.undo_mode",
        label: "Backspace undo",
        help: "Stepwise: the first Backspace removes the space, the second restores what you typed. Instant: one Backspace restores it. Off: Backspace only deletes.",
        control: Control::Choice(UNDO_MODE),
    },
    SettingSpec {
        key: "settings.undo_window_ms",
        label: "Undo window",
        help: "How long after a fix Backspace can still restore what you typed.",
        control: Control::Number {
            step: 500.0,
            min: 500.0,
            max: 60_000.0,
            decimals: 0,
            unit: "ms",
        },
    },
];

const VIETNAMESE_SPECS: &[SettingSpec] = &[SettingSpec {
    key: "plugins.vi_lotus",
    label: "Work with fcitx5-lotus",
    help: "Lets SmartKey run alongside the fcitx5-lotus Vietnamese input method and repair Telex typos.",
    control: Control::Toggle,
}];

const UNLISTED_SPEC: SettingSpec = SettingSpec {
    key: "app_profiles.unknown_app_policy",
    label: "Unlisted apps",
    help: "How to treat apps that are not in the built-in lists or your choices above. Built-in treats them as prose.",
    control: Control::Choice(UNLISTED),
};

const DIAGNOSTIC_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "diagnostics.event_log",
        label: "Activity log",
        help: "Records fixes, undos, and missed words so they appear on the Activity screen.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "diagnostics.log_level",
        label: "Log detail",
        help: "Every word also records words left unchanged. Useful for debugging, but the log grows quickly.",
        control: Control::Choice(LOG_DETAIL),
    },
    SettingSpec {
        key: "diagnostics.keep_days",
        label: "Keep logs",
        help: "Days of activity log to keep on disk.",
        control: Control::Number {
            step: 1.0,
            min: 1.0,
            max: 365.0,
            decimals: 0,
            unit: "days",
        },
    },
    SettingSpec {
        key: "diagnostics.near_miss_band",
        label: "Near-miss range",
        help: "How close a skipped word must come to the fix threshold to be logged as a missed word. 0 logs every miss.",
        control: Control::Number {
            step: 0.05,
            min: 0.0,
            max: 1.0,
            decimals: 2,
            unit: "",
        },
    },
    SettingSpec {
        key: "diagnostics.trace_log",
        label: "Key trace",
        help: "Records raw key events for bug reports. Leave off unless you are debugging.",
        control: Control::Toggle,
    },
    SettingSpec {
        key: "diagnostics.annotate_rules",
        label: "Comment learned rules",
        help: "Writes a comment next to each learned rule in rules.toml saying where it came from.",
        control: Control::Toggle,
    },
];

#[derive(Debug, Clone)]
enum SettingsRow {
    Header(&'static str),
    Setting(SettingSpec),
    App {
        app: String,
        profile: AppProfile,
        reason: &'static str,
        chosen: Option<AppProfile>,
    },
}

// ---------------------------------------------------------------------------
// Modals
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum RowAction {
    Ops {
        ops: Vec<StoreOp>,
        msg: String,
    },
    Explain {
        lang: LangId,
        prev: Option<String>,
        token: String,
    },
    PickApp {
        app: String,
    },
    GoReview,
}

#[derive(Debug, Clone)]
struct ActionItem {
    key: char,
    label: String,
    hint: String,
    action: RowAction,
}

#[derive(Debug, Clone)]
struct FormField {
    label: &'static str,
    value: String,
    required: bool,
}

#[derive(Debug, Clone)]
struct Form {
    section: DictSection,
    fields: Vec<FormField>,
    focus: usize,
}

impl Form {
    fn new(section: DictSection) -> Self {
        let f = |label: &'static str, value: &str, required: bool| FormField {
            label,
            value: value.to_string(),
            required,
        };
        let fields = match section {
            DictSection::Words => vec![f("Word", "", true), f("Language", "en", true)],
            DictSection::Fixes => vec![
                f("Typed", "", true),
                f("Becomes", "", true),
                f("Only after word", "", false),
                f("Language", "en", true),
            ],
            DictSection::Snippets => vec![f("Trigger", "", true), f("Expands to", "", true)],
            DictSection::NeverFix => vec![f("Typed", "", true), f("Never change to", "", true)],
            DictSection::TelexKeep => vec![f("Keys typed", "", true)],
        };
        Self {
            section,
            fields,
            focus: 0,
        }
    }

    fn value(&self, i: usize) -> &str {
        self.fields[i].value.trim()
    }

    fn first_missing(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|f| f.required && f.value.trim().is_empty())
    }

    fn to_ops(&self) -> (Vec<StoreOp>, String) {
        match self.section {
            DictSection::Words => (
                vec![StoreOp::AddWord {
                    lang: LangId(self.value(1).to_lowercase()),
                    word: self.value(0).to_string(),
                }],
                format!("Added word \"{}\"", self.value(0)),
            ),
            DictSection::Fixes => {
                let (typed, fixed, after) = (self.value(0), self.value(1), self.value(2));
                if after.is_empty() {
                    (
                        vec![StoreOp::SetFix {
                            lang: LangId(self.value(3).to_lowercase()),
                            typed: typed.to_string(),
                            fixed: fixed.to_string(),
                        }],
                        format!("Saved fix {typed} → {fixed}"),
                    )
                } else {
                    (
                        vec![StoreOp::SetContext {
                            prev: after.to_string(),
                            typed: typed.to_string(),
                            fixed: fixed.to_string(),
                        }],
                        format!("Saved fix {typed} → {fixed} after \"{after}\""),
                    )
                }
            }
            DictSection::Snippets => (
                vec![StoreOp::SetSnippet {
                    trigger: self.value(0).to_string(),
                    expansion: self.value(1).to_string(),
                }],
                format!("Saved snippet {}", self.value(0)),
            ),
            DictSection::NeverFix => (
                vec![StoreOp::AddBlock {
                    typed: self.value(0).to_string(),
                    rejected: self.value(1).to_string(),
                }],
                format!(
                    "SmartKey will never change {} → {}",
                    self.value(0),
                    self.value(1)
                ),
            ),
            DictSection::TelexKeep => (
                vec![StoreOp::AddPassthrough {
                    raw: self.value(0).to_string(),
                }],
                format!("Telex will keep \"{}\" as typed", self.value(0)),
            ),
        }
    }
}

const APP_CHOICES: [(Option<AppProfile>, &str, &str); 5] = [
    (None, "Built-in", "use SmartKey's own list"),
    (
        Some(AppProfile::Bypass),
        "Off",
        "never touch typing in this app",
    ),
    (
        Some(AppProfile::Browser),
        "Browser",
        "fix text, skip URL and password fields",
    ),
    (Some(AppProfile::Chat), "Chat", "casual messaging"),
    (Some(AppProfile::Prose), "Prose", "documents, notes, email"),
];

#[derive(Debug, Clone)]
enum Modal {
    Help,
    Actions {
        title: String,
        items: Vec<ActionItem>,
        selected: usize,
    },
    Info {
        title: String,
        lines: Vec<String>,
    },
    ExplainInput {
        input: String,
    },
    EditNumber {
        spec: SettingSpec,
        current: String,
        input: String,
    },
    AppPicker {
        app: String,
        selected: usize,
    },
    Form(Form),
    Confirm {
        prompt: String,
        ops: Vec<StoreOp>,
        msg: String,
    },
}

#[derive(Debug, Clone)]
struct Toast {
    text: String,
    error: bool,
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub(crate) struct App {
    config_dir: PathBuf,
    log_dir: PathBuf,
    notify_reload: fn() -> bool,
    screen: Screen,
    modal: Option<Modal>,
    filter_editing: bool,
    focus_sidebar: bool,
    toast: Option<Toast>,
    undo: Option<(String, Vec<StoreOp>)>,

    parse_locked: bool,
    parse_error: Option<String>,
    env_warnings: Vec<String>,
    enabled: bool,
    vi_lotus: bool,
    lotus_installed: bool,
    fcitx5_running: bool,

    activity: Vec<ActivityRow>,
    activity_view: Vec<usize>,
    activity_idx: usize,
    activity_filter: ActivityFilter,
    activity_search: String,
    day_fixes: usize,
    day_undone: usize,
    day_by_hand: usize,

    review: Vec<ReviewItem>,
    review_idx: usize,
    review_marked: BTreeSet<String>,
    review_hidden: usize,

    dict: Vec<DictRow>,
    dict_counts: [usize; 5],
    dict_section: DictSection,
    dict_view: Vec<usize>,
    dict_idx: usize,
    dict_filter: String,

    settings_rows: Vec<SettingsRow>,
    settings_values: BTreeMap<String, String>,
    settings_idx: usize,
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
    let mut last_refresh = Instant::now();
    loop {
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| CliError::Runtime(format!("terminal draw failed: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| CliError::Runtime(format!("event poll failed: {e}")))?
        {
            if let Event::Key(key) =
                event::read().map_err(|e| CliError::Runtime(format!("event read failed: {e}")))?
            {
                if key.kind == KeyEventKind::Press && app.handle_key(key) {
                    break;
                }
            }
        } else if last_refresh.elapsed() >= AUTO_REFRESH && app.is_idle() {
            app.refresh();
            last_refresh = Instant::now();
        }
    }

    Ok(())
}

fn print_tui_help() {
    println!(
        "\
SmartKey settings and activity (`smartkey tui`)

USAGE:
  smartkey tui [--dir <PATH>] [--log-dir <PATH>]

OPTIONS:
  --dir <PATH>       Config directory (default: ~/.config/smartkey)
  --log-dir <PATH>   Activity log directory (default: ~/.local/state/smartkey/logs)
  -h, --help         Show this help text

SCREENS:
  1 Activity     Recent fixes; press Enter on one to block it, keep it, or explain it
  2 Review       Learned rules and almost-learned fixes that need a yes or no
  3 Dictionary   Your words, fixes, snippets, blocked fixes, and Telex exceptions
  4 Settings     Correction, undo, Vietnamese, per-app, and logging options

KEYS:
  1-4 / Tab      Switch screen
  u              Undo the last change
  ?              Keys for the current screen
  q / Ctrl+C     Quit"
    );
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
            toast: None,
            undo: None,
            parse_locked: false,
            parse_error: None,
            env_warnings: inspect_im_environment().1,
            enabled: true,
            vi_lotus: false,
            lotus_installed: false,
            fcitx5_running: false,
            activity: Vec::new(),
            activity_view: Vec::new(),
            activity_idx: 0,
            activity_filter: ActivityFilter::All,
            activity_search: String::new(),
            day_fixes: 0,
            day_undone: 0,
            day_by_hand: 0,
            review: Vec::new(),
            review_idx: 0,
            review_marked: BTreeSet::new(),
            review_hidden: 0,
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

    fn is_idle(&self) -> bool {
        self.modal.is_none() && !self.filter_editing
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
        let records = EventLog::read_records_in_range(
            &self.log_dir,
            Some(now.saturating_sub(ACTIVITY_WINDOW_MS)),
            None,
        )
        .unwrap_or_default();

        self.build_activity(&records, &store, now);
        let rules = collect_rules(&store);
        self.build_review(&rules, &store);
        self.build_dictionary(&rules);
        self.build_settings(&store, &records);
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

    fn rebuild_activity_view(&mut self) {
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

    fn build_review(&mut self, rules: &[RuleEntry], store: &RuleStore) {
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
            let warning = (r.strikes >= 2).then_some("often undone");
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
                title: r.title(),
                detail: r.learned_detail(),
                warning,
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
            let Some((title, detail)) = describe_staged(
                s.queue,
                &s.key,
                s.canonical.as_deref(),
                s.count,
                s.threshold,
            ) else {
                continue;
            };
            almost.push((
                s.count,
                ReviewItem {
                    id: format!("staged:{}:{}", s.queue, s.key),
                    section: ReviewSection::Almost,
                    title,
                    detail,
                    warning: None,
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

    fn rebuild_dict_view(&mut self) {
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

    fn set_status(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: false,
        });
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast {
            text: text.into(),
            error: true,
        });
    }

    // -- mutations ---------------------------------------------------------------

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

    fn mutate(&mut self, ops: Vec<StoreOp>, msg: String, undoable: bool) {
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
                if undoable && !inverses.is_empty() {
                    self.undo = Some((msg.clone(), inverses));
                    self.set_status(format!("{msg}{suffix} · u undo"));
                } else {
                    self.set_status(format!("{msg}{suffix}"));
                }
            }
            Err((err, partial)) => {
                if undoable && !partial.is_empty() {
                    self.undo = Some((msg, partial));
                }
                self.set_error(err);
            }
        }
    }

    fn undo_last(&mut self) {
        let Some((label, ops)) = self.undo.take() else {
            self.set_status("Nothing to undo");
            return;
        };
        let result = self.apply_ops(&ops);
        (self.notify_reload)();
        self.refresh();
        match result {
            Ok(_) => self.set_status(format!("Undid: {label}")),
            Err((err, _)) => self.set_error(format!("Undo failed: {err}")),
        }
    }

    // -- keys ------------------------------------------------------------------

    /// Handles one key press. Returns `true` when the app should exit.
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return true;
        }
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return false;
        }
        if self.filter_editing {
            self.handle_filter_key(key);
            return false;
        }
        match key.code {
            KeyCode::Char('q') => return true,
            KeyCode::Char(c @ '1'..='4') => {
                self.screen = Screen::from_index((c as u8 - b'1') as usize);
                self.focus_sidebar = false;
                return false;
            }
            KeyCode::Tab => {
                self.screen = Screen::from_index(self.screen.index() + 1);
                self.focus_sidebar = false;
                return false;
            }
            KeyCode::BackTab => {
                self.screen = Screen::from_index(self.screen.index() + 3);
                self.focus_sidebar = false;
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
            KeyCode::Char('u') => {
                self.undo_last();
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
                _ => {}
            }
            return false;
        }
        if key.code == KeyCode::Esc && !self.has_pending_filter() {
            self.focus_sidebar = true;
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

    fn selected_activity(&self) -> Option<&ActivityRow> {
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
                    vec![StoreOp::SetSetting {
                        key: "settings.enabled".to_string(),
                        value: next.to_string(),
                    }],
                    if next {
                        "SmartKey on".to_string()
                    } else {
                        "SmartKey off".to_string()
                    },
                    true,
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
            KeyCode::Down | KeyCode::Char('j') => step(&mut self.review_idx, 1, self.review.len()),
            KeyCode::Up | KeyCode::Char('k') => step(&mut self.review_idx, -1, self.review.len()),
            KeyCode::Char(' ') => {
                if let Some(item) = self.review.get(self.review_idx) {
                    let id = item.id.clone();
                    if !self.review_marked.remove(&id) {
                        self.review_marked.insert(id);
                    }
                    step(&mut self.review_idx, 1, self.review.len());
                }
            }
            KeyCode::Char('a') => {
                if let Some(item) = self.review.get(self.review_idx).cloned() {
                    self.mutate(vec![item.keep_op], format!("Kept {}", item.title), false);
                }
            }
            KeyCode::Char('d') => {
                if let Some(item) = self.review.get(self.review_idx).cloned() {
                    self.mutate(
                        vec![item.remove_op],
                        format!("Removed {}", item.title),
                        true,
                    );
                }
            }
            KeyCode::Char('A') => {
                let ops: Vec<StoreOp> = self.marked_review().map(|i| i.keep_op.clone()).collect();
                if ops.is_empty() {
                    self.set_status("Mark items with Space first");
                } else {
                    let n = ops.len();
                    self.review_marked.clear();
                    self.mutate(ops, format!("Kept {n} items"), false);
                }
            }
            KeyCode::Char('D') => {
                let ops: Vec<StoreOp> = self.marked_review().map(|i| i.remove_op.clone()).collect();
                if ops.is_empty() {
                    self.set_status("Mark items with Space first");
                } else {
                    let n = ops.len();
                    self.modal = Some(Modal::Confirm {
                        prompt: format!("Remove {n} marked items?"),
                        ops,
                        msg: format!("Removed {n} items"),
                    });
                }
            }
            _ => {}
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
            app.rebuild_dict_view();
        };
        match key.code {
            KeyCode::Right | KeyCode::Char('l') => switch(self, 1),
            KeyCode::Left | KeyCode::Char('h') => switch(self, -1),
            KeyCode::Down | KeyCode::Char('j') => step(&mut self.dict_idx, 1, self.dict_view.len()),
            KeyCode::Up | KeyCode::Char('k') => step(&mut self.dict_idx, -1, self.dict_view.len()),
            KeyCode::Char('/') => self.filter_editing = true,
            KeyCode::Esc if !self.dict_filter.is_empty() => {
                self.dict_filter.clear();
                self.rebuild_dict_view();
            }
            KeyCode::Char('n') => self.modal = Some(Modal::Form(Form::new(self.dict_section))),
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(row) = self
                    .dict_view
                    .get(self.dict_idx)
                    .and_then(|&i| self.dict.get(i))
                    .cloned()
                {
                    self.mutate(vec![row.delete_op], format!("Removed {}", row.label), true);
                }
            }
            _ => {}
        }
    }

    fn move_settings(&mut self, delta: isize) {
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

    fn handle_settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_settings(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_settings(-1),
            KeyCode::Right | KeyCode::Char('l') => self.change_setting(1),
            KeyCode::Left | KeyCode::Char('h') => self.change_setting(-1),
            KeyCode::Enter | KeyCode::Char(' ') => {
                match self.settings_rows.get(self.settings_idx).cloned() {
                    Some(SettingsRow::Setting(spec)) => match spec.control {
                        Control::Number { .. } => {
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
                    vec![StoreOp::SetSetting {
                        key: spec.key.to_string(),
                        value: next,
                    }],
                    format!("{}: {shown}", spec.label),
                    true,
                );
            }
            Some(SettingsRow::App { app, chosen, .. }) => {
                let pos = APP_CHOICES
                    .iter()
                    .position(|(p, _, _)| *p == chosen)
                    .unwrap_or(0) as i32;
                let next = APP_CHOICES[(pos + delta).rem_euclid(APP_CHOICES.len() as i32) as usize];
                self.mutate(
                    vec![StoreOp::SetAppProfile {
                        app: app.clone(),
                        profile: next.0,
                    }],
                    format!("{app}: {}", next.1.to_lowercase()),
                    true,
                );
            }
            _ => {}
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Help | Modal::Info { .. } => {
                if !matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?')
                ) {
                    self.modal = Some(modal);
                }
            }
            Modal::Actions {
                title,
                items,
                mut selected,
            } => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    step(&mut selected, 1, items.len());
                    self.modal = Some(Modal::Actions {
                        title,
                        items,
                        selected,
                    });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    step(&mut selected, -1, items.len());
                    self.modal = Some(Modal::Actions {
                        title,
                        items,
                        selected,
                    });
                }
                KeyCode::Enter => {
                    if let Some(item) = items.get(selected).cloned() {
                        self.run_action(item.action);
                    }
                }
                KeyCode::Char(c) => {
                    if let Some(item) = items.iter().find(|i| i.key == c).cloned() {
                        self.run_action(item.action);
                    } else {
                        self.modal = Some(Modal::Actions {
                            title,
                            items,
                            selected,
                        });
                    }
                }
                _ => {
                    self.modal = Some(Modal::Actions {
                        title,
                        items,
                        selected,
                    })
                }
            },
            Modal::ExplainInput { mut input } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let (lang, rest) = split_lang_prefix(input.trim());
                    let rest = rest.trim();
                    let (prev, token) = match rest.rsplit_once(char::is_whitespace) {
                        Some((p, t)) => (Some(p.trim().to_string()), t.trim().to_string()),
                        None => (None, rest.to_string()),
                    };
                    if token.is_empty() {
                        self.modal = Some(Modal::ExplainInput { input });
                    } else {
                        self.run_action(RowAction::Explain { lang, prev, token });
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
            Modal::EditNumber {
                spec,
                current,
                mut input,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    let value = input.trim().to_string();
                    if value.is_empty() {
                        self.modal = Some(Modal::EditNumber {
                            spec,
                            current,
                            input,
                        });
                    } else {
                        let shown = display_setting(&spec, &value);
                        self.mutate(
                            vec![StoreOp::SetSetting {
                                key: spec.key.to_string(),
                                value,
                            }],
                            format!("{}: {shown}", spec.label),
                            true,
                        );
                    }
                }
                KeyCode::Backspace => {
                    input.pop();
                    self.modal = Some(Modal::EditNumber {
                        spec,
                        current,
                        input,
                    });
                }
                KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => {
                    input.push(c);
                    self.modal = Some(Modal::EditNumber {
                        spec,
                        current,
                        input,
                    });
                }
                _ => {
                    self.modal = Some(Modal::EditNumber {
                        spec,
                        current,
                        input,
                    })
                }
            },
            Modal::AppPicker { app, mut selected } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    step(&mut selected, 1, APP_CHOICES.len());
                    self.modal = Some(Modal::AppPicker { app, selected });
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    step(&mut selected, -1, APP_CHOICES.len());
                    self.modal = Some(Modal::AppPicker { app, selected });
                }
                KeyCode::Enter => {
                    let (profile, label, _) = APP_CHOICES[selected];
                    self.mutate(
                        vec![StoreOp::SetAppProfile {
                            app: app.clone(),
                            profile,
                        }],
                        format!("{app}: {}", label.to_lowercase()),
                        true,
                    );
                }
                _ => self.modal = Some(Modal::AppPicker { app, selected }),
            },
            Modal::Form(mut form) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Tab | KeyCode::Down => {
                    form.focus = (form.focus + 1) % form.fields.len();
                    self.modal = Some(Modal::Form(form));
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.focus = (form.focus + form.fields.len() - 1) % form.fields.len();
                    self.modal = Some(Modal::Form(form));
                }
                KeyCode::Enter => match form.first_missing() {
                    Some(i) => {
                        form.focus = i;
                        self.modal = Some(Modal::Form(form));
                    }
                    None => {
                        let (ops, msg) = form.to_ops();
                        self.mutate(ops, msg, true);
                    }
                },
                KeyCode::Backspace => {
                    let f = form.focus;
                    form.fields[f].value.pop();
                    self.modal = Some(Modal::Form(form));
                }
                KeyCode::Char(c) => {
                    let f = form.focus;
                    form.fields[f].value.push(c);
                    self.modal = Some(Modal::Form(form));
                }
                _ => self.modal = Some(Modal::Form(form)),
            },
            Modal::Confirm { prompt, ops, msg } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    self.review_marked.clear();
                    self.mutate(ops, msg, true);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.set_status("Cancelled"),
                _ => self.modal = Some(Modal::Confirm { prompt, ops, msg }),
            },
        }
    }

    fn run_action(&mut self, action: RowAction) {
        match action {
            RowAction::Ops { ops, msg } => self.mutate(ops, msg, true),
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
            RowAction::GoReview => self.screen = Screen::Review,
        }
    }

    fn explain(&self, lang: &LangId, prev: Option<&str>, token: &str) -> Vec<String> {
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
                    kind_label(*kind)
                ));
            }
            EvalOutcome::Unchanged { reason, .. } => {
                lines.push(format!("SmartKey leaves “{token}” as typed."));
                lines.push(format!(
                    "Reason: {} · a fix needs score {min_score:.2} and lead {min_margin:.2}",
                    kind_label(*reason)
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
                    kind_label(c.kind)
                ));
            }
        }
        lines
    }

    // -- rendering ---------------------------------------------------------------

    pub(crate) fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        if area.width < 24 || area.height < 8 {
            frame.render_widget(
                Paragraph::new("Window too small for SmartKey. Make it bigger or press q.")
                    .wrap(Wrap { trim: true }),
                area,
            );
            return;
        }

        let banners = self.banner_lines();
        let compact = area.width < SIDEBAR_MIN_TOTAL;
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
            self.render_header(frame, chunks[0]);
            i = 1;
        }
        if !banners.is_empty() {
            frame.render_widget(Paragraph::new(banners), chunks[i]);
            i += 1;
        }
        let body = chunks[i];
        let main = if compact {
            body
        } else {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(20)])
                .split(body);
            self.render_sidebar(frame, cols[0]);
            cols[1]
        };
        match self.screen {
            Screen::Activity => self.render_activity(frame, main),
            Screen::Review => self.render_review(frame, main),
            Screen::Dictionary => self.render_dictionary(frame, main),
            Screen::Settings => self.render_settings(frame, main),
        }
        self.render_toast(frame, chunks[i + 1]);
        self.render_hints(frame, chunks[i + 2]);

        if let Some(modal) = &self.modal {
            self.render_modal(frame, area, modal);
        }
    }

    fn banner_lines(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        if self.parse_locked {
            let detail = self
                .parse_error
                .clone()
                .unwrap_or_else(|| "syntax error".to_string());
            out.push(Line::from(Span::styled(
                format!(" rules.toml can't be read, so changes are blocked: {detail}"),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )));
        }
        if self.screen == Screen::Activity {
            if !self.fcitx5_running {
                out.push(warn_line(
                    "Fcitx5 is not running, so SmartKey is not fixing anything right now",
                ));
            } else if self.vi_lotus && !self.lotus_installed {
                out.push(warn_line(
                    "Vietnamese support is on, but fcitx5-lotus is not installed",
                ));
            }
            if let Some(w) = self.env_warnings.first() {
                out.push(warn_line(w));
            }
        }
        out.truncate(2);
        out
    }

    /// One-row screen switcher used when the window is too narrow for the sidebar.
    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let short = area.width < 72;
        let mut spans = vec![Span::styled(
            if short { " SK " } else { " SmartKey " },
            Style::default().add_modifier(Modifier::BOLD),
        )];
        spans.push(if self.enabled {
            Span::styled("● on ", Style::default().fg(Color::Green))
        } else {
            Span::styled("● off ", Style::default().fg(Color::Red))
        });
        spans.push(Span::raw("  "));
        for s in Screen::ALL {
            let selected = s == self.screen;
            let style = if selected {
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(MUTED)
            };
            spans.push(Span::styled(
                format!(" {} {}", s.index() + 1, s.label(short)),
                style,
            ));
            if s == Screen::Review && !self.review.is_empty() {
                spans.push(Span::styled(
                    format!(" {}", self.review.len()),
                    Style::default().fg(Color::Yellow),
                ));
            }
            spans.push(Span::raw(" "));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(6), Constraint::Min(0)])
            .split(area);

        let items: Vec<ListItem> = Screen::ALL
            .iter()
            .map(|&s| {
                let mut spans = vec![Span::raw(format!(" {} {}", s.index() + 1, s.label(false)))];
                if s == Screen::Review && !self.review.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", self.review.len()),
                        Style::default().fg(Color::Yellow),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let nav_style = if self.focus_sidebar {
            selected_style()
        } else {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        };
        let nav = List::new(items).highlight_style(nav_style).block(panel(
            Line::from(Span::styled(
                format!(" SmartKey {} ", if self.enabled { "●" } else { "○" }),
                Style::default().fg(if self.enabled {
                    Color::Green
                } else {
                    Color::Red
                }),
            )),
            self.focus_sidebar,
        ));
        let mut state = ListState::default();
        state.select(Some(self.screen.index()));
        frame.render_stateful_widget(nav, rows[0], &mut state);

        if rows[1].height >= 3 {
            let state_line = if self.enabled {
                Span::styled(" ● on", Style::default().fg(Color::Green))
            } else {
                Span::styled(" ● off", Style::default().fg(Color::Red))
            };
            let muted = Style::default().fg(MUTED);
            let lines = vec![
                Line::from(state_line),
                Line::from(Span::styled(
                    if self.fcitx5_running {
                        " Fcitx5 running"
                    } else {
                        " Fcitx5 stopped"
                    },
                    if self.fcitx5_running {
                        muted
                    } else {
                        Style::default().fg(Color::Yellow)
                    },
                )),
                Line::from(Span::styled(
                    format!(" Vietnamese {}", if self.vi_lotus { "on" } else { "off" }),
                    muted,
                )),
                Line::from(""),
                Line::from(Span::styled(" Last 24 hours", muted)),
                Line::from(Span::styled(
                    format!(" {} fixes", self.day_fixes),
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(format!(" {} undone", self.day_undone), muted)),
                Line::from(Span::styled(
                    format!(" {} fixed by you", self.day_by_hand),
                    muted,
                )),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(panel(Line::from(" Status "), false)),
                rows[1],
            );
        }
    }

    fn render_toast(&self, frame: &mut Frame, area: Rect) {
        let line = match &self.toast {
            Some(t) if t.error => Line::from(Span::styled(
                format!(" {}", t.text),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Some(t) => Line::from(Span::styled(
                format!(" {}", t.text),
                Style::default().fg(ACCENT),
            )),
            None => Line::default(),
        };
        frame.render_widget(Paragraph::new(line), area);
    }

    fn hint_pairs(&self) -> Vec<(&'static str, &'static str)> {
        if let Some(modal) = &self.modal {
            return match modal {
                Modal::Help | Modal::Info { .. } => vec![("esc", "close")],
                Modal::Actions { .. } => {
                    vec![("letter/⏎", "choose"), ("↑↓", "move"), ("esc", "close")]
                }
                Modal::ExplainInput { .. } => vec![("⏎", "explain"), ("esc", "cancel")],
                Modal::EditNumber { .. } => vec![("⏎", "save"), ("esc", "cancel")],
                Modal::AppPicker { .. } => vec![("↑↓", "choose"), ("⏎", "save"), ("esc", "cancel")],
                Modal::Form(_) => vec![("tab", "next field"), ("⏎", "save"), ("esc", "cancel")],
                Modal::Confirm { .. } => vec![("y", "yes"), ("n/esc", "no")],
            };
        }
        if self.filter_editing {
            return vec![("type", "to filter"), ("⏎", "done"), ("esc", "clear")];
        }
        if self.focus_sidebar {
            return vec![
                ("↑↓", "switch screen"),
                ("⏎", "open"),
                ("?", "help"),
                ("q", "quit"),
            ];
        }
        let mut pairs = match self.screen {
            Screen::Activity => vec![
                ("↑↓", "move"),
                ("⏎", "actions"),
                ("/", "search"),
                ("f", "filter"),
                ("e", if self.enabled { "turn off" } else { "turn on" }),
            ],
            Screen::Review => vec![
                ("↑↓", "move"),
                ("a", "keep"),
                ("d", "remove"),
                ("space", "mark"),
                ("A", "keep marked"),
                ("D", "remove marked"),
            ],
            Screen::Dictionary => vec![
                ("←→", "section"),
                ("↑↓", "move"),
                ("n", "new"),
                ("d", "delete"),
                ("/", "filter"),
            ],
            Screen::Settings => vec![("↑↓", "move"), ("←→", "change"), ("⏎", "edit")],
        };
        if self.undo.is_some() {
            pairs.push(("u", "undo"));
        }
        pairs.extend([("esc", "sidebar"), ("?", "help"), ("q", "quit")]);
        pairs
    }

    fn render_hints(&self, frame: &mut Frame, area: Rect) {
        let mut spans = vec![Span::raw(" ")];
        for (k, label) in self.hint_pairs() {
            spans.push(Span::styled(
                k,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" {label}   "),
                Style::default().fg(MUTED),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Splits a window into a list box and a detail box when there is room.
    fn split_list_detail(area: Rect) -> (Rect, Option<Rect>) {
        if area.width >= DETAIL_MIN_WIDTH {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
                .split(area);
            (cols[0], Some(cols[1]))
        } else {
            (area, None)
        }
    }

    fn row_style(&self) -> Style {
        if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        }
    }

    fn render_activity(&self, frame: &mut Frame, area: Rect) {
        let focused = !self.focus_sidebar;
        let (list_area, detail_area) = Self::split_list_detail(area);

        let mut title = String::from(" Activity");
        if self.activity_filter != ActivityFilter::All {
            title.push_str(&format!(" · {}", self.activity_filter.label()));
        }
        if self.filter_editing || !self.activity_search.is_empty() {
            title.push_str(&format!(
                " · / {}{}",
                self.activity_search,
                if self.filter_editing { "▏" } else { "" }
            ));
        }
        title.push_str(&format!(" ({}) ", self.activity_view.len()));
        let block = panel(Line::from(title), focused);
        let inner = block.inner(list_area);
        frame.render_widget(block, list_area);

        if let Some(d) = detail_area {
            self.render_activity_detail(frame, d);
        }

        if self.activity_view.is_empty() {
            let text = if self.activity.is_empty() {
                "No activity yet. Type in any app with SmartKey on; fixes appear here within a few seconds."
            } else {
                "Nothing matches. Press esc to clear the search or f to change the filter."
            };
            frame.render_widget(empty_state(text), inner);
            return;
        }

        let narrow = inner.width < 70;
        let now = current_epoch_ms();
        let rows: Vec<Row> = self
            .activity_view
            .iter()
            .map(|&i| {
                let r = &self.activity[i];
                let age = if r.ts_ms == 0 {
                    "–".to_string()
                } else {
                    format_age_ago(now, r.ts_ms)
                };
                let main_style = match r.tone {
                    Tone::Normal => Style::default(),
                    Tone::Warn => Style::default().fg(Color::Yellow),
                    Tone::Muted => Style::default().fg(MUTED),
                };
                let mut why = r.why.clone();
                if r.repeat > 1 {
                    why.push_str(&format!("  ×{}", r.repeat));
                }
                let mut cells = vec![
                    Cell::from(Span::styled(age, Style::default().fg(MUTED))),
                    Cell::from(Span::styled(r.change.clone(), main_style)),
                    Cell::from(Span::styled(why, Style::default().fg(MUTED))),
                ];
                if !narrow {
                    let mut app = r.app.clone().unwrap_or_default();
                    if r.lang != "en" && r.lang != "-" {
                        app = format!("{app} [{}]", r.lang);
                    }
                    cells.push(Cell::from(Span::styled(app, Style::default().fg(MUTED))));
                    cells.push(Cell::from(Span::styled(
                        r.note.clone(),
                        Style::default().fg(MUTED),
                    )));
                }
                Row::new(cells)
            })
            .collect();
        let widths: Vec<Constraint> = if narrow {
            vec![
                Constraint::Length(4),
                Constraint::Fill(3),
                Constraint::Fill(2),
            ]
        } else {
            vec![
                Constraint::Length(4),
                Constraint::Fill(4),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Fill(2),
            ]
        };
        let header = if narrow {
            Row::new(vec!["When", "Change", "Why"])
        } else {
            Row::new(vec!["When", "Change", "Why", "App", ""])
        }
        .style(Style::default().fg(MUTED));
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(self.row_style());
        let mut state = TableState::default();
        state.select(Some(self.activity_idx));
        frame.render_stateful_widget(table, inner, &mut state);
    }

    fn render_activity_detail(&self, frame: &mut Frame, area: Rect) {
        let block = panel(Line::from(" Details "), false);
        let muted = Style::default().fg(MUTED);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(row) = self.selected_activity() {
            let now = current_epoch_ms();
            let kv = |k: &str, v: String| {
                Line::from(vec![
                    Span::styled(format!("{k:<6}"), Style::default().fg(MUTED)),
                    Span::raw(v),
                ])
            };
            lines.push(Line::from(Span::styled(
                if row.change.is_empty() {
                    row.why.clone()
                } else {
                    row.change.clone()
                },
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            if !row.change.is_empty() {
                lines.push(kv("Why", row.why.clone()));
            }
            if !row.note.is_empty() {
                lines.push(kv("Note", row.note.clone()));
            }
            if row.ts_ms > 0 {
                lines.push(kv(
                    "When",
                    format!("{} ago", format_age_ago(now, row.ts_ms)),
                ));
            }
            if let Some(app) = row.app.as_deref().filter(|a| !a.is_empty()) {
                let lang = if row.lang != "en" && row.lang != "-" {
                    format!(" [{}]", row.lang)
                } else {
                    String::new()
                };
                lines.push(kv("App", format!("{app}{lang}")));
            }
            if row.repeat > 1 {
                lines.push(kv("Seen", format!("{}× in a row", row.repeat)));
            }
            let actions = activity_actions(row);
            if !actions.is_empty() {
                lines.push(Line::from(""));
            }
            for a in actions {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{}  ", a.key),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(a.label),
                ]));
                lines.push(Line::from(Span::styled(format!("   {}", a.hint), muted)));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "Select a row to see what happened and what you can do about it.",
                muted,
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }

    fn render_review(&self, frame: &mut Frame, area: Rect) {
        let focused = !self.focus_sidebar;
        let (list_area, detail_area) = Self::split_list_detail(area);

        let mut title = format!(" Review ({})", self.review.len());
        if !self.review_marked.is_empty() {
            title.push_str(&format!(" · {} marked", self.review_marked.len()));
        }
        title.push(' ');
        let block = panel(Line::from(title), focused);
        let inner = block.inner(list_area);
        frame.render_widget(block, list_area);

        if let Some(d) = detail_area {
            self.render_review_detail(frame, d);
        }

        if self.review.is_empty() {
            frame.render_widget(
                empty_state("Nothing needs a decision. When SmartKey learns a rule, or a rule keeps getting undone, it shows up here for a yes or no."),
                inner,
            );
            return;
        }

        let mut items = Vec::new();
        let mut selected_line = 0usize;
        let mut last: Option<ReviewSection> = None;
        for (i, item) in self.review.iter().enumerate() {
            if last != Some(item.section) {
                if last.is_some() {
                    items.push(ListItem::new(""));
                }
                items.push(ListItem::new(Line::from(Span::styled(
                    format!(" {}", item.section.title()),
                    Style::default().add_modifier(Modifier::BOLD),
                ))));
                last = Some(item.section);
            }
            if i == self.review_idx {
                selected_line = items.len();
            }
            let mark = if self.review_marked.contains(&item.id) {
                "[x] "
            } else {
                "[ ] "
            };
            let mut spans = vec![
                Span::styled(mark, Style::default().fg(ACCENT)),
                Span::raw(format!("{:<26} ", truncate(&item.title, 26))),
            ];
            if detail_area.is_none() || inner.width >= 80 {
                spans.push(Span::styled(
                    item.detail.clone(),
                    Style::default().fg(MUTED),
                ));
            }
            if let Some(w) = item.warning {
                spans.push(Span::styled(
                    format!("  {w}"),
                    Style::default().fg(Color::Yellow),
                ));
            }
            items.push(ListItem::new(Line::from(spans)));
        }
        let list = List::new(items).highlight_style(self.row_style());
        let mut state = ListState::default();
        state.select(Some(selected_line));
        frame.render_stateful_widget(list, inner, &mut state);
    }

    fn render_review_detail(&self, frame: &mut Frame, area: Rect) {
        let block = panel(Line::from(" Details "), false);
        let muted = Style::default().fg(MUTED);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(item) = self.review.get(self.review_idx) {
            lines.push(Line::from(Span::styled(
                item.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(item.detail.clone(), muted)));
            if let Some(w) = item.warning {
                lines.push(Line::from(Span::styled(
                    w,
                    Style::default().fg(Color::Yellow),
                )));
            }
            let (keep, remove) = item.section.actions_help();
            lines.push(Line::from(""));
            for (k, label, help) in [("a", "Keep", keep), ("d", "Remove", remove)] {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{k}  "),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(label),
                ]));
                lines.push(Line::from(Span::styled(format!("   {help}"), muted)));
            }
            lines.push(Line::from(vec![
                Span::styled(
                    "space  ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("Mark, then A or D for all marked"),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "Nothing to review right now.",
                muted,
            )));
        }
        if self.review_hidden > 0 {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "{} one-off typos are being watched. They only become rules if you repeat them.",
                    self.review_hidden
                ),
                muted,
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }

    fn render_dictionary(&self, frame: &mut Frame, area: Rect) {
        let focused = !self.focus_sidebar;
        let mut spans = vec![Span::raw(" ")];
        for (i, s) in DictSection::ALL.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Style::default().fg(RULE)));
            }
            let style = if *s == self.dict_section {
                Style::default()
                    .fg(ACCENT)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(MUTED)
            };
            spans.push(Span::styled(
                format!("{} {}", s.label(), self.dict_counts[s.index()]),
                style,
            ));
        }
        spans.push(Span::raw(" "));
        let mut block = panel(Line::from(spans), focused);
        if self.filter_editing || !self.dict_filter.is_empty() {
            block = block.title_bottom(Line::from(Span::styled(
                format!(
                    " / {}{} ",
                    self.dict_filter,
                    if self.filter_editing { "▏" } else { "" }
                ),
                Style::default().fg(ACCENT),
            )));
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);

        if self.dict_view.is_empty() {
            let text = if self.dict_filter.is_empty() {
                self.dict_section.empty_text()
            } else {
                "Nothing matches. Press esc to clear the filter."
            };
            frame.render_widget(empty_state(text), inner);
            return;
        }
        let rows: Vec<Row> = self
            .dict_view
            .iter()
            .map(|&i| {
                let r = &self.dict[i];
                let cells: Vec<Cell> = r
                    .cols
                    .iter()
                    .enumerate()
                    .map(|(ci, v)| {
                        if ci == 0 {
                            Cell::from(v.clone())
                        } else {
                            Cell::from(Span::styled(v.clone(), Style::default().fg(MUTED)))
                        }
                    })
                    .collect();
                Row::new(cells)
            })
            .collect();
        let table = Table::new(rows, self.dict_section.widths())
            .header(
                Row::new(self.dict_section.headers().to_vec()).style(Style::default().fg(MUTED)),
            )
            .row_highlight_style(self.row_style());
        let mut state = TableState::default();
        state.select(Some(self.dict_idx));
        frame.render_stateful_widget(table, inner, &mut state);
    }

    fn render_settings(&self, frame: &mut Frame, area: Rect) {
        let focused = !self.focus_sidebar;
        let (list_area, help_area) = if area.width >= DETAIL_MIN_WIDTH {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(66), Constraint::Percentage(34)])
                .split(area);
            (cols[0], cols[1])
        } else {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(5)])
                .split(area);
            (rows[0], rows[1])
        };

        let block = panel(Line::from(" Settings "), focused);
        let inner = block.inner(list_area);
        frame.render_widget(block, list_area);

        let label_w = 23usize;
        let room_for_options = inner.width >= 54;
        let items: Vec<ListItem> = self
            .settings_rows
            .iter()
            .enumerate()
            .map(|(idx, row)| match row {
                SettingsRow::Header(h) => ListItem::new(Line::from(Span::styled(
                    format!(" {h}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ))),
                SettingsRow::Setting(spec) => {
                    let value = self
                        .settings_values
                        .get(spec.key)
                        .cloned()
                        .unwrap_or_default();
                    let mut spans = vec![Span::raw(format!("   {:<label_w$}", spec.label))];
                    spans.extend(setting_value_spans(
                        spec,
                        &value,
                        room_for_options && idx == self.settings_idx,
                    ));
                    ListItem::new(Line::from(spans))
                }
                SettingsRow::App {
                    app,
                    profile,
                    reason,
                    chosen,
                } => {
                    let value_style = if *profile == AppProfile::Bypass {
                        Style::default().fg(Color::Yellow)
                    } else {
                        Style::default()
                    };
                    let source = if chosen.is_some() {
                        "your choice"
                    } else {
                        reason_label(reason)
                    };
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("   {:<label_w$}", truncate(app, label_w - 1))),
                        Span::styled(profile_label(*profile), value_style),
                        Span::styled(format!(" · {source}"), Style::default().fg(MUTED)),
                    ]))
                }
            })
            .collect();
        let list = List::new(items).highlight_style(self.row_style());
        let mut state = ListState::default();
        state.select(Some(self.settings_idx));
        frame.render_stateful_widget(list, inner, &mut state);

        let (title, body) = match self.settings_rows.get(self.settings_idx) {
            Some(SettingsRow::Setting(spec)) => (spec.label.to_string(), spec.help.to_string()),
            Some(SettingsRow::App { app, .. }) => (
                app.clone(),
                "Choose how SmartKey treats this app. Off means it never touches your typing there; Browser skips address bars and password fields. Press Enter for the list or ←→ to cycle.".to_string(),
            ),
            _ => (String::new(), String::new()),
        };
        let help = Paragraph::new(vec![
            Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(body, Style::default().fg(MUTED))),
        ])
        .wrap(Wrap { trim: true })
        .block(panel(Line::from(" About this setting "), false));
        frame.render_widget(help, help_area);
    }

    fn render_modal(&self, frame: &mut Frame, area: Rect, modal: &Modal) {
        let (title, lines): (String, Vec<Line>) = match modal {
            Modal::Help => ("Keys".to_string(), self.help_lines()),
            Modal::Info { title, lines } => (
                title.clone(),
                lines.iter().map(|l| Line::from(l.clone())).collect(),
            ),
            Modal::Actions { title, items, selected } => {
                let mut out: Vec<Line> = Vec::new();
                if items.is_empty() {
                    out.push(Line::from(Span::styled("Nothing to do for this event.", Style::default().fg(MUTED))));
                }
                let label_w = items.iter().map(|i| i.label.chars().count()).max().unwrap_or(0);
                for (i, item) in items.iter().enumerate() {
                    let style = if i == *selected { selected_style() } else { Style::default() };
                    out.push(Line::from(vec![
                        Span::styled(format!(" {} ", item.key), style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {:<label_w$}", item.label), style),
                        Span::styled(format!("   {}", item.hint), Style::default().fg(MUTED)),
                    ]));
                }
                (title.clone(), out)
            }
            Modal::ExplainInput { input } => (
                "Explain a word".to_string(),
                vec![
                    Line::from(Span::styled(
                        "Type a word, or two words for context (\"over ther\"). Prefix vi: for Vietnamese.",
                        Style::default().fg(MUTED),
                    )),
                    Line::from(""),
                    input_line(input),
                ],
            ),
            Modal::EditNumber { spec, current, input } => (
                spec.label.to_string(),
                vec![
                    Line::from(Span::styled(
                        format!("Now {}. Type a new value{}.", display_setting(spec, current), unit_hint(spec)),
                        Style::default().fg(MUTED),
                    )),
                    Line::from(""),
                    input_line(input),
                ],
            ),
            Modal::AppPicker { app, selected } => {
                let mut out = vec![Line::from(Span::styled(
                    format!("How should SmartKey treat {app}?"),
                    Style::default().fg(MUTED),
                ))];
                out.push(Line::from(""));
                for (i, (_, label, hint)) in APP_CHOICES.iter().enumerate() {
                    let style = if i == *selected { selected_style() } else { Style::default() };
                    out.push(Line::from(vec![
                        Span::styled(format!(" {label:<9}"), style),
                        Span::styled(format!(" {hint}"), Style::default().fg(MUTED)),
                    ]));
                }
                ("App".to_string(), out)
            }
            Modal::Form(form) => {
                let mut out = Vec::new();
                for (i, f) in form.fields.iter().enumerate() {
                    let focused = i == form.focus;
                    let label_style = if focused {
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(MUTED)
                    };
                    let opt = if f.required { "" } else { " (optional)" };
                    out.push(Line::from(vec![
                        Span::styled(format!("{:<18}", format!("{}{opt}", f.label)), label_style),
                        Span::raw(f.value.clone()),
                        Span::styled(if focused { "▏" } else { "" }, Style::default().fg(ACCENT)),
                    ]));
                }
                (format!("New {}", form.section.label().to_lowercase()), out)
            }
            Modal::Confirm { prompt, .. } => (
                "Confirm".to_string(),
                vec![Line::from(Span::styled(
                    prompt.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ))],
            ),
        };

        let width = area.width.saturating_sub(4).clamp(20, 76);
        let height = (lines.len() as u16 + 2)
            .min(area.height.saturating_sub(2))
            .max(3);
        let popup = Rect::new(
            area.x + (area.width.saturating_sub(width)) / 2,
            area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        );
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(ACCENT))
                    .title(Span::styled(
                        format!(" {title} "),
                        Style::default().add_modifier(Modifier::BOLD),
                    )),
            ),
            popup,
        );
    }

    fn help_lines(&self) -> Vec<Line<'static>> {
        let row = |k: &str, v: &str| {
            Line::from(vec![
                Span::styled(
                    format!(" {k:<12}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(v.to_string(), Style::default().fg(MUTED)),
            ])
        };
        let mut out = vec![
            row("1-4, tab", "switch screen"),
            row("esc", "go to the sidebar (⏎ comes back)"),
            row("u", "undo the last change"),
            row("r", "reload from disk"),
            row("q", "quit"),
            Line::from(""),
        ];
        out.extend(match self.screen {
            Screen::Activity => vec![
                row(
                    "⏎",
                    "act on this fix: block it, keep it, add the word, explain",
                ),
                row("b a w p", "run that action directly (see Details)"),
                row("/", "search"),
                row("f", "show everything / fixes / undos / missed words"),
                row("x", "explain any word"),
                row("e", "turn SmartKey on or off"),
            ],
            Screen::Review => vec![
                row("a", "keep: make this rule permanent, or learn it now"),
                row("d", "remove: delete the rule, or forget the staged fix"),
                row("space", "mark several, then A or D for all marked"),
            ],
            Screen::Dictionary => vec![
                row("←→", "switch section"),
                row("n", "add an entry"),
                row("d", "delete (u brings it back)"),
                row("/", "filter"),
            ],
            Screen::Settings => vec![
                row("←→", "change the value"),
                row("⏎", "toggle, or type a number, or pick an app mode"),
            ],
        });
        out
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn panel<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
    let (border, title_style) = if focused {
        (
            Style::default().fg(ACCENT),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        )
    } else {
        (Style::default().fg(RULE), Style::default().fg(MUTED))
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border)
        .title(title)
        .title_style(title_style)
}

fn selected_style() -> Style {
    Style::default()
        .fg(ACCENT)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

fn warn_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" ⚠ {text}"),
        Style::default().fg(Color::Yellow),
    ))
}

fn empty_state(text: &str) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(MUTED),
        )),
    ])
    .wrap(Wrap { trim: false })
}

fn input_line(input: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("> ", Style::default().fg(ACCENT)),
        Span::raw(input.to_string()),
        Span::styled("▏", Style::default().fg(ACCENT)),
    ])
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn step(index: &mut usize, delta: isize, len: usize) {
    if len == 0 {
        *index = 0;
        return;
    }
    let cur = (*index).min(len - 1) as isize;
    *index = (cur + delta).clamp(0, len as isize - 1) as usize;
}

fn clamp(index: &mut usize, len: usize) {
    if len == 0 {
        *index = 0;
    } else if *index >= len {
        *index = len - 1;
    }
}

fn cli_error_text(err: &CliError) -> &str {
    match err {
        CliError::Usage(m) | CliError::Runtime(m) => m,
    }
}

fn split_lang_prefix(input: &str) -> (LangId, &str) {
    if let Some((prefix, rest)) = input.split_once(':') {
        let p = prefix.trim();
        if (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic()) {
            return (LangId(p.to_ascii_lowercase()), rest);
        }
    }
    (LangId::en(), input)
}

fn format_number(v: f64, decimals: usize) -> String {
    if decimals == 0 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.decimals$}")
    }
}

fn unit_hint(spec: &SettingSpec) -> &'static str {
    match spec.control {
        Control::Number { unit: "ms", .. } => " in milliseconds",
        Control::Number { unit: "days", .. } => " in days",
        _ => "",
    }
}

fn display_setting(spec: &SettingSpec, value: &str) -> String {
    match spec.control {
        Control::Toggle => if value == "true" { "on" } else { "off" }.to_string(),
        Control::Choice(opts) => opts
            .iter()
            .find(|(v, _)| *v == value)
            .map(|(_, l)| l.to_string())
            .unwrap_or_else(|| value.to_string()),
        Control::Number { unit, .. } => match unit {
            "ms" => value
                .parse::<f64>()
                .map(|ms| format!("{:.1} s", ms / 1000.0))
                .unwrap_or_else(|_| value.to_string()),
            "days" => format!("{value} days"),
            _ => value.to_string(),
        },
    }
}

fn setting_value_spans(spec: &SettingSpec, value: &str, show_all: bool) -> Vec<Span<'static>> {
    match spec.control {
        Control::Toggle => {
            if value == "true" {
                vec![Span::styled("on", Style::default().fg(Color::Green))]
            } else {
                vec![Span::styled("off", Style::default().fg(MUTED))]
            }
        }
        Control::Choice(opts) if show_all => {
            let mut spans = vec![Span::styled("‹ ", Style::default().fg(MUTED))];
            for (v, label) in opts {
                if *v == value {
                    spans.push(Span::styled(
                        format!("[{label}]"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ));
                } else {
                    spans.push(Span::styled(label.to_string(), Style::default().fg(MUTED)));
                }
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled("›", Style::default().fg(MUTED)));
            spans
        }
        Control::Choice(_) => vec![Span::raw(display_setting(spec, value))],
        Control::Number { .. } => vec![
            Span::raw(display_setting(spec, value)),
            Span::styled("   ←→ adjust", Style::default().fg(MUTED)),
        ],
    }
}

fn profile_label(p: AppProfile) -> &'static str {
    match p {
        AppProfile::Bypass => "off",
        AppProfile::Browser => "browser",
        AppProfile::Chat => "chat",
        AppProfile::Prose => "prose",
    }
}

fn reason_label(reason: &str) -> &'static str {
    match reason {
        "program-list" => "built-in (terminal/editor)",
        "browser" | "chat" => "built-in",
        "prose" => "default",
        "user-app-override" => "your choice",
        "unknown-app-policy" => "unlisted-app rule",
        _ => "built-in",
    }
}

fn kind_label(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Bypass => "skipped",
        MatchKind::ValidWord => "real word",
        MatchKind::StretchedWord => "stretched word",
        MatchKind::UserSnippet => "your snippet",
        MatchKind::ShorthandExpansion => "shorthand",
        MatchKind::UserContextRule => "your fix (after a word)",
        MatchKind::UserCorrectionRule => "your fix",
        MatchKind::ShiftSlip => "Shift slip",
        MatchKind::ProperNoun => "name capitalised",
        MatchKind::DialectNormalization => "US/UK spelling",
        MatchKind::Contraction => "apostrophe",
        MatchKind::UnmergedCompound => "joined word",
        MatchKind::SemicolonContraction => "; typed for '",
        MatchKind::TransposedApostrophe => "apostrophe moved",
        MatchKind::KnownTypoDict => "common typo",
        MatchKind::SwitchChatter => "double key press",
        MatchKind::MissedSpaceSplit => "missed space",
        MatchKind::SpaceShift => "space in wrong place",
        MatchKind::FuzzyLexicon => "spelling",
        MatchKind::SmashRescue => "scrambled letters",
        MatchKind::PersonalDict => "your word",
        MatchKind::VietnameseTelexRescue => "Telex fix",
    }
}

fn manual_outcome_label(outcome: &str) -> String {
    if outcome.starts_with("skipped") {
        "not learned".to_string()
    } else if outcome.starts_with("staged") {
        "learning".to_string()
    } else if outcome.contains("graduat") || outcome.contains("learn") || outcome.contains("promot")
    {
        "learned".to_string()
    } else {
        outcome.replace('_', " ")
    }
}

fn describe_event(ev: &DiagnosticEvent) -> (String, String, String, Tone) {
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

fn explain_target(ev: &DiagnosticEvent) -> Option<(Option<String>, String)> {
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

fn activity_actions(row: &ActivityRow) -> Vec<ActionItem> {
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
                    ops: vec![StoreOp::AddBlock {
                        typed: typed.clone(),
                        rejected: applied.clone(),
                    }],
                    msg: format!("SmartKey will never change {typed} → {applied}"),
                },
            );
            if *kind != MatchKind::UserCorrectionRule {
                add(
                    'a',
                    "Always fix it this way".to_string(),
                    "saves it as your fix".to_string(),
                    RowAction::Ops {
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
                    ops: vec![StoreOp::AddBlock {
                        typed: typed.clone(),
                        rejected: rejected.clone(),
                    }],
                    msg: format!("SmartKey will never change {typed} → {rejected}"),
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

fn humanize_rule(rule: &str) -> String {
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

/// Staging counters keep ticking after a rule graduates; hide the ones that are already rules.
fn staged_already_active(queue: &str, key: &str, active: &BTreeSet<String>) -> bool {
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

fn describe_staged(
    queue: &str,
    key: &str,
    canonical: Option<&str>,
    count: u32,
    threshold: u32,
) -> Option<(String, String)> {
    let progress = if count >= threshold {
        format!("{count}× (not learned yet)")
    } else {
        format!("{count} of {threshold} times")
    };
    match queue {
        "fix" => {
            let (lang, pair) = key.split_once(':')?;
            let (typed, fixed) = pair.split_once("->")?;
            let tag = if lang == "en" {
                String::new()
            } else {
                format!("  ({lang})")
            };
            Some((
                format!("{typed} → {fixed}{tag}"),
                format!("you fixed this {progress}"),
            ))
        }
        "context" => {
            let (lhs, fixed) = key.split_once("->")?;
            let (prev, typed) = lhs.split_once('|')?;
            Some((
                format!("{prev} {typed} → {fixed}"),
                format!("you fixed this {progress}"),
            ))
        }
        "untouched_word" => {
            let (lang, word) = key.split_once(':')?;
            let shown = canonical.unwrap_or(word);
            let tag = if lang == "en" {
                String::new()
            } else {
                format!("  ({lang})")
            };
            Some((
                format!("{shown}{tag}"),
                format!("typed {progress} without fixing"),
            ))
        }
        "case_fix" => {
            let rest = key.strip_prefix("case:")?;
            let (_, word) = rest.split_once(':')?;
            Some((
                canonical.unwrap_or(word).to_string(),
                format!("capitalisation fixed {progress}"),
            ))
        }
        "telex_passthrough" => Some((
            key.to_string(),
            format!("kept as typed in Telex {progress}"),
        )),
        _ => None,
    }
}

fn collect_rules(store: &RuleStore) -> Vec<RuleEntry> {
    let c = store.compiled();
    let mut out = Vec::new();
    let mut push = |kind: RuleKind,
                    lang: LangId,
                    a: &str,
                    b: &str,
                    prev: &str,
                    stat_key: Option<String>,
                    pinned: bool| {
        let (tier, origin, hits, strikes, last_hit_ms) = match &stat_key {
            Some(k) => {
                let (tier, stat, hits, strikes) = store.classify_rule_confidence(k);
                (
                    tier,
                    stat.map(|s| s.origin),
                    hits,
                    strikes,
                    stat.and_then(|s| s.last_hit_epoch_ms),
                )
            }
            None => (ConfidenceTier::Manual, Some(RuleOrigin::Manual), 0, 0, None),
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
            last_hit_ms,
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

fn promote_staged_entry(
    store: &mut RuleStore,
    queue: &str,
    key: &str,
    canonical: Option<&str>,
) -> Result<(), String> {
    let e = |err: io::Error| err.to_string();
    match queue {
        "fix" => {
            let (lang, pair) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            let (typed, fixed) = pair
                .split_once("->")
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            store
                .set_user_correction(&LangId(lang.to_string()), typed, fixed)
                .map_err(e)?;
        }
        "case_fix" => {
            let rest = key.strip_prefix("case:").unwrap_or(key);
            let (lang, word) = rest
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged word '{key}'"))?;
            store
                .add_user_personal_word(&LangId(lang.to_string()), canonical.unwrap_or(word))
                .map_err(e)?;
        }
        "context" => {
            let (lhs, fixed) = key
                .split_once("->")
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            let (prev, typed) = lhs
                .split_once('|')
                .ok_or_else(|| format!("Malformed staged fix '{key}'"))?;
            store
                .set_user_context_correction(prev, typed, fixed)
                .map_err(e)?;
        }
        "untouched_word" => {
            let (lang, word) = key
                .split_once(':')
                .ok_or_else(|| format!("Malformed staged word '{key}'"))?;
            store
                .add_user_personal_word(&LangId(lang.to_string()), canonical.unwrap_or(word))
                .map_err(e)?;
        }
        "telex_passthrough" => {
            store.add_user_telex_passthrough(key).map_err(e)?;
        }
        other => {
            return Err(format!(
                "SmartKey can't learn '{other}' entries early; remove it instead"
            ))
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use smartkey_core::event_log::DiagnosticsConfig;
    use smartkey_core::RuleStat;

    static NONCE: AtomicU64 = AtomicU64::new(1);
    static RELOADS: AtomicUsize = AtomicUsize::new(0);

    fn test_reload() -> bool {
        RELOADS.fetch_add(1, Ordering::SeqCst);
        true
    }

    struct Dirs {
        base: PathBuf,
        cfg: PathBuf,
        logs: PathBuf,
    }

    impl Drop for Dirs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn dirs(name: &str) -> Dirs {
        let base = std::env::temp_dir().join(format!(
            "smartkey_tui_{name}_{}_{}",
            std::process::id(),
            NONCE.fetch_add(1, Ordering::Relaxed)
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
        Dirs { base, cfg, logs }
    }

    fn log_event(logs: &Path, app: &str, ev: DiagnosticEvent) {
        let mut w = EventLog::new();
        w.set_dir(Some(logs.to_path_buf()));
        w.set_app(Some(app));
        w.record(&DiagnosticsConfig::default(), "en", &ev);
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(app: &mut App, c: char) {
        app.handle_key(key(KeyCode::Char(c)));
    }

    fn type_text(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, c);
        }
    }

    fn store(d: &Dirs) -> RuleStore {
        load_rule_store_readonly(&d.cfg, Preset::Balanced)
            .unwrap()
            .0
    }

    fn screen_text(app: &App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer().clone();
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
    fn tab_bar_is_visible_and_every_screen_renders_at_any_size() {
        let d = dirs("render");
        log_event(
            &d.logs,
            "kitty",
            DiagnosticEvent::AutocorrectApplied {
                typed: "teh".into(),
                applied: "the".into(),
                kind: MatchKind::KnownTypoDict,
                total_count: 1,
            },
        );
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        let top = screen_text(&app, 110, 32);
        for label in ["1 Activity", "2 Review", "3 Dictionary", "4 Settings"] {
            assert!(top.contains(label), "sidebar missing {label}:\n{top}");
        }
        assert!(top.contains("╭"), "panels should be drawn as boxes:\n{top}");
        assert!(top.contains("Details"), "detail box missing:\n{top}");
        assert!(top.contains("teh → the"), "activity row missing:\n{top}");

        let compact = screen_text(&app, 60, 16);
        let first_line = compact.lines().next().unwrap();
        for label in ["1 Act", "2 Rev", "3 Dict", "4 Set"] {
            assert!(
                first_line.contains(label),
                "compact tab strip missing {label}: {first_line}"
            );
        }

        for (w, h) in [(110u16, 32u16), (60, 16), (40, 12), (24, 8), (10, 4)] {
            for c in ['1', '2', '3', '4'] {
                press(&mut app, c);
                screen_text(&app, w, h);
                press(&mut app, '?');
                screen_text(&app, w, h);
                app.handle_key(key(KeyCode::Esc));
            }
        }
    }

    #[test]
    fn activity_row_actions_block_a_fix_and_undo_restores_it() {
        let d = dirs("activity");
        log_event(
            &d.logs,
            "kitty",
            DiagnosticEvent::AutocorrectApplied {
                typed: "coule".into(),
                applied: "could".into(),
                kind: MatchKind::KnownTypoDict,
                total_count: 1,
            },
        );
        let before = RELOADS.load(Ordering::SeqCst);
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        assert_eq!(app.activity.len(), 1);

        app.handle_key(key(KeyCode::Enter));
        let Some(Modal::Actions { items, .. }) = &app.modal else {
            panic!("expected action menu");
        };
        let keys: Vec<char> = items.iter().map(|i| i.key).collect();
        assert_eq!(keys, vec!['b', 'a', 'w', 'x', 'p']);

        press(&mut app, 'b');
        assert!(app.modal.is_none());
        assert!(store(&d)
            .compiled()
            .blocklist
            .contains(&("coule".to_string(), "could".to_string())));
        assert!(RELOADS.load(Ordering::SeqCst) > before);
        assert!(app.undo.is_some());

        press(&mut app, 'u');
        assert!(!store(&d)
            .compiled()
            .blocklist
            .contains(&("coule".to_string(), "could".to_string())));
        assert!(app.undo.is_none());

        app.handle_key(key(KeyCode::Enter));
        press(&mut app, 'w');
        assert!(store(&d).compiled().personal_dict.contains("coule"));
    }

    #[test]
    fn activity_explain_toggle_search_and_filter() {
        let d = dirs("explain");
        log_event(
            &d.logs,
            "kitty",
            DiagnosticEvent::ManualBackspaceFix {
                orig: "mt".into(),
                fixed: "me".into(),
                outcome: "staged (1x)".into(),
                attempt_count: 1,
            },
        );
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        assert_eq!(app.activity[0].why, "you fixed it");
        assert_eq!(app.activity[0].note, "learning");

        press(&mut app, 'x');
        let Some(Modal::ExplainInput { input }) = &app.modal else {
            panic!("expected explain input");
        };
        assert_eq!(input, "mt");
        for _ in 0..2 {
            app.handle_key(key(KeyCode::Backspace));
        }
        type_text(&mut app, "teh");
        app.handle_key(key(KeyCode::Enter));
        let Some(Modal::Info { lines, .. }) = &app.modal else {
            panic!("expected explain result");
        };
        assert!(lines[0].contains("“the”"), "explain: {lines:?}");
        app.handle_key(key(KeyCode::Esc));

        press(&mut app, 'f');
        assert_eq!(app.activity_filter, ActivityFilter::Fixes);
        assert!(app.activity_view.is_empty());
        press(&mut app, 'f');
        assert_eq!(app.activity_view.len(), 1);

        press(&mut app, '/');
        type_text(&mut app, "zzz");
        assert!(app.activity_view.is_empty());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.activity_view.len(), 1);

        press(&mut app, 'e');
        assert!(!store(&d).compiled().enabled);
        press(&mut app, 'u');
        assert!(store(&d).compiled().enabled);
    }

    #[test]
    fn review_shows_only_shaky_rules_and_repeated_staging() {
        let d = dirs("review");
        {
            let mut s = load_rule_store_mut(&d.cfg, Preset::Balanced).unwrap();
            s.add_user_personal_word(&LangId::en(), "zorblax").unwrap();
            s.set_user_correction(&LangId::en(), "dont", "don't")
                .unwrap();
            s.state_mut().rule_stats.insert(
                RuleStore::stat_key_word("en", "zorblax"),
                RuleStat {
                    origin: RuleOrigin::Untouched3x,
                    created_epoch_ms: current_epoch_ms(),
                    last_hit_epoch_ms: None,
                    hits: 0,
                },
            );
            let st = s.state_mut();
            st.staged_corrections.insert("en:al->all".into(), 2);
            st.staged_corrections.insert("en:abl->sable".into(), 1);
            st.staged_corrections.insert("en:dont->don't".into(), 3);
            st.untouched_unknowns.insert("en:hyprland".into(), 2);
            st.staged_personal_canonical
                .insert("en:hyprland".into(), "Hyprland".into());
            s.flush_state().unwrap();
        }
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        let titles: Vec<&str> = app.review.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["zorblax", "al → all", "Hyprland"]);
        assert_eq!(app.review_hidden, 1);
        assert!(app.review[0].detail.contains("never used since"));

        press(&mut app, '2');
        press(&mut app, 'a');
        let s = store(&d);
        let (tier, ..) = s.classify_rule_confidence(&RuleStore::stat_key_word("en", "zorblax"));
        assert_eq!(tier, ConfidenceTier::Manual);
        assert_eq!(app.review.len(), 2);

        press(&mut app, 'a');
        assert_eq!(
            store(&d)
                .compiled()
                .corrections
                .get("al")
                .map(String::as_str),
            Some("all")
        );
        assert!(!store(&d)
            .state()
            .staged_corrections
            .contains_key("en:al->all"));

        press(&mut app, ' ');
        press(&mut app, 'D');
        assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
        press(&mut app, 'y');
        assert!(!store(&d)
            .state()
            .untouched_unknowns
            .contains_key("en:hyprland"));
        assert!(app.review.is_empty());
    }

    #[test]
    fn dictionary_form_adds_fix_and_delete_can_be_undone() {
        let d = dirs("dict");
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        press(&mut app, '3');
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.dict_section, DictSection::Fixes);

        press(&mut app, 'n');
        type_text(&mut app, "recieve");
        app.handle_key(key(KeyCode::Enter));
        let Some(Modal::Form(form)) = &app.modal else {
            panic!("form should stay open while Becomes is empty");
        };
        assert_eq!(form.focus, 1);
        type_text(&mut app, "receive");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.modal.is_none());
        assert_eq!(
            store(&d)
                .compiled()
                .corrections
                .get("recieve")
                .map(String::as_str),
            Some("receive")
        );
        assert_eq!(app.dict_view.len(), 1);

        press(&mut app, 'd');
        assert!(!store(&d).compiled().corrections.contains_key("recieve"));
        press(&mut app, 'u');
        assert_eq!(
            store(&d)
                .compiled()
                .corrections
                .get("recieve")
                .map(String::as_str),
            Some("receive")
        );

        app.handle_key(key(KeyCode::Left));
        press(&mut app, 'n');
        type_text(&mut app, "Hyprland");
        app.handle_key(key(KeyCode::Enter));
        assert!(store(&d).compiled().personal_dict.contains("hyprland"));
        press(&mut app, '/');
        type_text(&mut app, "nomatch");
        assert!(app.dict_view.is_empty());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.dict_view.len(), 1);
    }

    #[test]
    fn settings_change_values_and_app_modes() {
        let d = dirs("settings");
        log_event(
            &d.logs,
            "obsidian",
            DiagnosticEvent::AutocorrectApplied {
                typed: "teh".into(),
                applied: "the".into(),
                kind: MatchKind::KnownTypoDict,
                total_count: 1,
            },
        );
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        press(&mut app, '4');
        let find = |app: &App, pred: &dyn Fn(&SettingsRow) -> bool| {
            app.settings_rows.iter().position(pred).unwrap()
        };

        app.settings_idx = find(
            &app,
            &|r| matches!(r, SettingsRow::Setting(s) if s.key == "settings.aggressiveness"),
        );
        app.handle_key(key(KeyCode::Right));
        assert_eq!(
            store(&d).get_setting("settings.aggressiveness").as_deref(),
            Some("aggressive")
        );

        app.settings_idx = find(
            &app,
            &|r| matches!(r, SettingsRow::Setting(s) if s.key == "settings.undo_window_ms"),
        );
        let before: u64 = store(&d)
            .get_setting("settings.undo_window_ms")
            .unwrap()
            .parse()
            .unwrap();
        app.handle_key(key(KeyCode::Right));
        let after: u64 = store(&d)
            .get_setting("settings.undo_window_ms")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(after, before + 500);
        app.handle_key(key(KeyCode::Enter));
        type_text(&mut app, "3000");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(
            store(&d).get_setting("settings.undo_window_ms").as_deref(),
            Some("3000")
        );

        app.settings_idx = find(
            &app,
            &|r| matches!(r, SettingsRow::App { app, .. } if app == "obsidian"),
        );
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Enter));
        assert!(store(&d)
            .compiled()
            .app_profiles
            .bypass
            .contains("obsidian"));
        press(&mut app, 'u');
        assert!(!store(&d)
            .compiled()
            .app_profiles
            .bypass
            .contains("obsidian"));

        app.settings_idx = 0;
        app.handle_key(key(KeyCode::Up));
        assert!(!matches!(
            app.settings_rows[app.settings_idx],
            SettingsRow::Header(_)
        ));
    }

    #[test]
    fn sidebar_focus_switches_screens_and_letters_act_without_a_menu() {
        let d = dirs("sidebar");
        log_event(
            &d.logs,
            "kitty",
            DiagnosticEvent::AutocorrectApplied {
                typed: "coule".into(),
                applied: "could".into(),
                kind: MatchKind::KnownTypoDict,
                total_count: 1,
            },
        );
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        assert!(!app.focus_sidebar);

        press(&mut app, 'w');
        assert!(app.modal.is_none());
        assert!(store(&d).compiled().personal_dict.contains("coule"));

        app.handle_key(key(KeyCode::Esc));
        assert!(app.focus_sidebar);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.screen, Screen::Review);
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.screen, Screen::Settings);
        app.handle_key(key(KeyCode::Down));
        assert_eq!(app.screen, Screen::Activity);

        press(&mut app, 'e');
        assert!(
            store(&d).compiled().enabled,
            "main keys must be ignored while the sidebar has focus"
        );
        let side = screen_text(&app, 110, 32);
        assert!(side.contains("SmartKey"), "{side}");

        app.handle_key(key(KeyCode::Enter));
        assert!(!app.focus_sidebar);
        press(&mut app, '/');
        type_text(&mut app, "zzz");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Esc));
        assert!(
            !app.focus_sidebar,
            "esc clears a pending search before it leaves the window"
        );
        assert!(app.activity_search.is_empty());
        app.handle_key(key(KeyCode::Esc));
        assert!(app.focus_sidebar);
    }

    #[test]
    fn broken_rules_toml_shows_banner_and_blocks_changes() {
        let d = dirs("parse_lock");
        let corrupt = "enabled = [broken toml\n";
        fs::write(d.cfg.join("rules.toml"), corrupt).unwrap();
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        assert!(app.parse_locked);
        let screen = screen_text(&app, 110, 32);
        assert!(
            screen.contains("changes are blocked"),
            "banner missing:\n{screen}"
        );

        press(&mut app, 'e');
        assert!(app.toast.as_ref().is_some_and(|t| t.error));
        assert_eq!(
            fs::read_to_string(d.cfg.join("rules.toml")).unwrap(),
            corrupt
        );
    }

    #[test]
    fn cmd_args_help_and_unknown_flag() {
        assert!(cmd_tui(&["--help".to_string()]).is_ok());
        assert!(matches!(
            cmd_tui(&["--bogus".to_string()]),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            cmd_tui(&["--dir".to_string()]),
            Err(CliError::Usage(_))
        ));
    }
}
