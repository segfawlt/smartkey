mod activity;
mod app;
mod dictionary;
mod modals;
mod ops;
mod review;
mod settings;
mod sidebar;
mod theme;
mod types;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use crossterm::execute;

use crate::{default_config_dir, CliError, CliResult};

#[cfg(test)]
pub(crate) use app::App;

const AUTO_REFRESH: Duration = Duration::from_secs(4);

struct TerminalGuard {
    mouse_enabled: bool,
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.mouse_enabled {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
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
    let mut app = app::App::new(cfg_dir, resolved_log_dir, crate::notify_fcitx5_reload);

    let mut terminal = ratatui::init();
    let mouse_enabled = execute!(io::stdout(), EnableMouseCapture).is_ok();
    let _guard = TerminalGuard { mouse_enabled };

    let mut last_refresh = Instant::now();
    loop {
        terminal
            .draw(|f| app.render(f))
            .map_err(|e| CliError::Runtime(format!("terminal draw failed: {e}")))?;

        if event::poll(Duration::from_millis(250))
            .map_err(|e| CliError::Runtime(format!("event poll failed: {e}")))?
        {
            match event::read().map_err(|e| CliError::Runtime(format!("event read failed: {e}")))? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if app.handle_key(key) {
                        break;
                    }
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                Event::Resize(_, _) => {}
                _ => {}
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
  2 Review       Learned rules and almost-learned fixes, plus session Recent changes
  3 Dictionary   Your words, fixes, snippets, blocked fixes, and Telex exceptions
  4 Settings     Correction, undo, Vietnamese, per-app, and logging options

KEYS:
  1-4 / Tab      Switch screen
  gg / G / C-d u Vim navigation; v visual range select (Review / Dictionary)
  < > + - = z    Resize panes, adjust Inspect split, reset layout, or zoom
  u / Ctrl+r     Undo / Redo change
  ?              Keys for the current screen
  q / Ctrl+C     Quit"
    );
}

#[cfg(test)]
mod tests {
    use super::types::{
        ActivityFilter, DictSection, Modal, ReviewTab, Screen, SettingsRow, SIDEBAR_WIDTH_DEFAULT,
        SPLIT_PCT_DEFAULT,
    };
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use smartkey_core::event_log::{current_epoch_ms, DiagnosticsConfig};
    use smartkey_core::{
        ConfidenceTier, DiagnosticEvent, EventLog, LangId, MatchKind, Preset, RuleOrigin, RuleStat,
        RuleStore,
    };

    use crate::load_rule_store_mut;
    use crate::load_rule_store_readonly;

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

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
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
        for label in [
            "1 Activity",
            "2 Review",
            "3 Dictionary",
            "4 Settings",
            "Updated",
            "Watching",
            "Mode",
        ] {
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

        for (w, h) in [
            (110u16, 32u16),
            (80, 24),
            (60, 16),
            (40, 12),
            (24, 8),
            (10, 4),
        ] {
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
        assert!(!app.undo_stack.is_empty());

        press(&mut app, 'u');
        assert!(!store(&d)
            .compiled()
            .blocklist
            .contains(&("coule".to_string(), "could".to_string())));
        assert!(app.undo_stack.is_empty());

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
    fn review_split_inspect_actions_and_watching_moved_to_status() {
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
        let rev_screen = screen_text(&app, 110, 32);
        assert!(rev_screen.contains("Inspect · Word (en)"), "{rev_screen}");
        assert!(rev_screen.contains("Actions"), "{rev_screen}");
        assert!(
            rev_screen.contains("Stays in en dictionary"),
            "{rev_screen}"
        );
        assert!(rev_screen.contains("Watching 1 staged"), "{rev_screen}");
        assert!(
            !rev_screen.contains("one-off typos are being"),
            "Watching note should live in Status, not Review:\n{rev_screen}"
        );

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
    fn recent_changes_history_and_multi_step_undo_redo() {
        let d = dirs("history");
        {
            let mut s = load_rule_store_mut(&d.cfg, Preset::Balanced).unwrap();
            s.add_user_personal_word(&LangId::en(), "zorblax").unwrap();
            s.state_mut().rule_stats.insert(
                RuleStore::stat_key_word("en", "zorblax"),
                RuleStat {
                    origin: RuleOrigin::Untouched3x,
                    created_epoch_ms: current_epoch_ms(),
                    last_hit_epoch_ms: None,
                    hits: 0,
                },
            );
            s.flush_state().unwrap();
        }
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);

        // 1. Add a word in Dictionary
        press(&mut app, '3');
        press(&mut app, 'n');
        type_text(&mut app, "Hyprland");
        app.handle_key(key(KeyCode::Enter));
        assert!(store(&d).compiled().personal_dict.contains("hyprland"));

        // 2. Keep a learned word in Review
        press(&mut app, '2');
        press(&mut app, 'a');
        let s = store(&d);
        let (tier, ..) = s.classify_rule_confidence(&RuleStore::stat_key_word("en", "zorblax"));
        assert_eq!(tier, ConfidenceTier::Manual);
        assert_eq!(app.history.len(), 2);

        // Switch to Recent changes tab and verify newest-first display + revert
        press(&mut app, 'l');
        assert_eq!(app.review_tab, ReviewTab::Changes);
        let changes_screen = screen_text(&app, 110, 32);
        assert!(changes_screen.contains("Kept"), "{changes_screen}");
        assert!(changes_screen.contains("Hyprland"), "{changes_screen}");

        // Revert Keep via Enter inside Recent changes
        app.handle_key(key(KeyCode::Enter));
        let (tier_after, ..) =
            store(&d).classify_rule_confidence(&RuleStore::stat_key_word("en", "zorblax"));
        assert_eq!(tier_after, ConfidenceTier::Suspect);

        // Reapply Keep via Ctrl+r
        app.handle_key(ctrl('r'));
        let (tier_redone, ..) =
            store(&d).classify_rule_confidence(&RuleStore::stat_key_word("en", "zorblax"));
        assert_eq!(tier_redone, ConfidenceTier::Manual);

        // Global undo twice restores initial state
        press(&mut app, 'h');
        assert_eq!(app.review_tab, ReviewTab::Queue);
        press(&mut app, 'u');
        press(&mut app, 'u');
        assert!(!store(&d).compiled().personal_dict.contains("hyprland"));
    }

    #[test]
    fn vim_motions_and_visual_mass_select_work_cleanly() {
        let d = dirs("vim_visual");
        {
            let mut s = load_rule_store_mut(&d.cfg, Preset::Balanced).unwrap();
            for word in ["alpha", "bravo", "charlie"] {
                s.add_user_personal_word(&LangId::en(), word).unwrap();
                s.state_mut().rule_stats.insert(
                    RuleStore::stat_key_word("en", word),
                    RuleStat {
                        origin: RuleOrigin::Untouched3x,
                        created_epoch_ms: current_epoch_ms(),
                        last_hit_epoch_ms: None,
                        hits: 0,
                    },
                );
            }
            s.flush_state().unwrap();
        }
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        press(&mut app, '2');
        assert_eq!(app.review.len(), 3);

        // Visual select first two rows and mark with Space
        press(&mut app, 'v');
        assert_eq!(app.visual_anchor, Some(0));
        press(&mut app, 'j');
        let vis_text = screen_text(&app, 110, 32);
        assert!(vis_text.contains("VISUAL · 2 selected"), "{vis_text}");
        press(&mut app, ' ');
        assert!(app.visual_anchor.is_none());
        assert_eq!(app.review_marked.len(), 2);

        // gg -> top, v + G -> batch remove all 3, u -> restore all 3
        press(&mut app, 'g');
        press(&mut app, 'g');
        assert_eq!(app.review_idx, 0);
        press(&mut app, 'v');
        press(&mut app, 'G');
        assert_eq!(app.review_idx, 2);
        press(&mut app, 'd');
        assert!(matches!(app.modal, Some(Modal::Confirm { .. })));
        press(&mut app, 'y');
        assert!(app.review.is_empty());

        // Safe no-op on empty list
        press(&mut app, 'v');
        assert!(app.visual_anchor.is_none());

        press(&mut app, 'u');
        assert_eq!(app.review.len(), 3);

        // Ctrl+h focuses sidebar, Ctrl+l returns to main
        app.handle_key(ctrl('h'));
        assert!(app.focus_sidebar);
        app.handle_key(ctrl('l'));
        assert!(!app.focus_sidebar);
    }

    #[test]
    fn keyboard_and_mouse_resize_and_clicks_work() {
        let d = dirs("resize_mouse");
        let mut app = App::new(d.cfg.clone(), d.logs.clone(), test_reload);
        let initial_split = app.layout.split_pct;
        press(&mut app, '>');
        assert!(app.layout.split_pct > initial_split);
        press(&mut app, '+');
        assert!(app.layout.review_inspect_pct > 62);
        press(&mut app, '=');
        assert_eq!(app.layout.split_pct, SPLIT_PCT_DEFAULT);

        press(&mut app, 'z');
        let zoomed = screen_text(&app, 110, 32);
        assert!(zoomed.contains("[zoom]"), "{zoomed}");
        press(&mut app, 'z');

        let _ = screen_text(&app, 110, 32);
        let r = app.rects.get();
        // Mouse click on 3rd row in sidebar nav (Dictionary)
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.nav_inner.x + 2,
            row: r.nav_inner.y + 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.screen, Screen::Dictionary);

        // Drag sidebar border wider
        let border_x = r.sidebar.right().saturating_sub(1);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: border_x,
            row: r.sidebar.y + 2,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: border_x + 6,
            row: r.sidebar.y + 2,
            modifiers: KeyModifiers::NONE,
        });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: border_x + 6,
            row: r.sidebar.y + 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.layout.sidebar_width, SIDEBAR_WIDTH_DEFAULT + 6);
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
