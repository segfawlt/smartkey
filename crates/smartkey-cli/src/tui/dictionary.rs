use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Row, Table, TableState};
use ratatui::Frame;

use super::app::App;
use super::theme::{empty_state, panel, selected_style, visual_style, ACCENT, MUTED, RULE};
use super::types::{range_indices, DictSection, LayoutRects};

impl App {
    pub(super) fn render_dictionary(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        let focused = !self.focus_sidebar;
        rects.list_box = area;
        rects.dict_title = Rect::new(area.x, area.y, area.width, 1);

        let visual_set = range_indices(self.visual_anchor, self.dict_idx);
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
        if !visual_set.is_empty() {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" VISUAL · {} selected ", visual_set.len()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        } else if self.filter_editing || !self.dict_filter.is_empty() {
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
        rects.list_inner = inner;
        rects.list_has_header = true;
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
            .enumerate()
            .map(|(vi, &i)| {
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
                let mut row = Row::new(cells);
                if visual_set.contains(&vi) && vi != self.dict_idx {
                    row = row.style(visual_style());
                }
                row
            })
            .collect();
        let row_style = if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        };
        let table = Table::new(rows, self.dict_section.widths())
            .header(
                Row::new(self.dict_section.headers().to_vec()).style(Style::default().fg(MUTED)),
            )
            .row_highlight_style(row_style);
        let mut state = TableState::default();
        state.select(Some(self.dict_idx));
        frame.render_stateful_widget(table, inner, &mut state);
        rects.list_offset = state.offset();
    }
}
