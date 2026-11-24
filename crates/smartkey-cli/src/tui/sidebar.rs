use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;

use crate::format_age_ago;

use super::app::App;
use super::theme::{panel, selected_style, truncate, warn_line, ACCENT, MUTED};
use super::types::{Modal, ReviewTab, Screen};

impl App {
    pub(super) fn banner_lines(&self) -> Vec<Line<'static>> {
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

    /// One-row screen switcher used when zoomed or too narrow for the sidebar.
    pub(super) fn render_header(&self, frame: &mut Frame, area: Rect) {
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
        if self.layout.zoomed {
            spans.push(Span::styled(" [zoom]", Style::default().fg(ACCENT)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// Renders the left navigation box + rich Status box, returning nav's inner `Rect`.
    pub(super) fn render_sidebar(&self, frame: &mut Frame, area: Rect) -> Rect {
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
                        format!(" {}", self.review.len()),
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
        let nav_block = panel(
            Line::from(vec![
                Span::raw(" SmartKey "),
                if self.enabled {
                    Span::styled("●", Style::default().fg(Color::Green))
                } else {
                    Span::styled("○", Style::default().fg(Color::Red))
                },
                Span::raw(" "),
            ]),
            self.focus_sidebar,
        );
        let nav_inner = nav_block.inner(rows[0]);
        let nav = List::new(items).highlight_style(nav_style).block(nav_block);
        let mut state = ListState::default();
        state.select(Some(self.screen.index()));
        frame.render_stateful_widget(nav, rows[0], &mut state);

        if rows[1].height >= 3 {
            let block = panel(Line::from(" Status "), false);
            let inner_w = block.inner(rows[1]).width as usize;
            let muted = Style::default().fg(MUTED);
            let kv = |k: &str, v: String, color: Option<Color>| {
                let val_w = inner_w.saturating_sub(10).max(4);
                let val_style = match color {
                    Some(c) => Style::default().fg(c),
                    None => Style::default(),
                };
                Line::from(vec![
                    Span::styled(format!(" {k:<8} "), muted),
                    Span::styled(truncate(&v, val_w), val_style),
                ])
            };

            let updated = {
                let now = current_epoch_ms();
                let secs = now.saturating_sub(self.last_refresh_ms) / 1000;
                if secs < 2 {
                    "just now".to_string()
                } else {
                    format!("{} ago", format_age_ago(now, self.last_refresh_ms))
                }
            };
            let lotus = if !self.vi_lotus {
                ("vi off".to_string(), Some(MUTED))
            } else if self.lotus_installed {
                ("vi on".to_string(), None)
            } else {
                ("vi missing".to_string(), Some(Color::Yellow))
            };
            let last_app = self.last_app.as_deref().unwrap_or("none yet").to_string();
            let total_outcomes = self.day_fixes + self.day_undone;
            let ok_pct = (self.day_fixes * 100)
                .checked_div(total_outcomes)
                .unwrap_or(100);
            let rate_color = if total_outcomes > 0 && ok_pct < 90 {
                Color::Yellow
            } else {
                Color::Green
            };

            let mut lines = vec![
                kv(
                    "Fcitx5",
                    if self.fcitx5_running {
                        "running".to_string()
                    } else {
                        "stopped".to_string()
                    },
                    Some(if self.fcitx5_running {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
                kv("Lotus", lotus.0, lotus.1),
                kv("Updated", updated, Some(MUTED)),
                Line::from(""),
                kv("Mode", self.mode_summary.clone(), None),
                kv("Undo", self.undo_summary.clone(), Some(MUTED)),
                kv("Last app", last_app, Some(MUTED)),
                Line::from(""),
                Line::from(Span::styled(" Last 24 hours", muted)),
                Line::from(vec![
                    Span::styled(
                        format!(" {} fixes", self.day_fixes),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    if total_outcomes > 0 {
                        Span::styled(format!(" · {ok_pct}% ok"), Style::default().fg(rate_color))
                    } else {
                        Span::raw("")
                    },
                ]),
                Line::from(Span::styled(
                    format!(" {} undone · {} you", self.day_undone, self.day_by_hand),
                    muted,
                )),
                Line::from(""),
                kv(
                    "Brain",
                    format!(
                        "{}w {}f {}b",
                        self.brain_words, self.brain_fixes, self.brain_blocks
                    ),
                    Some(MUTED),
                ),
                kv(
                    "Watching",
                    format!("{} staged", self.review_hidden),
                    Some(MUTED),
                ),
            ];
            let active_changes = self.active_change_count();
            if active_changes > 0 {
                lines.push(kv(
                    "Changes",
                    format!("{active_changes} saved"),
                    Some(ACCENT),
                ));
            }
            frame.render_widget(Paragraph::new(lines).block(block), rows[1]);
        }
        nav_inner
    }

    pub(super) fn render_toast(&self, frame: &mut Frame, area: Rect) {
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

    pub(super) fn hint_pairs(&self) -> Vec<(&'static str, &'static str)> {
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
                ("<>", "resize"),
                ("?", "help"),
                ("q", "quit"),
            ];
        }
        if self.visual_anchor.is_some() {
            return match self.screen {
                Screen::Review => vec![
                    ("j k", "extend"),
                    ("a", "keep"),
                    ("d", "remove"),
                    ("space", "mark"),
                    ("esc", "cancel"),
                ],
                _ => vec![("j k", "extend"), ("d", "delete"), ("esc", "cancel")],
            };
        }
        let mut pairs = match self.screen {
            Screen::Activity => vec![
                ("j k", "move"),
                ("⏎", "actions"),
                ("/", "search"),
                ("f", "filter"),
                ("z", "zoom"),
            ],
            Screen::Review => match self.review_tab {
                ReviewTab::Queue => vec![
                    ("h l", "tab"),
                    ("v", "visual"),
                    ("a", "keep"),
                    ("d", "remove"),
                    ("space", "mark"),
                ],
                ReviewTab::Changes => vec![("h l", "tab"), ("j k", "move"), ("u / ⏎", "revert")],
            },
            Screen::Dictionary => vec![
                ("←→", "section"),
                ("v", "visual"),
                ("n", "new"),
                ("d", "delete"),
                ("/", "filter"),
            ],
            Screen::Settings => vec![("j k", "move"), ("←→", "change"), ("{}", "section")],
        };
        if !self.undo_stack.is_empty()
            && !(self.screen == Screen::Review && self.review_tab == ReviewTab::Changes)
        {
            pairs.push(("u", "undo"));
        }
        if !self.redo_stack.is_empty() {
            pairs.push(("C-r", "redo"));
        }
        pairs.extend([
            ("<>", "resize"),
            ("esc", "sidebar"),
            ("?", "help"),
            ("q", "quit"),
        ]);
        pairs
    }

    pub(super) fn render_hints(&self, frame: &mut Frame, area: Rect) {
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
}
