use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;

use crate::format_age_ago;

use super::app::App;
use super::ops::activity_actions;
use super::theme::{empty_state, panel, selected_style, ACCENT, MUTED};
use super::types::{ActivityFilter, LayoutRects, Tone};

impl App {
    pub(super) fn render_activity(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        let focused = !self.focus_sidebar;
        let (list_area, detail_area) = self.split_list_detail(area);
        rects.list_box = list_area;

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
        rects.list_inner = inner;
        rects.list_has_header = true;
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
        let row_style = if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        };
        let table = Table::new(rows, widths)
            .header(header)
            .row_highlight_style(row_style);
        let mut state = TableState::default();
        state.select(Some(self.activity_idx));
        frame.render_stateful_widget(table, inner, &mut state);
        rects.list_offset = state.offset();
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
}
