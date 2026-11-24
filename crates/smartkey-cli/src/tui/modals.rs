use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::App;
use super::theme::{
    display_setting, input_line, selected_style, split_lang_prefix, step, unit_hint, ACCENT, MUTED,
};
use super::types::{ChangeKind, Modal, RowAction, Screen, StoreOp, APP_CHOICES};

impl App {
    pub(super) fn handle_modal_key(&mut self, key: KeyEvent) {
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
                            ChangeKind::Changed,
                            vec![StoreOp::SetSetting {
                                key: spec.key.to_string(),
                                value,
                            }],
                            format!("{}: {shown}", spec.label),
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
                        ChangeKind::Changed,
                        vec![StoreOp::SetAppProfile {
                            app: app.clone(),
                            profile,
                        }],
                        format!("{app}: {}", label.to_lowercase()),
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
                        let (kind, ops, msg) = form.to_ops();
                        self.mutate(kind, ops, msg);
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
            Modal::Confirm {
                prompt,
                kind,
                ops,
                msg,
                clear_marks,
            } => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    if clear_marks {
                        self.review_marked.clear();
                    }
                    self.mutate(kind, ops, msg);
                }
                KeyCode::Char('n') | KeyCode::Esc => self.set_status("Cancelled"),
                _ => {
                    self.modal = Some(Modal::Confirm {
                        prompt,
                        kind,
                        ops,
                        msg,
                        clear_marks,
                    })
                }
            },
        }
    }

    pub(super) fn render_modal(&self, frame: &mut Frame, area: Rect, modal: &Modal) {
        let (title, lines): (String, Vec<Line>) = match modal {
            Modal::Help => ("Keys".to_string(), self.help_lines()),
            Modal::Info { title, lines } => (
                title.clone(),
                lines.iter().map(|l| Line::from(l.clone())).collect(),
            ),
            Modal::Actions {
                title,
                items,
                selected,
            } => {
                let mut out: Vec<Line> = Vec::new();
                if items.is_empty() {
                    out.push(Line::from(Span::styled(
                        "Nothing to do for this event.",
                        Style::default().fg(MUTED),
                    )));
                }
                let label_w = items
                    .iter()
                    .map(|i| i.label.chars().count())
                    .max()
                    .unwrap_or(0);
                for (i, item) in items.iter().enumerate() {
                    let style = if i == *selected {
                        selected_style()
                    } else {
                        Style::default()
                    };
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
            Modal::EditNumber {
                spec,
                current,
                input,
            } => (
                spec.label.to_string(),
                vec![
                    Line::from(Span::styled(
                        format!(
                            "Now {}. Type a new value{}.",
                            display_setting(spec, current),
                            unit_hint(spec)
                        ),
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
                    let style = if i == *selected {
                        selected_style()
                    } else {
                        Style::default()
                    };
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
                    format!(" {k:<14}"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(v.to_string(), Style::default().fg(MUTED)),
            ])
        };
        let mut out = vec![
            row("1-4, tab", "switch screen"),
            row("esc / C-h l", "focus sidebar / window"),
            row("j k gg G", "move, top, bottom"),
            row("C-d/u C-f/b", "half-page / full-page"),
            row("< > + - = z", "resize panes, inspect height, reset, zoom"),
            row("u / Ctrl+r", "undo / redo change"),
            row("r / q", "reload / quit"),
            Line::from(""),
        ];
        out.extend(match self.screen {
            Screen::Activity => vec![
                row("⏎ / b a w p", "act on fix, add word, set app mode"),
                row("/  f  x  e", "search, filter, explain word, on/off"),
            ],
            Screen::Review => vec![
                row("h l / [ ]", "switch Queue ↔ Recent changes"),
                row("a  d  { }", "keep, remove, jump section"),
                row("v / space", "visual range select / mark (A/D for marked)"),
            ],
            Screen::Dictionary => vec![
                row("← → / [ ]", "switch section"),
                row("n  d  v  /", "new, delete, visual select, filter"),
            ],
            Screen::Settings => vec![
                row("← → / ⏎", "change value, edit number, pick app mode"),
                row("{ }", "jump between setting groups"),
            ],
        });
        out
    }
}
