use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use smartkey_core::AppProfile;

use super::app::App;
use super::theme::{
    panel, profile_label, reason_label, selected_style, setting_value_spans, truncate, ACCENT,
    MUTED,
};
use super::types::{LayoutRects, SettingsRow, DETAIL_MIN_WIDTH, SPLIT_PCT_MAX, SPLIT_PCT_MIN};

impl App {
    pub(super) fn render_settings(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        let focused = !self.focus_sidebar;
        let (list_area, help_area) = if !self.layout.zoomed && area.width >= DETAIL_MIN_WIDTH {
            let left = self
                .layout
                .split_pct
                .max(60)
                .clamp(SPLIT_PCT_MIN, SPLIT_PCT_MAX);
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Percentage(left),
                    Constraint::Percentage(100 - left),
                ])
                .split(area);
            (cols[0], Some(cols[1]))
        } else if !self.layout.zoomed {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(5)])
                .split(area);
            (rows[0], Some(rows[1]))
        } else {
            (area, None)
        };
        rects.list_box = list_area;

        let block = panel(Line::from(" Settings "), focused);
        let inner = block.inner(list_area);
        rects.list_inner = inner;
        rects.list_has_header = false;
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
        let row_style = if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        };
        let list = List::new(items).highlight_style(row_style);
        let mut state = ListState::default();
        state.select(Some(self.settings_idx));
        frame.render_stateful_widget(list, inner, &mut state);
        rects.list_offset = state.offset();

        if let Some(help_area) = help_area {
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
    }
}
