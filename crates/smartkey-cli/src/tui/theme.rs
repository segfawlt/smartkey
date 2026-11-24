use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use smartkey_core::{AppProfile, LangId, MatchKind};

use crate::CliError;

use super::types::{Control, SettingSpec};

pub(super) const ACCENT: Color = Color::Cyan;
pub(super) const MUTED: Color = Color::Gray;
pub(super) const RULE: Color = Color::DarkGray;

pub(super) fn panel<'a>(title: Line<'a>, focused: bool) -> Block<'a> {
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

pub(super) fn selected_style() -> Style {
    Style::default()
        .fg(ACCENT)
        .add_modifier(Modifier::REVERSED | Modifier::BOLD)
}

pub(super) fn visual_style() -> Style {
    Style::default().bg(Color::DarkGray).fg(Color::White)
}

pub(super) fn warn_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!(" ⚠ {text}"),
        Style::default().fg(Color::Yellow),
    ))
}

pub(super) fn empty_state(text: &str) -> Paragraph<'static> {
    Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {text}"),
            Style::default().fg(MUTED),
        )),
    ])
    .wrap(Wrap { trim: false })
}

pub(super) fn input_line(input: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled("> ", Style::default().fg(ACCENT)),
        Span::raw(input.to_string()),
        Span::styled("▏", Style::default().fg(ACCENT)),
    ])
}

pub(super) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

pub(super) fn step(index: &mut usize, delta: isize, len: usize) {
    if len == 0 {
        *index = 0;
        return;
    }
    let cur = (*index).min(len - 1) as isize;
    *index = (cur + delta).clamp(0, len as isize - 1) as usize;
}

pub(super) fn clamp(index: &mut usize, len: usize) {
    if len == 0 {
        *index = 0;
    } else if *index >= len {
        *index = len - 1;
    }
}

pub(super) fn point_in(col: u16, row: u16, rect: Rect) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

pub(super) fn near_x(col: u16, target: u16) -> bool {
    col.abs_diff(target) <= 1
}

pub(super) fn near_y(row: u16, target: u16) -> bool {
    row.abs_diff(target) <= 1
}

pub(super) fn cli_error_text(err: &CliError) -> &str {
    match err {
        CliError::Usage(m) | CliError::Runtime(m) => m,
    }
}

pub(super) fn split_lang_prefix(input: &str) -> (LangId, &str) {
    if let Some((prefix, rest)) = input.split_once(':') {
        let p = prefix.trim();
        if (2..=3).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphabetic()) {
            return (LangId(p.to_ascii_lowercase()), rest);
        }
    }
    (LangId::en(), input)
}

pub(super) fn format_number(v: f64, decimals: usize) -> String {
    if decimals == 0 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.decimals$}")
    }
}

pub(super) fn unit_hint(spec: &SettingSpec) -> &'static str {
    match spec.control {
        Control::Number { unit: "ms", .. } => " in milliseconds",
        Control::Number { unit: "days", .. } => " in days",
        _ => "",
    }
}

pub(super) fn display_setting(spec: &SettingSpec, value: &str) -> String {
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

pub(super) fn setting_value_spans(
    spec: &SettingSpec,
    value: &str,
    show_all: bool,
) -> Vec<Span<'static>> {
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

pub(super) fn profile_label(p: AppProfile) -> &'static str {
    match p {
        AppProfile::Bypass => "off",
        AppProfile::Browser => "browser",
        AppProfile::Chat => "chat",
        AppProfile::Prose => "prose",
    }
}

pub(super) fn language_name(code: &str) -> String {
    match code {
        "en" => "English (en)".to_string(),
        "vi" => "Vietnamese (vi)".to_string(),
        "es" => "Spanish (es)".to_string(),
        "fr" => "French (fr)".to_string(),
        other => other.to_string(),
    }
}

pub(super) fn reason_label(reason: &str) -> &'static str {
    match reason {
        "program-list" => "built-in (terminal/editor)",
        "browser" | "chat" => "built-in",
        "prose" => "default",
        "user-app-override" => "your choice",
        "unknown-app-policy" => "unlisted-app rule",
        _ => "built-in",
    }
}

pub(super) fn kind_label(kind: MatchKind) -> &'static str {
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

pub(super) fn manual_outcome_label(outcome: &str) -> String {
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
