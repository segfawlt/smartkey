use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState, Wrap};
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;

use crate::format_age_ago;

use super::app::App;
use super::ops::activity_actions;
use super::theme::{empty_state, panel, selected_style, ACCENT, MUTED};
use super::types::{
    ActivityFilter, ActivityLayout, ActivityPane, ActivityRow, LayoutRects, Tone, GRID_PCT_MAX,
    GRID_PCT_MIN, SPLIT_GRID_MIN_HEIGHT, SPLIT_GRID_MIN_WIDTH,
};

fn compact_why(r: &ActivityRow) -> String {
    match r.why.as_str() {
        "SmartKey learned this" => "learned".to_string(),
        "you fixed it" => "you fixed".to_string(),
        "you undid it" => "undone".to_string(),
        s if s.starts_with("left alone (") && s.ends_with(')') => {
            let inner = &s["left alone (".len()..s.len() - 1];
            if let Some(cand) = inner.strip_suffix(" too risky") {
                format!("risky: {cand}")
            } else {
                inner.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn build_activity_rows(
    activity: &[ActivityRow],
    indices: &[usize],
    full_cols: bool,
    compact_text: bool,
    now: u64,
) -> Vec<Row<'static>> {
    indices
        .iter()
        .map(|&i| {
            let r = &activity[i];
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
            let mut why = if compact_text {
                compact_why(r)
            } else {
                r.why.clone()
            };
            if r.repeat > 1 {
                why.push_str(&format!("  ×{}", r.repeat));
            }
            let mut cells = vec![
                Cell::from(Span::styled(age, Style::default().fg(MUTED))),
                Cell::from(Span::styled(r.change.clone(), main_style)),
                Cell::from(Span::styled(why, Style::default().fg(MUTED))),
            ];
            if full_cols {
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
        .collect()
}

impl App {
    pub(super) fn render_activity(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        if self.activity_layout == ActivityLayout::Split {
            self.render_activity_split(frame, area, rects);
            return;
        }
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
        let rows = build_activity_rows(&self.activity, &self.activity_view, !narrow, false, now);
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

    fn render_activity_split(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        let now = current_epoch_ms();
        let narrow_single =
            area.width < SPLIT_GRID_MIN_WIDTH || area.height < SPLIT_GRID_MIN_HEIGHT;
        if self.layout.zoomed || narrow_single {
            let tag = if self.layout.zoomed {
                Some("[zoom]".to_string())
            } else {
                Some(format!("[{}/4 · h/l]", self.activity_pane.index() + 1))
            };
            self.render_activity_quadrant(
                frame,
                area,
                self.activity_pane,
                tag.as_deref(),
                rects,
                now,
            );
            return;
        }

        let h_pct = self
            .layout
            .activity_grid_h_pct
            .clamp(GRID_PCT_MIN, GRID_PCT_MAX);
        let v_pct = self
            .layout
            .activity_grid_v_pct
            .clamp(GRID_PCT_MIN, GRID_PCT_MAX);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(v_pct),
                Constraint::Percentage(100 - v_pct),
            ])
            .split(area);
        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(h_pct),
                Constraint::Percentage(100 - h_pct),
            ])
            .split(rows[0]);
        let bot = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(h_pct),
                Constraint::Percentage(100 - h_pct),
            ])
            .split(rows[1]);

        let cells = [
            (ActivityPane::AutoFixes, top[0]),
            (ActivityPane::Telex, top[1]),
            (ActivityPane::ByHand, bot[0]),
            (ActivityPane::Learning, bot[1]),
        ];
        for (pane, rect) in cells {
            self.render_activity_quadrant(frame, rect, pane, None, rects, now);
        }
    }

    fn render_activity_quadrant(
        &self,
        frame: &mut Frame,
        area: Rect,
        pane: ActivityPane,
        extra_tag: Option<&str>,
        rects: &mut LayoutRects,
        now: u64,
    ) {
        let p = pane.index();
        let active = !self.focus_sidebar && self.activity_pane == pane;
        let indices = &self.split_views[p];
        let mut title = format!(" {} ({})", pane.title(), indices.len());
        if self.activity_pane == pane && (self.filter_editing || !self.activity_search.is_empty()) {
            title.push_str(&format!(
                " · / {}{}",
                self.activity_search,
                if self.filter_editing { "▏" } else { "" }
            ));
        }
        if let Some(tag) = extra_tag {
            title.push_str(&format!(" · {tag}"));
        }
        title.push(' ');

        let block = panel(Line::from(title), active);
        let inner = block.inner(area);
        rects.split_boxes[p] = area;
        rects.split_inners[p] = inner;
        let full_cols = inner.width >= 68;
        rects.split_has_header[p] = full_cols;
        frame.render_widget(block, area);

        if indices.is_empty() {
            let msg = if !self.activity_search.is_empty() {
                "Nothing matches / filter."
            } else {
                pane.empty_msg()
            };
            frame.render_widget(empty_state(msg), inner);
            return;
        }

        let rows = build_activity_rows(&self.activity, indices, full_cols, !full_cols, now);
        let widths: Vec<Constraint> = if full_cols {
            vec![
                Constraint::Length(4),
                Constraint::Fill(4),
                Constraint::Fill(3),
                Constraint::Fill(2),
                Constraint::Fill(2),
            ]
        } else {
            vec![
                Constraint::Length(4),
                Constraint::Fill(3),
                Constraint::Fill(2),
            ]
        };
        let row_style = if active {
            selected_style()
        } else {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        };
        let mut table = Table::new(rows, widths).row_highlight_style(row_style);
        if full_cols {
            table = table.header(
                Row::new(vec!["When", "Change", "Why", "App", ""])
                    .style(Style::default().fg(MUTED)),
            );
        }
        let mut state = TableState::default();
        state.select(Some(self.split_idx[p]));
        frame.render_stateful_widget(table, inner, &mut state);
        rects.split_offsets[p] = state.offset();
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
