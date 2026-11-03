use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use smartkey_core::{
    apply_key_to_buffer, DiagnosticEvent, Dialect, EnglishScorer, EvalContext, KeyInput, MatchKind,
    Preset, RuleStore, SessionAction, SmartKeyEngine, SmartKeySession, UndoResolution,
};

fn default_config_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("smartkey");
        }
    }
    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".config/smartkey");
        }
    }
    PathBuf::from(".smartkey")
}

#[cfg(feature = "bundled-en")]
fn load_scorer() -> Result<EnglishScorer, String> {
    EnglishScorer::new_bundled().map_err(|e| format!("failed to load bundled lexicon: {e}"))
}

#[cfg(not(feature = "bundled-en"))]
fn load_scorer() -> Result<EnglishScorer, String> {
    Err("smartkeyctl was compiled without `bundled-en` feature".to_string())
}

fn load_rule_store(dir: &Path, preset: Preset) -> Result<RuleStore, String> {
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");
    RuleStore::load_or_init(rules_path, state_path, preset).map_err(|e| e.to_string())
}

fn notify_fcitx5_reload() -> bool {
    if let Ok(status) = Command::new("busctl")
        .args([
            "--user",
            "call",
            "org.fcitx.Fcitx5",
            "/controller",
            "org.fcitx.Fcitx.Controller1",
            "ReloadConfig",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        if status.success() {
            return true;
        }
    }
    if let Ok(status) = Command::new("fcitx5-remote")
        .arg("-r")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        return status.success();
    }
    false
}

fn detect_fcitx5_running() -> bool {
    Command::new("fcitx5-remote")
        .arg("--check")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn detect_installed_addon() -> Option<PathBuf> {
    if let Ok(home) = env::var("HOME") {
        let user_so = PathBuf::from(home).join(".local/lib/fcitx5/libsmartkey.so");
        if user_so.exists() {
            return Some(user_so);
        }
    }
    for sys in [
        "/usr/lib/fcitx5/libsmartkey.so",
        "/usr/lib64/fcitx5/libsmartkey.so",
        "/usr/local/lib/fcitx5/libsmartkey.so",
    ] {
        let p = PathBuf::from(sys);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

#[derive(Serialize)]
struct StatusJson {
    enabled: bool,
    fcitx5_running: bool,
    addon_library: Option<String>,
    rules_path: String,
    dialect: Dialect,
    personal_words: usize,
    learned_corrections: usize,
    autocorrects_applied: u32,
    backspace_undos: u32,
    text: &'static str,
    tooltip: String,
}

fn cmd_status(args: &[String]) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut as_json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            "--json" => {
                as_json = true;
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl status [--json] [--dir <path>])"
                ));
            }
        }
        i += 1;
    }

    let store = load_rule_store(&dir, Preset::Balanced)?;
    let c = store.compiled();
    let st = store.state();
    let fcitx_up = detect_fcitx5_running();
    let addon_so = detect_installed_addon();

    let total_applied: u32 = st.applied_counts.values().copied().sum();
    let total_undos: u32 = st.undo_counts.values().copied().sum();
    let total_learned = c.corrections.len() + c.context_corrections.len();
    let rules_str = store
        .rules_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<in-memory>".to_string());

    if as_json {
        let text = if c.enabled { "SK" } else { "SK-OFF" };
        let tooltip = format!(
            "SmartKey: {} | learned={} | applied={} | undos={}",
            if c.enabled { "enabled" } else { "disabled" },
            total_learned,
            total_applied,
            total_undos
        );
        let payload = StatusJson {
            enabled: c.enabled,
            fcitx5_running: fcitx_up,
            addon_library: addon_so.map(|p| p.display().to_string()),
            rules_path: rules_str,
            dialect: c.scorer_config.dialect,
            personal_words: c.personal_dict.len(),
            learned_corrections: total_learned,
            autocorrects_applied: total_applied,
            backspace_undos: total_undos,
            text,
            tooltip,
        };
        println!("{}", serde_json::to_string(&payload).unwrap());
        return Ok(());
    }

    println!("=== SmartKey Status ===");
    println!(
        "Engine State         : {}",
        if c.enabled { "ENABLED" } else { "DISABLED" }
    );
    println!(
        "Fcitx5 Daemon        : {}",
        if fcitx_up {
            "running"
        } else {
            "not detected in current session"
        }
    );
    println!(
        "Fcitx5 Addon Library : {}",
        addon_so
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "not found".to_string())
    );
    println!("Rules File           : {rules_str}");
    println!(
        "Profile              : dialect={:?} | aggressiveness={:?} | undo_window={}ms",
        c.scorer_config.dialect, c.scorer_config.aggressiveness, c.undo_window_ms
    );
    println!(
        "Personal Brain       : {} dict words | {} learned rules | {} blocklisted pairs",
        c.personal_dict.len(),
        total_learned,
        c.blocklist.len()
    );
    println!(
        "Diagnostic Counters  : {} autocorrects | {} backspace undos",
        total_applied, total_undos
    );
    Ok(())
}

fn cmd_set_enabled(args: &[String], mode: Option<bool>) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl enable|disable|toggle [--dir <path>])"
                ));
            }
        }
        i += 1;
    }

    let mut store = load_rule_store(&dir, Preset::Balanced)?;
    let target = mode.unwrap_or(!store.compiled().enabled);
    store.set_enabled(target).map_err(|e| e.to_string())?;
    let reloaded = notify_fcitx5_reload();
    println!(
        "SmartKey {} ({})",
        if target { "enabled" } else { "disabled" },
        if reloaded {
            "Fcitx5 reloaded via DBus"
        } else {
            "saved to rules.toml; will apply on next focus change"
        }
    );
    Ok(())
}

fn cmd_reload() -> Result<(), String> {
    if notify_fcitx5_reload() {
        println!("Reloaded Fcitx5 SmartKey configuration via DBus.");
    } else {
        println!(
            "Could not reach Fcitx5 DBus in this shell; rules.toml still hot-reloads automatically on window focus change."
        );
    }
    Ok(())
}

fn cmd_init(args: &[String]) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut preset = Preset::Balanced;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            "--preset" => {
                i += 1;
                let raw = args.get(i).ok_or("--preset requires a name")?;
                preset = Preset::parse(raw).ok_or_else(|| {
                    format!(
                        "unknown preset {:?} (expected balanced, chat_casual, or developer_strict)",
                        raw
                    )
                })?;
            }
            "--force" => {
                force = true;
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl init [--preset balanced|chat_casual|developer_strict] [--dir <path>] [--force])"
                ));
            }
        }
        i += 1;
    }

    let rules_path = dir.join("rules.toml");
    if rules_path.exists() && !force {
        println!(
            "Already initialized at {} (pass --force to overwrite with fresh preset)",
            rules_path.display()
        );
        return Ok(());
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(&rules_path, preset.initial_toml()).map_err(|e| e.to_string())?;
    let _ = notify_fcitx5_reload();
    println!(
        "Initialized SmartKey rules ({preset:?}) at {}",
        rules_path.display()
    );
    Ok(())
}

fn cmd_rules(args: &[String]) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl rules [--dir <path>])"
                ));
            }
        }
        i += 1;
    }

    let store = load_rule_store(&dir, Preset::Balanced)?;
    let c = store.compiled();
    let st = store.state();

    println!("=== SmartKey Personal Rule Brain ===");
    println!(
        "Rules File           : {}",
        store
            .rules_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<in-memory>".to_string())
    );
    println!(
        "Settings             : enabled={} | dialect={:?} | aggressiveness={:?} | undo_window={}ms",
        c.enabled, c.scorer_config.dialect, c.scorer_config.aggressiveness, c.undo_window_ms
    );
    println!("Personal Dict (en)   : {} words", c.personal_dict.len());
    println!("Blocklist Pairs      : {} pairs", c.blocklist.len());
    println!(
        "Learned Corrections  : {} unigram | {} contextual",
        c.corrections.len(),
        c.context_corrections.len()
    );
    println!("Snippets             : {} triggers", c.snippets.len());
    println!(
        "Telex Passthrough    : {} ASCII words",
        c.telex_passthrough.len()
    );
    println!(
        "Staging Counters     : {} edits | {} context | {} unknowns | {} trouble words",
        st.staged_corrections.len(),
        st.staged_context.len(),
        st.untouched_unknowns.len(),
        st.trouble_words.len()
    );
    if !st.skipped_edits.is_empty() {
        println!(
            "\nRecent Guard-Rejected Edits ({}):",
            st.skipped_edits.len()
        );
        for entry in st.skipped_edits.iter().rev().take(10) {
            println!(
                "  [{}x] ({}) prev={:?} {:?} -> {:?} ({})",
                entry.count, entry.lang, entry.prev_word, entry.orig, entry.fixed, entry.reason
            );
        }
    }
    Ok(())
}

fn match_kind_slug(kind: MatchKind) -> &'static str {
    match kind {
        MatchKind::Bypass => "bypass",
        MatchKind::ValidWord => "valid_word",
        MatchKind::StretchedWord => "stretched_word",
        MatchKind::UserSnippet => "user_snippet",
        MatchKind::UserContextRule => "user_context_rule",
        MatchKind::UserCorrectionRule => "user_correction_rule",
        MatchKind::ShiftSlip => "shift_slip",
        MatchKind::DialectNormalization => "dialect_normalization",
        MatchKind::Contraction => "contraction",
        MatchKind::UnmergedCompound => "unmerged_compound",
        MatchKind::SemicolonContraction => "semicolon_contraction",
        MatchKind::TransposedApostrophe => "transposed_apostrophe",
        MatchKind::KnownTypoDict => "known_typo_dict",
        MatchKind::SwitchChatter => "switch_chatter",
        MatchKind::MissedSpaceSplit => "missed_space_split",
        MatchKind::FuzzyLexicon => "fuzzy_lexicon",
        MatchKind::SmashRescue => "smash_rescue",
    }
}

fn undo_resolution_label(res: &UndoResolution) -> String {
    match res {
        UndoResolution::Triggered => "triggered".to_string(),
        UndoResolution::KeptVerbatim => "kept_verbatim".to_string(),
        UndoResolution::EditedToThird { final_word } => {
            format!("edited_to_third -> {final_word:?}")
        }
        UndoResolution::SelfConfirmCancelled => "self_confirm_cancelled".to_string(),
        UndoResolution::ClauseErased => "clause_erased".to_string(),
    }
}

fn print_top_pairs(title: &str, map: &std::collections::BTreeMap<String, u32>, max_items: usize) {
    if map.is_empty() {
        return;
    }
    let mut pairs: Vec<(&String, &u32)> = map.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("\n{title}:");
    for (pair, count) in pairs.into_iter().take(max_items) {
        if let Some((lhs, rhs)) = pair.split_once("->") {
            println!("  [{count}x] {lhs:?} -> {rhs:?}");
        } else {
            println!("  [{count}x] {pair}");
        }
    }
}

fn cmd_log(args: &[String]) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut limit: usize = 50;
    let mut clear = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            "--limit" | "-n" => {
                i += 1;
                let raw = args.get(i).ok_or("--limit requires a number")?;
                limit = raw
                    .parse::<usize>()
                    .map_err(|_| format!("invalid --limit {raw:?} (expected integer)"))?;
            }
            "--clear" => {
                clear = true;
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl log [--limit <N>] [--clear] [--dir <path>])"
                ));
            }
        }
        i += 1;
    }

    let mut store = load_rule_store(&dir, Preset::Balanced)?;
    let state_label = store
        .state_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<in-memory>".to_string());

    if clear {
        store.clear_diagnostic_logs().map_err(|e| e.to_string())?;
        let _ = notify_fcitx5_reload();
        println!("Cleared SmartKey diagnostic logs at {state_label}");
        return Ok(());
    }

    let st = store.state();
    let total_applied: u32 = st.applied_counts.values().copied().sum();
    let total_undos: u32 = st.undo_counts.values().copied().sum();
    let total_manual: u32 = st.manual_fix_counts.values().copied().sum();

    println!("=== SmartKey Diagnostic Log ===");
    println!("State File           : {state_label}");
    println!(
        "Autocorrects Applied : {} total ({} unique pairs)",
        total_applied,
        st.applied_counts.len()
    );
    println!(
        "Backspace Undos      : {} total ({} unique pairs)",
        total_undos,
        st.undo_counts.len()
    );
    println!(
        "Manual Backspace Fix : {} total ({} unique pairs)",
        total_manual,
        st.manual_fix_counts.len()
    );

    print_top_pairs("Backspace Undo Counts (1x / 2x / 3x+)", &st.undo_counts, 10);
    print_top_pairs(
        "Manual Backspace Fix Counts (1x / 2x / 3x+)",
        &st.manual_fix_counts,
        10,
    );
    print_top_pairs("Top Autocorrects Applied", &st.applied_counts, 10);

    if st.recent_events.is_empty() {
        println!("\nRecent Events: (none yet)");
        return Ok(());
    }

    let total_events = st.recent_events.len();
    let start = total_events.saturating_sub(limit);
    let shown = &st.recent_events[start..];
    println!(
        "\nRecent Events (showing {} of {}, oldest -> newest):",
        shown.len(),
        total_events
    );
    for ev in shown {
        match ev {
            DiagnosticEvent::AutocorrectApplied {
                typed,
                applied,
                kind,
                total_count,
            } => {
                println!(
                    "  [{total_count}x] AUTOCORRECT ({}) {typed:?} -> {applied:?}",
                    match_kind_slug(*kind)
                );
            }
            DiagnosticEvent::BackspaceUndo {
                typed,
                rejected,
                resolution,
                undo_count,
            } => {
                println!(
                    "  [{undo_count}x] BACKSPACE_UNDO ({}) {typed:?} -> {rejected:?}",
                    undo_resolution_label(resolution)
                );
            }
            DiagnosticEvent::ManualBackspaceFix {
                orig,
                fixed,
                outcome,
                attempt_count,
            } => {
                println!("  [{attempt_count}x] MANUAL_FIX ({outcome}) {orig:?} -> {fixed:?}");
            }
        }
    }

    Ok(())
}

fn cmd_explain(args: &[String]) -> Result<(), String> {
    let mut prev: Option<String> = None;
    let mut punct: Option<char> = None;
    let mut dialect: Option<Dialect> = None;
    let mut sentence_start = false;
    let mut dir: Option<PathBuf> = None;
    let mut token: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--prev" => {
                i += 1;
                prev = args.get(i).cloned();
            }
            "--punct" => {
                i += 1;
                punct = args.get(i).and_then(|s| s.chars().next());
            }
            "--dialect" => {
                i += 1;
                dialect = Some(match args.get(i).map(String::as_str) {
                    Some("us" | "en_US") => Dialect::EnUs,
                    Some("gb" | "uk" | "en_GB") => Dialect::EnGb,
                    _ => Dialect::Mixed,
                });
            }
            "--dir" => {
                i += 1;
                dir = args.get(i).map(PathBuf::from);
            }
            "--sentence-start" => {
                sentence_start = true;
            }
            other => {
                token = Some(other.to_string());
            }
        }
        i += 1;
    }

    let Some(tok) = token else {
        return Err(
            "usage: smartkeyctl explain [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>"
                .to_string(),
        );
    };

    let scorer = load_scorer()?;
    let cfg_dir = dir.unwrap_or_else(default_config_dir);
    let mut store = if cfg_dir.join("rules.toml").exists() {
        load_rule_store(&cfg_dir, Preset::Balanced)?
    } else {
        RuleStore::in_memory(Preset::Balanced)
    };
    if let Some(d) = dialect {
        store.scorer_config_mut().dialect = d;
    }

    let engine = SmartKeyEngine::new(Some(scorer), store);
    let ctx = EvalContext {
        prev_word: prev.as_deref(),
        boundary_punct: punct,
        at_sentence_start: sentence_start,
        ..EvalContext::default()
    };
    let exp = engine.explain_token(&tok, &ctx);
    println!("{}", serde_json::to_string_pretty(&exp).unwrap());
    Ok(())
}

struct RawTermGuard {
    fd: i32,
    orig: libc::termios,
}

impl RawTermGuard {
    fn enter() -> io::Result<Self> {
        let fd = libc::STDIN_FILENO;
        unsafe {
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut orig) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = orig;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSAFLUSH, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { fd, orig })
        }
    }
}

impl Drop for RawTermGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSAFLUSH, &self.orig);
        }
    }
}

fn cmd_repl(args: &[String]) -> Result<(), String> {
    let mut dir = default_config_dir();
    let mut in_memory = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                i += 1;
                dir = PathBuf::from(args.get(i).ok_or("--dir requires a path")?);
            }
            "--in-memory" => {
                in_memory = true;
            }
            other => {
                return Err(format!(
                    "unknown flag {other} (usage: smartkeyctl repl [--dir <path>] [--in-memory])"
                ));
            }
        }
        i += 1;
    }

    let scorer = load_scorer()?;
    let store = if in_memory {
        RuleStore::in_memory(Preset::Balanced)
    } else {
        load_rule_store(&dir, Preset::Balanced)?
    };

    let rules_label = store
        .rules_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<in-memory>".to_string());

    let mut engine = SmartKeyEngine::new(Some(scorer), store);
    let mut session = SmartKeySession::new();
    let t0 = Instant::now();

    println!("SmartKey Interactive REPL — Layer 1 + Layer 2 Learning Active");
    println!("Rules brain: {rules_label}  (Ctrl-C or Esc to quit, Ctrl-W for word-backspace)\n");

    let _guard =
        RawTermGuard::enter().map_err(|e| format!("REPL requires an interactive TTY ({e})"))?;

    let mut line_buf = String::new();
    let mut last_note = String::from("ready");

    let redraw = |line: &str, note: &str| {
        print!("\r\x1b[2K> {line}    \x1b[90m[{note}]\x1b[0m");
        let _ = io::stdout().flush();
    };
    redraw(&line_buf, &last_note);

    let mut stdin = io::stdin();
    let mut byte_buf = [0u8; 4];

    loop {
        if stdin.read(&mut byte_buf[..1]).map_err(|e| e.to_string())? == 0 {
            break;
        }
        let now_ms = t0.elapsed().as_millis() as u64;
        let b0 = byte_buf[0];

        if b0 == 3 || b0 == 27 {
            let _ = session.reset(&mut engine);
            println!("\nBye!");
            break;
        }

        let key = if b0 == b'\r' || b0 == b'\n' {
            KeyInput::Enter
        } else if b0 == 0x7f || b0 == 0x08 {
            KeyInput::BackSpace
        } else if b0 == 0x17 {
            KeyInput::CtrlBackSpace
        } else if b0 == b'\t' {
            KeyInput::Tab
        } else {
            let width = if b0 < 0x80 {
                1
            } else if (b0 & 0xE0) == 0xC0 {
                2
            } else if (b0 & 0xF0) == 0xE0 {
                3
            } else {
                4
            };
            if width > 1 {
                stdin
                    .read_exact(&mut byte_buf[1..width])
                    .map_err(|e| e.to_string())?;
            }
            let Ok(s) = std::str::from_utf8(&byte_buf[..width]) else {
                continue;
            };
            let Some(ch) = s.chars().next() else {
                continue;
            };
            KeyInput::Char(ch)
        };

        if key == KeyInput::Enter {
            let out = session.process_key(&mut engine, key, now_ms);
            line_buf.clear();
            println!();
            last_note = if out.learn_events.is_empty() {
                "newline".to_string()
            } else {
                format!("newline | {:?}", out.learn_events)
            };
            redraw(&line_buf, &last_note);
            continue;
        }

        let out = session.process_key(&mut engine, key, now_ms);
        apply_key_to_buffer(&mut line_buf, key, &out.action);

        let mut notes = Vec::new();
        if let SessionAction::Replace { insert, .. } = &out.action {
            if key == KeyInput::BackSpace {
                notes.push(format!("UNDO -> {insert:?}"));
            } else if let Some(kind) = out.matched_kind {
                notes.push(format!("fix -> {:?} ({kind:?})", insert.trim_end()));
            }
        }
        if !out.learn_events.is_empty() {
            notes.push(format!("learned: {:?}", out.learn_events));
        }
        if !notes.is_empty() {
            last_note = notes.join(" | ");
        }
        redraw(&line_buf, &last_note);
    }

    Ok(())
}

fn print_help() {
    println!(
        "\
smartkeyctl {} — Control & inspection CLI for SmartKey

USAGE:
  smartkeyctl <COMMAND> [OPTIONS]

COMMANDS:
  status [--json] [--dir <path>]     Show daemon, addon, and personal brain status
  enable [--dir <path>]              Enable SmartKey and reload Fcitx5
  disable [--dir <path>]             Disable SmartKey and reload Fcitx5
  toggle [--dir <path>]              Toggle SmartKey on/off (ideal for WM keybinds)
  reload                             Reload rules.toml in running Fcitx5 daemon
  log [--limit <N>] [--clear]        Show autocorrects and 1x/2x/3x+ backspace logs
  rules [--dir <path>]               Show personal dictionary, blocklist & learned rules
  explain [FLAGS] <word>             Explain scoring and candidate ranking for a token
  init [--preset <name>] [--force]   Initialize ~/.config/smartkey/rules.toml
  repl [--in-memory]                 Interactive terminal typing & learning sandbox",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("status") => cmd_status(&args[1..]),
        Some("enable" | "on") => cmd_set_enabled(&args[1..], Some(true)),
        Some("disable" | "off") => cmd_set_enabled(&args[1..], Some(false)),
        Some("toggle") => cmd_set_enabled(&args[1..], None),
        Some("reload") => cmd_reload(),
        Some("init") => cmd_init(&args[1..]),
        Some("rules") => cmd_rules(&args[1..]),
        Some("log") => cmd_log(&args[1..]),
        Some("explain") => cmd_explain(&args[1..]),
        Some("repl") => cmd_repl(&args[1..]),
        Some("--version" | "-V") => {
            println!("smartkeyctl {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        _ => {
            print_help();
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
