use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use smartkey_core::event_log::current_epoch_ms;

use crate::format_age_ago;

use super::app::App;
use super::theme::{
    empty_state, panel, selected_style, truncate, visual_style, ACCENT, MUTED, RULE,
};
use super::types::{
    range_indices, LayoutRects, ReviewSection, ReviewTab, INSPECT_PCT_MAX, INSPECT_PCT_MIN,
};

impl App {
    pub(super) fn render_review(&self, frame: &mut Frame, area: Rect, rects: &mut LayoutRects) {
        let focused = !self.focus_sidebar;
        let (list_area, detail_area) = self.split_list_detail(area);
        rects.list_box = list_area;
        rects.review_title = Rect::new(list_area.x, list_area.y, list_area.width.min(54), 1);

        let visual_set = if self.review_tab == ReviewTab::Queue {
            range_indices(self.visual_anchor, self.review_idx)
        } else {
            Default::default()
        };

        let mut title_spans = vec![Span::raw(" Review · ")];
        let queue_style = if self.review_tab == ReviewTab::Queue {
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        };
        let changes_style = if self.review_tab == ReviewTab::Changes {
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(MUTED)
        };
        title_spans.push(Span::styled(
            format!("Queue ({})", self.review.len()),
            queue_style,
        ));
        title_spans.push(Span::styled(" · ", Style::default().fg(RULE)));
        title_spans.push(Span::styled(
            format!("Recent changes ({})", self.history.len()),
            changes_style,
        ));
        if self.review_tab == ReviewTab::Queue && !self.review_marked.is_empty() {
            title_spans.push(Span::styled(
                format!(" · {} marked", self.review_marked.len()),
                Style::default().fg(ACCENT),
            ));
        }
        title_spans.push(Span::raw(" "));

        let mut block = panel(Line::from(title_spans), focused);
        if !visual_set.is_empty() {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" VISUAL · {} selected ", visual_set.len()),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )));
        }
        let inner = block.inner(list_area);
        rects.list_inner = inner;
        rects.list_has_header = false;
        frame.render_widget(block, list_area);

        match self.review_tab {
            ReviewTab::Queue => {
                self.render_review_queue(frame, inner, detail_area, rects, &visual_set)
            }
            ReviewTab::Changes => self.render_review_changes(frame, inner, detail_area, rects),
        }
    }

    fn render_review_queue(
        &self,
        frame: &mut Frame,
        inner: Rect,
        detail_area: Option<Rect>,
        rects: &mut LayoutRects,
        visual_set: &std::collections::BTreeSet<usize>,
    ) {
        if let Some(d) = detail_area {
            self.render_review_right(frame, d, rects, visual_set.len());
        }

        if self.review.is_empty() {
            frame.render_widget(
                empty_state("Nothing needs a decision. Learned rules, undo blocks, and repeated fixes show up here."),
                inner,
            );
            return;
        }

        let mut items = Vec::new();
        let mut line_map: Vec<Option<usize>> = Vec::new();
        let mut selected_line = 0usize;
        let mut last: Option<ReviewSection> = None;
        for (i, item) in self.review.iter().enumerate() {
            if last != Some(item.section) {
                if last.is_some() {
                    items.push(ListItem::new(""));
                    line_map.push(None);
                }
                items.push(ListItem::new(Line::from(Span::styled(
                    format!(" {}", item.section.title()),
                    Style::default().add_modifier(Modifier::BOLD),
                ))));
                line_map.push(None);
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
            let in_visual = visual_set.contains(&i) && i != self.review_idx;
            let mut spans = vec![
                Span::styled(mark, Style::default().fg(ACCENT)),
                Span::raw(format!("{:<24} ", truncate(&item.title, 24))),
            ];
            if detail_area.is_none() || inner.width >= 78 {
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
            let mut list_item = ListItem::new(Line::from(spans));
            if in_visual {
                list_item = list_item.style(visual_style());
            }
            items.push(list_item);
            line_map.push(Some(i));
        }

        let row_style = if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        };
        let list = List::new(items).highlight_style(row_style);
        let mut state = ListState::default();
        state.select(Some(selected_line));
        frame.render_stateful_widget(list, inner, &mut state);

        let offset = state.offset();
        rects.list_offset = offset;
        for (slot, entry) in rects.review_line_map.iter_mut().enumerate() {
            *entry = line_map.get(offset + slot).copied().flatten();
        }
    }

    fn render_review_right(
        &self,
        frame: &mut Frame,
        area: Rect,
        rects: &mut LayoutRects,
        visual_count: usize,
    ) {
        let (inspect_area, actions_area) = if area.height >= 12 {
            let pct = self
                .layout
                .review_inspect_pct
                .clamp(INSPECT_PCT_MIN, INSPECT_PCT_MAX);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(pct),
                    Constraint::Percentage(100 - pct),
                ])
                .split(area);
            rects.review_inspect = chunks[0];
            (chunks[0], Some(chunks[1]))
        } else {
            (area, None)
        };

        self.render_review_inspect(frame, inspect_area);
        if let Some(act) = actions_area {
            self.render_review_actions(frame, act, visual_count);
        }
    }

    fn render_review_inspect(&self, frame: &mut Frame, area: Rect) {
        let muted = Style::default().fg(MUTED);
        let Some(item) = self.review.get(self.review_idx) else {
            let block = panel(Line::from(" Inspect "), false);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled("Nothing in queue.", muted))).block(block),
                area,
            );
            return;
        };

        let title = format!(" Inspect · {} ", item.type_badge);
        let block = panel(Line::from(title), false);
        let now = current_epoch_ms();
        let added = if item.added_ms == 0 {
            "in rules.toml".to_string()
        } else {
            format!("{} ago", format_age_ago(now, item.added_ms))
        };
        let last_used = match item.last_hit_ms {
            Some(t) => format!("{} ago", format_age_ago(now, t)),
            None => "never".to_string(),
        };
        let prob_color = if item.strikes >= 2 || item.warning.is_some() {
            Color::Yellow
        } else {
            Color::Reset
        };
        let apps = if item.seen_apps.is_empty() {
            "none in last 3 days".to_string()
        } else {
            item.seen_apps.join(", ")
        };

        let kv = |k: &str, v: String, style: Style| {
            Line::from(vec![
                Span::styled(format!(" {k:<9} "), muted),
                Span::styled(v, style),
            ])
        };

        let mut lines = vec![
            kv(
                item.kind_noun,
                item.item_label.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            kv(
                "Problem",
                item.problem.clone(),
                Style::default().fg(prob_color),
            ),
            Line::from(""),
            kv("Language", item.lang_label.clone(), Style::default()),
            kv("Source", item.source.clone(), muted),
            kv("Added", format!("{added} · Last used: {last_used}"), muted),
        ];
        if item.section != ReviewSection::Almost {
            lines.push(kv(
                "Usage",
                format!("used {}× · undone {}×", item.hits, item.strikes),
                muted,
            ));
        }
        lines.push(kv("Apps", apps, muted));

        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }

    fn render_review_actions(&self, frame: &mut Frame, area: Rect, visual_count: usize) {
        let block = panel(Line::from(" Actions "), false);
        let muted = Style::default().fg(MUTED);
        let mut lines = Vec::new();
        if visual_count > 0 {
            lines.push(Line::from(vec![
                Span::styled(
                    " a Keep   ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("Keep {visual_count} selected")),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    " d Remove ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("Remove {visual_count} selected")),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    " Space    ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("Mark range (Esc exit)"),
            ]));
        } else if let Some(item) = self.review.get(self.review_idx) {
            lines.push(Line::from(vec![
                Span::styled(
                    " a Keep   ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(item.keep_effect.clone()),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    " d Remove ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(item.remove_effect.clone()),
            ]));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" v Visual · Space Mark", muted)));
            lines.push(Line::from(Span::styled(
                " A/D Apply marked · u Undo",
                muted,
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Nothing selected. Press h or [ to inspect Recent changes.",
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

    fn render_review_changes(
        &self,
        frame: &mut Frame,
        inner: Rect,
        detail_area: Option<Rect>,
        rects: &mut LayoutRects,
    ) {
        if let Some(d) = detail_area {
            self.render_change_detail(frame, d);
        }

        if self.history.is_empty() {
            frame.render_widget(
                empty_state("No changes made in this session yet. Adding words, keeping rules, deleting fixes, or changing settings will show up here."),
                inner,
            );
            return;
        }

        let now = current_epoch_ms();
        let items: Vec<ListItem> =
            self.history
                .iter()
                .rev()
                .map(|e| {
                    let badge_color = if e.reverted {
                        MUTED
                    } else {
                        match e.kind {
                            super::types::ChangeKind::Added => Color::Green,
                            super::types::ChangeKind::Kept => ACCENT,
                            super::types::ChangeKind::Removed
                            | super::types::ChangeKind::Blocked => Color::Yellow,
                            super::types::ChangeKind::Changed => Color::Reset,
                        }
                    };
                    let age = format_age_ago(now, e.ts_ms);
                    let verb_prefix = format!("{} ", e.kind.verb());
                    let summary = e.summary.strip_prefix(&verb_prefix).unwrap_or(&e.summary);
                    let mut spans = vec![
                        Span::styled(
                            format!(" {} ", e.kind.badge()),
                            Style::default().fg(badge_color),
                        ),
                        Span::styled(
                            format!("{:<28} ", truncate(summary, 28)),
                            if e.reverted {
                                Style::default().fg(MUTED)
                            } else {
                                Style::default()
                            },
                        ),
                        Span::styled(age, Style::default().fg(MUTED)),
                    ];
                    if e.reverted {
                        spans.push(Span::styled(
                            "  reverted",
                            Style::default().fg(Color::Yellow),
                        ));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();

        let row_style = if self.focus_sidebar {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            selected_style()
        };
        let list = List::new(items).highlight_style(row_style);
        let mut state = ListState::default();
        state.select(Some(self.changes_idx));
        frame.render_stateful_widget(list, inner, &mut state);
        rects.list_offset = state.offset();
    }

    fn render_change_detail(&self, frame: &mut Frame, area: Rect) {
        let block = panel(Line::from(" Change details "), false);
        let muted = Style::default().fg(MUTED);
        if self.history.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled("No changes recorded yet.", muted)))
                    .block(block),
                area,
            );
            return;
        }
        let hist_idx = self.history.len() - 1 - self.changes_idx.min(self.history.len() - 1);
        let entry = &self.history[hist_idx];
        let age = format!("{} ago", format_age_ago(current_epoch_ms(), entry.ts_ms));
        let mut lines = vec![
            Line::from(Span::styled(
                entry.summary.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled(" Action  ", muted),
                Span::raw(entry.kind.verb()),
            ]),
            Line::from(vec![
                Span::styled(" Screen  ", muted),
                Span::raw(entry.screen.label(false)),
            ]),
            Line::from(vec![Span::styled(" When    ", muted), Span::raw(age)]),
            Line::from(vec![
                Span::styled(" Status  ", muted),
                if entry.reverted {
                    Span::styled(
                        "reverted (not currently active)",
                        Style::default().fg(Color::Yellow),
                    )
                } else {
                    Span::styled("active on disk", Style::default().fg(Color::Green))
                },
            ]),
            Line::from(""),
        ];
        if entry.reverted && !entry.forward_ops.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    " u / ⏎   ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("Reapply this change"),
            ]));
        } else if !entry.reverted && !entry.inverse_ops.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    " u / ⏎   ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("Revert this change"),
            ]));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(block),
            area,
        );
    }
}
