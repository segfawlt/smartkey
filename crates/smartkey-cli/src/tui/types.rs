use std::collections::BTreeSet;

use ratatui::layout::{Constraint, Rect};
use smartkey_core::{
    AppProfile, ConfidenceTier, DiagnosticEvent, LangId, MatchKind, RuleOrigin, RuleStat,
};

use crate::LogFilter;

pub(super) const ACTIVITY_WINDOW_MS: u64 = 3 * 86_400_000;
pub(super) const ACTIVITY_MAX_ROWS: usize = 400;
pub(super) const MAX_SEEN_APPS: usize = 15;
pub(super) const MAX_ALMOST: usize = 20;
pub(super) const MAX_HISTORY: usize = 200;

pub(super) const SIDEBAR_WIDTH_DEFAULT: u16 = 26;
pub(super) const SIDEBAR_WIDTH_MIN: u16 = 16;
pub(super) const SIDEBAR_WIDTH_MAX: u16 = 34;
pub(super) const SIDEBAR_MIN_TOTAL: u16 = 70;
pub(super) const DETAIL_MIN_WIDTH: u16 = 84;
pub(super) const SPLIT_PCT_DEFAULT: u16 = 58;
pub(super) const SPLIT_PCT_MIN: u16 = 35;
pub(super) const SPLIT_PCT_MAX: u16 = 80;
pub(super) const INSPECT_PCT_DEFAULT: u16 = 62;
pub(super) const INSPECT_PCT_MIN: u16 = 35;
pub(super) const INSPECT_PCT_MAX: u16 = 80;
pub(super) const GRID_PCT_DEFAULT: u16 = 50;
pub(super) const GRID_PCT_MIN: u16 = 25;
pub(super) const GRID_PCT_MAX: u16 = 75;
pub(super) const SPLIT_GRID_MIN_WIDTH: u16 = 64;
pub(super) const SPLIT_GRID_MIN_HEIGHT: u16 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Screen {
    Activity,
    Review,
    Dictionary,
    Settings,
}

impl Screen {
    pub(super) const ALL: [Screen; 4] = [
        Screen::Activity,
        Screen::Review,
        Screen::Dictionary,
        Screen::Settings,
    ];

    pub(super) fn index(self) -> usize {
        match self {
            Self::Activity => 0,
            Self::Review => 1,
            Self::Dictionary => 2,
            Self::Settings => 3,
        }
    }

    pub(super) fn from_index(i: usize) -> Self {
        Self::ALL[i % 4]
    }

    pub(super) fn label(self, short: bool) -> &'static str {
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
pub(super) enum ActivityFilter {
    All,
    Fixes,
    ByHand,
    Missed,
    Apps,
}

impl ActivityFilter {
    pub(super) fn next(self) -> Self {
        match self {
            Self::All => Self::Fixes,
            Self::Fixes => Self::ByHand,
            Self::ByHand => Self::Missed,
            Self::Missed => Self::Apps,
            Self::Apps => Self::All,
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::All => "everything",
            Self::Fixes => "fixes",
            Self::ByHand => "undos and hand fixes",
            Self::Missed => "missed words",
            Self::Apps => "app focus",
        }
    }

    pub(super) fn matches(self, ev: &DiagnosticEvent) -> bool {
        match self {
            Self::All => !matches!(ev, DiagnosticEvent::AppBypass { .. }),
            Self::Fixes => matches!(ev, DiagnosticEvent::AutocorrectApplied { .. }),
            Self::ByHand => matches!(
                ev,
                DiagnosticEvent::BackspaceUndo { .. } | DiagnosticEvent::ManualBackspaceFix { .. }
            ),
            Self::Missed => matches!(
                ev,
                DiagnosticEvent::UnfixedUnknown { .. } | DiagnosticEvent::RealWordDeclined { .. }
            ),
            Self::Apps => matches!(ev, DiagnosticEvent::AppBypass { .. }),
        }
    }
}

impl From<ActivityFilter> for LogFilter {
    fn from(value: ActivityFilter) -> Self {
        match value {
            ActivityFilter::All | ActivityFilter::Apps => LogFilter::All,
            ActivityFilter::Fixes => LogFilter::FixedOnly,
            ActivityFilter::ByHand => LogFilter::ManualOnly,
            ActivityFilter::Missed => LogFilter::UnfixedOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ActivityLayout {
    #[default]
    List,
    Split,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum ActivityPane {
    #[default]
    AutoFixes = 0,
    Telex = 1,
    ByHand = 2,
    Learning = 3,
}

impl ActivityPane {
    pub(super) const ALL: [ActivityPane; 4] = [
        ActivityPane::AutoFixes,
        ActivityPane::Telex,
        ActivityPane::ByHand,
        ActivityPane::Learning,
    ];

    pub(super) fn index(self) -> usize {
        self as usize
    }

    pub(super) fn from_index(i: usize) -> Self {
        Self::ALL[i % 4]
    }

    pub(super) fn toggle_lr(self) -> Self {
        match self {
            Self::AutoFixes => Self::Telex,
            Self::Telex => Self::AutoFixes,
            Self::ByHand => Self::Learning,
            Self::Learning => Self::ByHand,
        }
    }

    pub(super) fn toggle_ud(self) -> Self {
        match self {
            Self::AutoFixes => Self::ByHand,
            Self::ByHand => Self::AutoFixes,
            Self::Telex => Self::Learning,
            Self::Learning => Self::Telex,
        }
    }

    pub(super) fn title(self) -> &'static str {
        match self {
            Self::AutoFixes => "Auto-fixes",
            Self::Telex => "Telex / VI",
            Self::ByHand => "Edits & undos",
            Self::Learning => "Learning & missed",
        }
    }

    pub(super) fn empty_msg(self) -> &'static str {
        match self {
            Self::AutoFixes => "No automatic English fixes yet.",
            Self::Telex => "No Vietnamese or Telex activity yet.",
            Self::ByHand => "No hand edits or undos yet.",
            Self::Learning => "No learning notices or missed words yet.",
        }
    }

    pub(super) fn classify(row: &ActivityRow) -> Option<Self> {
        match &row.event {
            DiagnosticEvent::AppBypass { .. } => None,
            DiagnosticEvent::ManualBackspaceFix { .. } | DiagnosticEvent::BackspaceUndo { .. } => {
                Some(Self::ByHand)
            }
            DiagnosticEvent::AutocorrectApplied { kind, .. } => {
                if row.lang == "vi" || *kind == MatchKind::VietnameseTelexRescue {
                    Some(Self::Telex)
                } else {
                    Some(Self::AutoFixes)
                }
            }
            DiagnosticEvent::RuleLearned { rule, stage, .. } => {
                if stage.contains("telex") || rule.starts_with("vi:") {
                    Some(Self::Telex)
                } else {
                    Some(Self::Learning)
                }
            }
            DiagnosticEvent::UnfixedUnknown { .. }
            | DiagnosticEvent::RealWordDeclined { .. }
            | DiagnosticEvent::WordUnchanged { .. }
            | DiagnosticEvent::PackNotice { .. } => Some(Self::Learning),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewTab {
    Queue,
    Changes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DictSection {
    Words,
    Fixes,
    Snippets,
    NeverFix,
    TelexKeep,
}

impl DictSection {
    pub(super) const ALL: [DictSection; 5] = [
        DictSection::Words,
        DictSection::Fixes,
        DictSection::Snippets,
        DictSection::NeverFix,
        DictSection::TelexKeep,
    ];

    pub(super) fn index(self) -> usize {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Words => "Words",
            Self::Fixes => "Fixes",
            Self::Snippets => "Snippets",
            Self::NeverFix => "Never fix",
            Self::TelexKeep => "Telex keep",
        }
    }

    pub(super) fn headers(self) -> &'static [&'static str] {
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

    pub(super) fn widths(self) -> Vec<Constraint> {
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

    pub(super) fn empty_text(self) -> &'static str {
        match self {
            Self::Words => "No words yet. Press n to add a word SmartKey should never correct.",
            Self::Fixes => "No fixes yet. Press n to teach SmartKey a correction.",
            Self::Snippets => "No snippets yet. Press n to add a trigger that expands into text.",
            Self::NeverFix => "Nothing blocked. Fixes you undo twice show up here.",
            Self::TelexKeep => "No Telex exceptions. These keep English words like \"aur\" as typed in Vietnamese mode.",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum StoreOp {
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
    KeepRule {
        stat_key: Option<String>,
        inner: Box<StoreOp>,
    },
    DeleteRule {
        stat_key: Option<String>,
        inner: Box<StoreOp>,
    },
    RestoreRuleStat {
        stat_key: String,
        stat: Option<RuleStat>,
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
    RestoreStaged {
        queue: String,
        key: String,
        count: u32,
        canonical: Option<String>,
    },
    Batch(Vec<StoreOp>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChangeKind {
    Added,
    Kept,
    Removed,
    Blocked,
    Changed,
}

impl ChangeKind {
    pub(super) fn badge(self) -> &'static str {
        match self {
            Self::Added => "+ Added  ",
            Self::Kept => "✓ Kept   ",
            Self::Removed => "× Removed",
            Self::Blocked => "⊘ Blocked",
            Self::Changed => "~ Changed",
        }
    }

    pub(super) fn verb(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Kept => "Kept",
            Self::Removed => "Removed",
            Self::Blocked => "Blocked",
            Self::Changed => "Changed",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ChangeEntry {
    pub(super) id: u64,
    pub(super) kind: ChangeKind,
    pub(super) summary: String,
    pub(super) screen: Screen,
    pub(super) ts_ms: u64,
    pub(super) forward_ops: Vec<StoreOp>,
    pub(super) inverse_ops: Vec<StoreOp>,
    pub(super) reverted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Normal,
    Warn,
    Muted,
}

#[derive(Debug, Clone)]
pub(super) struct ActivityRow {
    pub(super) ts_ms: u64,
    pub(super) app: Option<String>,
    pub(super) lang: String,
    pub(super) change: String,
    pub(super) why: String,
    pub(super) note: String,
    pub(super) tone: Tone,
    pub(super) repeat: u32,
    pub(super) event: DiagnosticEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuleKind {
    Fix,
    Context,
    Snippet,
    Word,
    Block,
    Passthrough,
}

#[derive(Debug, Clone)]
pub(super) struct RuleEntry {
    pub(super) kind: RuleKind,
    pub(super) lang: LangId,
    pub(super) a: String,
    pub(super) b: String,
    pub(super) prev: String,
    pub(super) tier: ConfidenceTier,
    pub(super) origin: Option<RuleOrigin>,
    pub(super) pinned: bool,
    pub(super) hits: u32,
    pub(super) strikes: u32,
    pub(super) created_ms: u64,
    pub(super) last_hit_ms: Option<u64>,
    pub(super) stat_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviewSection {
    Learned,
    Blocked,
    Almost,
}

impl ReviewSection {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Learned => "Learned rules to check",
            Self::Blocked => "Fixes blocked after you undid them",
            Self::Almost => "Almost learned (most repeated first)",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ReviewItem {
    pub(super) id: String,
    pub(super) section: ReviewSection,
    pub(super) type_badge: String,
    pub(super) kind_noun: &'static str,
    pub(super) title: String,
    pub(super) item_label: String,
    pub(super) lang_label: String,
    pub(super) problem: String,
    pub(super) source: String,
    pub(super) detail: String,
    pub(super) warning: Option<&'static str>,
    pub(super) added_ms: u64,
    pub(super) last_hit_ms: Option<u64>,
    pub(super) hits: u32,
    pub(super) strikes: u32,
    pub(super) seen_apps: Vec<String>,
    pub(super) keep_effect: String,
    pub(super) remove_effect: String,
    pub(super) keep_op: StoreOp,
    pub(super) remove_op: StoreOp,
}

#[derive(Debug, Clone)]
pub(super) struct DictRow {
    pub(super) section: DictSection,
    pub(super) cols: Vec<String>,
    pub(super) haystack: String,
    pub(super) label: String,
    pub(super) delete_op: StoreOp,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum Control {
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
pub(super) struct SettingSpec {
    pub(super) key: &'static str,
    pub(super) label: &'static str,
    pub(super) help: &'static str,
    pub(super) control: Control,
}

pub(super) const STRENGTH: &[(&str, &str)] = &[
    ("conservative", "careful"),
    ("balanced", "balanced"),
    ("aggressive", "eager"),
];
pub(super) const SPELLING: &[(&str, &str)] =
    &[("mixed", "US + UK"), ("en_US", "US"), ("en_GB", "UK")];
pub(super) const UNDO_MODE: &[(&str, &str)] = &[
    ("step", "stepwise"),
    ("immediate", "instant"),
    ("off", "off"),
];
pub(super) const LOG_DETAIL: &[(&str, &str)] =
    &[("interesting", "important only"), ("all", "every word")];
pub(super) const UNLISTED: &[(&str, &str)] = &[
    ("default", "built-in"),
    ("prose", "prose"),
    ("bypass", "off"),
    ("browser", "browser"),
    ("chat", "chat"),
];

pub(super) const CORRECTION_SPECS: &[SettingSpec] = &[
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

pub(super) const UNDO_SPECS: &[SettingSpec] = &[
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

pub(super) const VIETNAMESE_SPECS: &[SettingSpec] = &[SettingSpec {
    key: "plugins.vi_lotus",
    label: "Work with fcitx5-lotus",
    help: "Lets SmartKey run alongside the fcitx5-lotus Vietnamese input method and repair Telex typos.",
    control: Control::Toggle,
}];

pub(super) const UNLISTED_SPEC: SettingSpec = SettingSpec {
    key: "app_profiles.unknown_app_policy",
    label: "Unlisted apps",
    help: "How to treat apps that are not in the built-in lists or your choices above. Built-in treats them as prose.",
    control: Control::Choice(UNLISTED),
};

pub(super) const DIAGNOSTIC_SPECS: &[SettingSpec] = &[
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
pub(super) enum SettingsRow {
    Header(&'static str),
    Setting(SettingSpec),
    App {
        app: String,
        profile: AppProfile,
        reason: &'static str,
        chosen: Option<AppProfile>,
    },
}

#[derive(Debug, Clone)]
pub(super) enum RowAction {
    Ops {
        kind: ChangeKind,
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
pub(super) struct ActionItem {
    pub(super) key: char,
    pub(super) label: String,
    pub(super) hint: String,
    pub(super) action: RowAction,
}

#[derive(Debug, Clone)]
pub(super) struct FormField {
    pub(super) label: &'static str,
    pub(super) value: String,
    pub(super) required: bool,
}

#[derive(Debug, Clone)]
pub(super) struct Form {
    pub(super) section: DictSection,
    pub(super) fields: Vec<FormField>,
    pub(super) focus: usize,
}

impl Form {
    pub(super) fn new(section: DictSection) -> Self {
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

    pub(super) fn value(&self, i: usize) -> &str {
        self.fields[i].value.trim()
    }

    pub(super) fn first_missing(&self) -> Option<usize> {
        self.fields
            .iter()
            .position(|f| f.required && f.value.trim().is_empty())
    }

    pub(super) fn to_ops(&self) -> (ChangeKind, Vec<StoreOp>, String) {
        match self.section {
            DictSection::Words => (
                ChangeKind::Added,
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
                        ChangeKind::Added,
                        vec![StoreOp::SetFix {
                            lang: LangId(self.value(3).to_lowercase()),
                            typed: typed.to_string(),
                            fixed: fixed.to_string(),
                        }],
                        format!("Saved fix {typed} → {fixed}"),
                    )
                } else {
                    (
                        ChangeKind::Added,
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
                ChangeKind::Added,
                vec![StoreOp::SetSnippet {
                    trigger: self.value(0).to_string(),
                    expansion: self.value(1).to_string(),
                }],
                format!("Saved snippet {}", self.value(0)),
            ),
            DictSection::NeverFix => (
                ChangeKind::Blocked,
                vec![StoreOp::AddBlock {
                    typed: self.value(0).to_string(),
                    rejected: self.value(1).to_string(),
                }],
                format!("Blocked {} → {}", self.value(0), self.value(1)),
            ),
            DictSection::TelexKeep => (
                ChangeKind::Added,
                vec![StoreOp::AddPassthrough {
                    raw: self.value(0).to_string(),
                }],
                format!("Telex will keep \"{}\" as typed", self.value(0)),
            ),
        }
    }
}

pub(super) const APP_CHOICES: [(Option<AppProfile>, &str, &str); 5] = [
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
pub(super) enum Modal {
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
        kind: ChangeKind,
        ops: Vec<StoreOp>,
        msg: String,
        clear_marks: bool,
    },
}

#[derive(Debug, Clone)]
pub(super) struct Toast {
    pub(super) text: String,
    pub(super) error: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LayoutState {
    pub(super) sidebar_width: u16,
    pub(super) split_pct: u16,
    pub(super) review_inspect_pct: u16,
    pub(super) activity_grid_h_pct: u16,
    pub(super) activity_grid_v_pct: u16,
    pub(super) zoomed: bool,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            split_pct: SPLIT_PCT_DEFAULT,
            review_inspect_pct: INSPECT_PCT_DEFAULT,
            activity_grid_h_pct: GRID_PCT_DEFAULT,
            activity_grid_v_pct: GRID_PCT_DEFAULT,
            zoomed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DragTarget {
    Sidebar,
    Split,
    ReviewVertical,
    ActivityGridH,
    ActivityGridV,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutRects {
    pub(super) body: Rect,
    pub(super) sidebar: Rect,
    pub(super) nav_inner: Rect,
    pub(super) main: Rect,
    pub(super) list_box: Rect,
    pub(super) list_inner: Rect,
    pub(super) list_has_header: bool,
    pub(super) list_offset: usize,
    pub(super) split_boxes: [Rect; 4],
    pub(super) split_inners: [Rect; 4],
    pub(super) split_offsets: [usize; 4],
    pub(super) split_has_header: [bool; 4],
    pub(super) review_inspect: Rect,
    pub(super) header_strip: Rect,
    pub(super) review_title: Rect,
    pub(super) dict_title: Rect,
    pub(super) review_line_map: [Option<usize>; 128],
}

impl Default for LayoutRects {
    fn default() -> Self {
        Self {
            body: Rect::default(),
            sidebar: Rect::default(),
            nav_inner: Rect::default(),
            main: Rect::default(),
            list_box: Rect::default(),
            list_inner: Rect::default(),
            list_has_header: false,
            list_offset: 0,
            split_boxes: [Rect::default(); 4],
            split_inners: [Rect::default(); 4],
            split_offsets: [0; 4],
            split_has_header: [false; 4],
            review_inspect: Rect::default(),
            header_strip: Rect::default(),
            review_title: Rect::default(),
            dict_title: Rect::default(),
            review_line_map: [None; 128],
        }
    }
}

pub(super) fn range_indices(anchor: Option<usize>, cur: usize) -> BTreeSet<usize> {
    match anchor {
        None => BTreeSet::new(),
        Some(a) => (a.min(cur)..=a.max(cur)).collect(),
    }
}
