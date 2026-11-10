use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use smartkey_core::{
    apply_key_to_buffer, DiagnosticEvent, Dialect, EnglishScorer, EvalContext, EvalOutcome,
    KeyInput, LangId, MatchKind, Preset, RuleStore, SessionAction, SmartKeyEngine,
    SmartKeySession, UndoResolution,
};

/// Git state stamped at build time by `build.rs`: HEAD hash or tag (e.g.
/// `cf4ba21`), with a `-dirty` suffix on uncommitted trees, or `release` for
/// tarball builds outside a git checkout.
const GIT_VERSION: &str = env!("SMARTKEY_GIT_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
enum CliError {
    /// Invalid flags or arguments -> exit code `2`.
    Usage(String),
    /// Operational failure (I/O, network, parse-locked TOML, unreachable daemon) -> exit code `1`.
    Runtime(String),
}

type CliResult = Result<(), CliError>;

fn default_config_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("smartkey");
        }
    }
    if let Some(home) = smartkey_core::resolve_home_dir() {
        return home.join(".config/smartkey");
    }
    env::temp_dir().join("smartkey/config")
}

fn load_scorer() -> Result<EnglishScorer, CliError> {
    if let Some(path) = smartkey_core::resolve_dict_path("en") {
        match fs::read(&path) {
            Ok(bytes) => match smartkey_core::EnglishLexicon::from_owned_bytes(bytes) {
                Ok(lex) => return Ok(EnglishScorer::new(lex)),
                Err(err) => {
                    eprintln!(
                        "warning: invalid dictionary pack at {} ({err}); falling back to bundled lexicon",
                        path.display()
                    );
                }
            },
            Err(err) => {
                eprintln!(
                    "warning: could not read dictionary pack at {} ({err}); falling back to bundled lexicon",
                    path.display()
                );
            }
        }
    }
    #[cfg(feature = "bundled-en")]
    {
        EnglishScorer::new_bundled()
            .map_err(|e| CliError::Runtime(format!("failed to load bundled lexicon: {e}")))
    }
    #[cfg(not(feature = "bundled-en"))]
    {
        Err(CliError::Runtime(
            "en.skdict not found in dictionary directories (~/.local/share/smartkey/dicts/ or /usr/share/smartkey/dicts/)"
                .to_string(),
        ))
    }
}

fn load_rule_store_mut(dir: &Path, preset: Preset) -> Result<RuleStore, CliError> {
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");
    RuleStore::load_or_init(rules_path, state_path, preset)
        .map_err(|e| CliError::Runtime(e.to_string()))
}

fn load_rule_store_readonly(dir: &Path, preset: Preset) -> Result<(RuleStore, bool), CliError> {
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");
    RuleStore::load_readonly_or_default(rules_path, state_path, preset)
        .map_err(|e| CliError::Runtime(e.to_string()))
}

fn notify_fcitx5_reload() -> bool {
    if let Ok(status) = Command::new("fcitx5-remote")
        .arg("-r")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        if status.success() {
            return true;
        }
    }
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

/// Searches user-local (`~/.local/lib/fcitx5`), standard (`/usr/lib`, `/usr/lib64`, `/usr/local/lib`),
/// and Debian/Ubuntu multiarch (`/usr/lib/<triplet>/fcitx5`) addon directories for `so_name`.
fn detect_fcitx5_module_so(so_name: &str) -> Option<PathBuf> {
    if let Some(home) = smartkey_core::resolve_home_dir() {
        let user_so = home.join(".local/lib/fcitx5").join(so_name);
        if user_so.exists() {
            return Some(user_so);
        }
    }
    for base in [
        "/usr/local/lib/fcitx5",
        "/usr/local/lib64/fcitx5",
        "/usr/lib/fcitx5",
        "/usr/lib64/fcitx5",
        "/usr/lib/x86_64-linux-gnu/fcitx5",
        "/usr/lib/aarch64-linux-gnu/fcitx5",
    ] {
        let p = Path::new(base).join(so_name);
        if p.exists() {
            return Some(p);
        }
    }
    if let Ok(entries) = fs::read_dir("/usr/lib") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.contains("linux-") {
                let p = entry.path().join("fcitx5").join(so_name);
                if p.exists() {
                    return Some(p);
                }
            }
        }
    }
    None
}

fn detect_installed_addon() -> Option<PathBuf> {
    detect_fcitx5_module_so("libsmartkey.so")
}

/// Validates desktop input-method routing environment variables without printing raw env values.
fn inspect_im_environment() -> (bool, Vec<String>) {
    let mut warnings = Vec::new();
    let is_wayland = env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
        || env::var("XDG_SESSION_TYPE")
            .map(|s| s.eq_ignore_ascii_case("wayland"))
            .unwrap_or(false);

    match env::var("XMODIFIERS") {
        Ok(v) if v.contains("@im=fcitx") => {}
        _ => {
            warnings.push(
                "XMODIFIERS does not include '@im=fcitx' (XWayland/X11 apps may bypass Fcitx5)"
                    .to_string(),
            );
        }
    }

    if is_wayland {
        if env::var("GTK_IM_MODULE")
            .map(|s| s.contains("fcitx"))
            .unwrap_or(false)
        {
            warnings.push(
                "GTK_IM_MODULE=fcitx on Wayland routes GTK4 apps via legacy DBus instead of native Wayland text-input; consider unsetting GTK_IM_MODULE on Wayland"
                    .to_string(),
            );
        }
    } else if env::var_os("DISPLAY").is_some_and(|v| !v.is_empty()) {
        if !env::var("GTK_IM_MODULE")
            .map(|s| s.contains("fcitx"))
            .unwrap_or(false)
        {
            warnings.push("GTK_IM_MODULE is not set to 'fcitx' on X11".to_string());
        }
        if !env::var("QT_IM_MODULE")
            .map(|s| s.contains("fcitx"))
            .unwrap_or(false)
        {
            warnings.push("QT_IM_MODULE is not set to 'fcitx' on X11".to_string());
        }
    }

    (warnings.is_empty(), warnings)
}

#[derive(Serialize)]
struct StatusJson {
    enabled: bool,
    vi_lotus_enabled: bool,
    fcitx5_running: bool,
    addon_library: Option<String>,
    rules_path: String,
    initialized: bool,
    parse_locked: bool,
    im_env_ok: bool,
    im_env_warnings: Vec<String>,
    dialect: Dialect,
    personal_words: usize,
    learned_corrections: usize,
    autocorrects_applied: u32,
    backspace_undos: u32,
    text: &'static str,
    tooltip: String,
}

fn cmd_status(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut as_json = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey status [--json] [--dir <path>]\n\n\
                     Inspect Fcitx5 daemon, installed addon library, IM environment, and personal rule counts."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            "--json" => {
                as_json = true;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey status [--json] [--dir <path>])"
                )));
            }
        }
        i += 1;
    }

    let (store, initialized) = load_rule_store_readonly(&dir, Preset::Balanced)?;
    let c = store.compiled();
    let st = store.state();
    let fcitx_up = detect_fcitx5_running();
    let addon_so = detect_installed_addon();
    let (im_env_ok, im_env_warnings) = inspect_im_environment();

    let total_applied: u32 = st.applied_counts.values().copied().sum();
    let total_undos: u32 = st.undo_counts.values().copied().sum();
    let total_learned = c.corrections.len() + c.context_corrections.len();
    let expected_rules_path = dir.join("rules.toml");
    let rules_str = if initialized {
        expected_rules_path.display().to_string()
    } else {
        format!("{} (not initialized yet)", expected_rules_path.display())
    };

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
            vi_lotus_enabled: c.vi_lotus_enabled,
            fcitx5_running: fcitx_up,
            addon_library: addon_so.map(|p| p.display().to_string()),
            rules_path: expected_rules_path.display().to_string(),
            initialized,
            parse_locked: store.is_parse_locked(),
            im_env_ok,
            im_env_warnings,
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
        "Lotus Bridge Plugin  : {}",
        if c.vi_lotus_enabled {
            "ENABLED (vi-lotus)"
        } else {
            "DISABLED (vi-lotus)"
        }
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
    println!(
        "IM Environment       : {}",
        if im_env_ok {
            "ok".to_string()
        } else {
            im_env_warnings.join("; ")
        }
    );
    println!("Rules File           : {rules_str}");
    if let Some(parse_err) = store.parse_error() {
        println!("TOML Syntax Warning  : WRITE-LOCKED ({parse_err})");
    }
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

fn cmd_set_enabled(args: &[String], mode: Option<bool>) -> CliResult {
    let mut dir = default_config_dir();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey <enable|disable|toggle> [--dir <path>]\n  Aliases: on, off\n\n\
                     Persist `[settings].enabled` in rules.toml and reload Fcitx5."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey enable|disable|toggle [--dir <path>])"
                )));
            }
        }
        i += 1;
    }

    let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
    let target = mode.unwrap_or(!store.compiled().enabled);
    store
        .set_enabled(target)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let reloaded = notify_fcitx5_reload();
    println!(
        "SmartKey {} ({})",
        if target { "enabled" } else { "disabled" },
        if reloaded {
            "Fcitx5 reloaded"
        } else {
            "saved to rules.toml; will apply on next focus change"
        }
    );
    Ok(())
}

fn cmd_reload(args: &[String]) -> CliResult {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        println!(
            "USAGE:\n  smartkey reload\n\n\
             Ask the running Fcitx5 daemon to reload SmartKey configuration and dictionary packs."
        );
        return Ok(());
    }
    if let Some(other) = args.first() {
        return Err(CliError::Usage(format!(
            "unknown argument {other} (usage: smartkey reload)"
        )));
    }
    if notify_fcitx5_reload() {
        println!("Reloaded Fcitx5 SmartKey configuration.");
        Ok(())
    } else {
        Err(CliError::Runtime(
            "could not reach running Fcitx5 daemon (rules.toml still hot-reloads on window focus change)"
                .to_string(),
        ))
    }
}

fn cmd_init(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut preset = Preset::Balanced;
    let mut force = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey init [--preset balanced|chat_casual|developer_strict] [--dir <path>] [--force]\n\n\
                     Create ~/.config/smartkey/rules.toml from a starter preset."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            "--preset" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--preset requires a name".to_string()))?;
                preset = Preset::parse(raw).ok_or_else(|| {
                    CliError::Usage(format!(
                        "unknown preset {raw:?} (expected balanced, chat_casual, or developer_strict)"
                    ))
                })?;
            }
            "--force" => {
                force = true;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey init [--preset balanced|chat_casual|developer_strict] [--dir <path>] [--force])"
                )));
            }
        }
        i += 1;
    }

    let rules_path = dir.join("rules.toml");
    if rules_path.exists() && !force {
        return Err(CliError::Runtime(format!(
            "already initialized at {} (pass --force to overwrite with fresh preset)",
            rules_path.display()
        )));
    }
    if force && rules_path.exists() {
        fs::remove_file(&rules_path).map_err(|e| CliError::Runtime(e.to_string()))?;
    }
    let _ = load_rule_store_mut(&dir, preset)?;
    let _ = notify_fcitx5_reload();
    println!(
        "Initialized SmartKey rules ({preset:?}) at {}",
        rules_path.display()
    );
    Ok(())
}

fn cmd_rules(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey rules [--dir <path>]\n\n\
                     Show summary of personal dictionary, blocklist, snippets, and learned rules."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey rules [--dir <path>])"
                )));
            }
        }
        i += 1;
    }

    let (store, initialized) = load_rule_store_readonly(&dir, Preset::Balanced)?;
    let c = store.compiled();
    let st = store.state();
    let rules_label = if initialized {
        dir.join("rules.toml").display().to_string()
    } else {
        format!("{} (not initialized yet)", dir.join("rules.toml").display())
    };

    println!("=== SmartKey Personal Rule Brain ===");
    println!("Rules File           : {rules_label}");
    if let Some(parse_err) = store.parse_error() {
        println!("TOML Syntax Warning  : WRITE-LOCKED ({parse_err})");
    }
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
        MatchKind::ProperNoun => "proper_noun",
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
        MatchKind::VietnameseTelexRescue => "vietnamese_telex_rescue",
        MatchKind::ShorthandExpansion => "shorthand_expansion",
        MatchKind::PersonalDict => "personal_dict",
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

fn print_top_single(title: &str, map: &std::collections::BTreeMap<String, u32>, max_items: usize) {
    if map.is_empty() {
        return;
    }
    let mut pairs: Vec<(&String, &u32)> = map.iter().collect();
    pairs.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    println!("\n{title}:");
    for (word, count) in pairs.into_iter().take(max_items) {
        println!("  [{count}x] {word:?}");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogFilter {
    All,
    FixedOnly,
    ManualOnly,
    UnfixedOnly,
}

fn cmd_log(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut limit: usize = 50;
    let mut clear = false;
    let mut filter = LogFilter::All;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey log [fixed|manual|unfixed] [-n|--limit <N>] [--clear] [--dir <path>]\n\n\
                     Inspect autocorrect, backspace-undo, manual-fix, and missed-unknown diagnostic events.\n\
                     Durable JSONL event logs live in: {}",
                    smartkey_core::default_state_log_dir().display()
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            "--limit" | "-n" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--limit requires a number".to_string()))?;
                limit = raw.parse::<usize>().map_err(|_| {
                    CliError::Usage(format!("invalid --limit {raw:?} (expected integer)"))
                })?;
            }
            "--clear" => {
                clear = true;
            }
            "fixed" | "--fixed" | "--autocorrect" => {
                filter = LogFilter::FixedOnly;
            }
            "manual" | "--manual" => {
                filter = LogFilter::ManualOnly;
            }
            "unfixed" | "--unfixed" | "missed" => {
                filter = LogFilter::UnfixedOnly;
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown argument {other} (usage: smartkey log [fixed|manual|unfixed] [--limit <N>] [--clear] [--dir <path>])"
                )));
            }
        }
        i += 1;
    }

    let expected_state_path = dir.join("learner_state.json");
    if clear {
        let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
        store
            .clear_diagnostic_logs()
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let _ = notify_fcitx5_reload();
        println!(
            "Cleared SmartKey diagnostic logs at {}",
            expected_state_path.display()
        );
        return Ok(());
    }

    let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
    let state_label = if expected_state_path.exists() {
        expected_state_path.display().to_string()
    } else {
        format!("{} (no events recorded yet)", expected_state_path.display())
    };

    let st = store.state();
    let total_applied: u32 = st.applied_counts.values().copied().sum();
    let total_undos: u32 = st.undo_counts.values().copied().sum();
    let total_manual: u32 = st.manual_fix_counts.values().copied().sum();
    let mut unfixed_words: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();
    for (k, &v) in &st.untouched_unknowns {
        let bare = k.split_once(':').map(|(_, w)| w).unwrap_or(k.as_str());
        let entry = unfixed_words.entry(bare.to_string()).or_insert(0);
        *entry = (*entry).max(v);
    }
    let total_unfixed: u32 = unfixed_words.values().copied().sum();

    println!("=== SmartKey Diagnostic Log ({:?}) ===", filter);
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
    println!(
        "Unfixed Unknowns     : {} total ({} unique words)",
        total_unfixed,
        unfixed_words.len()
    );

    if filter == LogFilter::All || filter == LogFilter::ManualOnly {
        print_top_pairs("Backspace Undo Counts (1x / 2x / 3x+)", &st.undo_counts, 10);
        print_top_pairs(
            "Manual Backspace Fix Counts (1x / 2x / 3x+)",
            &st.manual_fix_counts,
            10,
        );
    }
    if filter == LogFilter::All || filter == LogFilter::FixedOnly {
        print_top_pairs("Top Autocorrects Applied", &st.applied_counts, 10);
    }
    if filter == LogFilter::All || filter == LogFilter::UnfixedOnly {
        print_top_single("Top Unfixed Unknown Words", &unfixed_words, 10);
    }

    if st.recent_events.is_empty() {
        println!("\nRecent Events: (none yet)");
        return Ok(());
    }

    let matching_events: Vec<&DiagnosticEvent> = st
        .recent_events
        .iter()
        .filter(|ev| match filter {
            LogFilter::All => true,
            LogFilter::FixedOnly => matches!(ev, DiagnosticEvent::AutocorrectApplied { .. }),
            LogFilter::ManualOnly => matches!(
                ev,
                DiagnosticEvent::ManualBackspaceFix { .. } | DiagnosticEvent::BackspaceUndo { .. }
            ),
            LogFilter::UnfixedOnly => matches!(ev, DiagnosticEvent::UnfixedUnknown { .. }),
        })
        .collect();

    if matching_events.is_empty() {
        println!("\nRecent Events: (no events matching filter {:?})", filter);
        return Ok(());
    }

    let total_events = matching_events.len();
    let start = total_events.saturating_sub(limit);
    let shown = &matching_events[start..];
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
                let tag = format!("[{total_count}x]");
                println!(
                    "  {tag:<6} AUTOCORRECT ({}) {typed:?} -> {applied:?}",
                    match_kind_slug(*kind)
                );
            }
            DiagnosticEvent::BackspaceUndo {
                typed,
                rejected,
                resolution,
                undo_count,
            } => {
                let tag = format!("[{undo_count}x]");
                println!(
                    "  {tag:<6} BACKSPACE_UNDO ({}) {typed:?} -> {rejected:?}",
                    undo_resolution_label(resolution)
                );
            }
            DiagnosticEvent::ManualBackspaceFix {
                orig,
                fixed,
                outcome,
                attempt_count,
            } => {
                let tag = format!("[{attempt_count}x]");
                println!("  {tag:<6} MANUAL_FIX ({outcome}) {orig:?} -> {fixed:?}");
            }
            DiagnosticEvent::UnfixedUnknown {
                word,
                best_candidate,
                best_score,
                threshold,
                total_count,
            } => {
                let tag = format!("[{total_count}x]");
                if let Some(cand) = best_candidate {
                    println!(
                        "  {tag:<6} UNFIXED {word:?} (closest: {cand:?} score={best_score:.2} < thresh={threshold:.2})"
                    );
                } else {
                    println!("  {tag:<6} UNFIXED {word:?} (no valid candidates)");
                }
            }
            DiagnosticEvent::RealWordDeclined {
                prev,
                word,
                candidate,
                candidate_score,
                typed_score,
            } => {
                println!(
                    "  {:<6} REAL_WORD_DECLINED prev={prev:?} {word:?} (candidate: {candidate:?} score={candidate_score:.2} vs typed={typed_score:.2})",
                    "[--]"
                );
            }
            DiagnosticEvent::AppBypass {
                app,
                profile,
                reason,
            } => {
                println!(
                    "  {:<6} APP_BYPASS app={app:?} profile={profile:?} reason={reason:?}",
                    "[--]"
                );
            }
            DiagnosticEvent::RuleLearned {
                rule,
                stage,
                previous,
            } => {
                println!(
                    "  {:<6} RULE_LEARNED ({stage}) {rule} (previous={previous:?})",
                    "[--]"
                );
            }
            DiagnosticEvent::WordUnchanged { word, reason } => {
                println!(
                    "  {:<6} WORD_UNCHANGED ({}) {word:?}",
                    "[--]",
                    match_kind_slug(*reason)
                );
            }
        }
    }

    Ok(())
}

fn parse_dialect_flag(raw: &str) -> Result<Dialect, CliError> {
    match raw.trim() {
        "mixed" | "Mixed" | "auto" => Ok(Dialect::Mixed),
        "us" | "en_US" | "en_us" => Ok(Dialect::EnUs),
        "gb" | "uk" | "en_GB" | "en_gb" => Ok(Dialect::EnGb),
        other => Err(CliError::Usage(format!(
            "unknown --dialect {other:?} (expected mixed, us, or gb)"
        ))),
    }
}

fn cmd_explain(args: &[String]) -> CliResult {
    let mut prev: Option<String> = None;
    let mut punct: Option<char> = None;
    let mut dialect: Option<Dialect> = None;
    let mut lang: LangId = LangId::en();
    let mut sentence_start = false;
    let mut as_json = false;
    let mut dir: Option<PathBuf> = None;
    let mut token: Option<String> = None;
    let mut end_of_flags = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if end_of_flags {
            if token.is_some() {
                return Err(CliError::Usage(format!(
                    "unexpected extra argument {arg:?} (usage: smartkey explain [FLAGS] <token>)"
                )));
            }
            token = Some(arg.to_string());
            i += 1;
            continue;
        }
        match arg {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey explain [--json] [--lang en|vi] [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>\n\n\
                     Evaluate a token against user rules, Vietnamese Telex rescue (with --lang vi), and the Layer 1 scorer."
                );
                return Ok(());
            }
            "--" => {
                end_of_flags = true;
            }
            "--prev" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--prev requires a word".to_string()))?;
                prev = Some(v.clone());
            }
            "--punct" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--punct requires a character".to_string()))?;
                let mut chars = raw.chars();
                let ch = chars
                    .next()
                    .ok_or_else(|| CliError::Usage("--punct requires a non-empty character".to_string()))?;
                if chars.next().is_some() {
                    return Err(CliError::Usage(format!(
                        "--punct expects a single character, got {raw:?}"
                    )));
                }
                punct = Some(ch);
            }
            "--dialect" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage("--dialect requires mixed, us, or gb".to_string())
                })?;
                dialect = Some(parse_dialect_flag(raw)?);
            }
            "--lang" | "-l" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage("--lang requires an id (en|vi)".to_string())
                })?;
                lang = match raw.as_str() {
                    "vi" | "vi-lotus" | "vi_lotus" | "lotus" => LangId::vi(),
                    "en" | "en_US" | "en_GB" => LangId::en(),
                    other => LangId(other.to_string()),
                };
            }
            "--json" => {
                as_json = true;
            }
            "--dir" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = Some(PathBuf::from(raw));
            }
            "--sentence-start" => {
                sentence_start = true;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey explain [--json] [--lang en|vi] [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>)"
                )));
            }
            other => {
                if token.is_some() {
                    return Err(CliError::Usage(format!(
                        "unexpected extra token {other:?} (explain accepts a single <token>)"
                    )));
                }
                token = Some(other.to_string());
            }
        }
        i += 1;
    }

    let Some(tok) = token else {
        return Err(CliError::Usage(
            "usage: smartkey explain [--json] [--lang en|vi] [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>"
                .to_string(),
        ));
    };

    let scorer = load_scorer()?;
    let cfg_dir = dir.unwrap_or_else(default_config_dir);
    let (mut store, _) = load_rule_store_readonly(&cfg_dir, Preset::Balanced)?;
    if let Some(d) = dialect {
        store.scorer_config_mut().dialect = d;
    }
    store.set_active_lang(lang);

    let engine = SmartKeyEngine::new(Some(scorer), store);
    let ctx = EvalContext {
        prev_word: prev.as_deref(),
        boundary_punct: punct,
        at_sentence_start: sentence_start,
        ..EvalContext::default()
    };
    let exp = engine.explain_token(&tok, &ctx);

    if as_json {
        println!("{}", serde_json::to_string_pretty(&exp).unwrap());
        return Ok(());
    }

    println!("=== SmartKey Explain: {:?} ===", exp.input);
    println!("Input Token     : {:?}", exp.input);
    if let Some(prev) = &exp.prev_word {
        println!("Previous Word   : {:?}", prev);
    }
    if let Some(punct) = exp.boundary_punct {
        println!("Boundary Punct  : {:?}", punct);
    }
    match &exp.outcome {
        EvalOutcome::Autocorrect {
            replacement,
            kind,
            score,
            margin,
        } => {
            println!(
                "Verdict         : AUTOCORRECT -> {:?} ({}, score={:.2}, margin={:.2})",
                replacement,
                match_kind_slug(*kind),
                score,
                margin
            );
        }
        EvalOutcome::Unchanged {
            reason,
            do_not_learn,
        } => {
            println!(
                "Verdict         : UNCHANGED ({}, do_not_learn={})",
                match_kind_slug(*reason),
                do_not_learn
            );
        }
    }
    let aggr = engine.store().scorer_config().aggressiveness;
    let (thresh_score, thresh_margin) = EnglishScorer::thresholds(aggr);
    println!(
        "Configuration   : dialect={:?} | aggressiveness={:?} (min_score >= {:.2}, margin >= {:.2})",
        engine.store().scorer_config().dialect,
        aggr,
        thresh_score,
        thresh_margin
    );
    if !exp.top_candidates.is_empty() {
        println!("\nTop Ranked Candidates ({}):", exp.top_candidates.len());
        for (idx, c) in exp.top_candidates.iter().take(5).enumerate() {
            println!(
                "  #{:<2} {:<16} score={:<6.2} edit={:<5.2} freq={:<4} [{}]",
                idx + 1,
                format!("{:?}", c.replacement),
                c.score,
                c.edit_cost,
                c.freq,
                match_kind_slug(c.kind)
            );
        }
    } else {
        println!("\nTop Ranked Candidates : (none)");
    }
    Ok(())
}

fn validate_and_install_skdict(staged_path: &Path, target_path: &Path) -> Result<(), CliError> {
    let bytes = fs::read(staged_path).map_err(|e| CliError::Runtime(e.to_string()))?;
    if let Err(err) = smartkey_core::EnglishLexicon::from_owned_bytes(bytes) {
        let _ = fs::remove_file(staged_path);
        return Err(CliError::Runtime(format!("invalid dictionary format: {err}")));
    }
    fs::rename(staged_path, target_path).map_err(|e| {
        let _ = fs::remove_file(staged_path);
        CliError::Runtime(e.to_string())
    })
}

fn cmd_dict(args: &[String]) -> CliResult {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!(
                "USAGE:\n  smartkey dict [list|dir|install <lang> <path_or_url>|remove <lang>|update [lang]]\n  Alias: dicts\n\n\
                 Inspect, install, update, or remove modular .skdict language dictionary packs."
            );
            Ok(())
        }
        Some("list" | "ls") | None => {
            if args.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (usage: smartkey dict list)",
                    args[1]
                )));
            }
            println!("=== SmartKey Language Packs ===");
            let user_dir = smartkey_core::default_user_dict_dir();
            println!("User Dict Directory   : {}", user_dir.display());
            println!("System Dict Directory : /usr/share/smartkey/dicts\n");

            let mut found = false;
            if user_dir.exists() {
                if let Ok(entries) = fs::read_dir(&user_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("skdict") {
                            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                            let meta = entry.metadata().ok();
                            let sz = meta.map(|m| m.len() as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
                            println!("  [user]   {:<6} ({:.2} MiB) -> {}", stem, sz, path.display());
                            found = true;
                        }
                    }
                }
            }
            for sys in ["/usr/share/smartkey/dicts", "/usr/local/share/smartkey/dicts"] {
                let sys_dir = Path::new(sys);
                if sys_dir.exists() {
                    if let Ok(entries) = fs::read_dir(sys_dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.extension().and_then(|s| s.to_str()) == Some("skdict") {
                                let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
                                let meta = entry.metadata().ok();
                                let sz =
                                    meta.map(|m| m.len() as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
                                println!(
                                    "  [system] {:<6} ({:.2} MiB) -> {}",
                                    stem,
                                    sz,
                                    path.display()
                                );
                                found = true;
                            }
                        }
                    }
                }
            }
            if !found {
                #[cfg(feature = "bundled-en")]
                println!("  [bundled] en (built into binary fallback)");
                #[cfg(not(feature = "bundled-en"))]
                println!("  (no language packs installed yet; install with `smartkey dict install en <path>`)");
            }
            Ok(())
        }
        Some("dir" | "path") => {
            if args.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (usage: smartkey dict dir)",
                    args[1]
                )));
            }
            println!("{}", smartkey_core::default_user_dict_dir().display());
            Ok(())
        }
        Some("install") => {
            if args.len() != 3 {
                return Err(CliError::Usage(
                    "usage: smartkey dict install <lang> <path_or_url>".to_string(),
                ));
            }
            let lang = &args[1];
            let source = &args[2];
            let user_dir = smartkey_core::default_user_dict_dir();
            fs::create_dir_all(&user_dir).map_err(|e| CliError::Runtime(e.to_string()))?;
            let target_path = user_dir.join(format!("{lang}.skdict"));
            let staged_path =
                user_dir.join(format!(".{lang}.skdict.tmp.{}", std::process::id()));

            if source.starts_with("http://") || source.starts_with("https://") {
                println!("Downloading {source}...");
                let status = Command::new("curl")
                    .args(["-fsSL", source, "-o", staged_path.to_str().unwrap()])
                    .status()
                    .map_err(|e| CliError::Runtime(format!("failed to run curl: {e}")))?;
                if !status.success() {
                    let _ = fs::remove_file(&staged_path);
                    return Err(CliError::Runtime(format!(
                        "curl download failed with status {status}"
                    )));
                }
            } else {
                let src_path = PathBuf::from(source);
                if !src_path.exists() {
                    return Err(CliError::Runtime(format!(
                        "source file does not exist: {}",
                        src_path.display()
                    )));
                }
                fs::copy(&src_path, &staged_path).map_err(|e| CliError::Runtime(e.to_string()))?;
            }

            validate_and_install_skdict(&staged_path, &target_path)?;

            let reloaded = notify_fcitx5_reload();
            println!(
                "Successfully installed {lang}.skdict to {} ({})",
                target_path.display(),
                if reloaded {
                    "Fcitx5 reloaded"
                } else {
                    "will apply on next focus change"
                }
            );
            Ok(())
        }
        Some("remove" | "rm" | "uninstall") => {
            if args.len() != 2 {
                return Err(CliError::Usage(
                    "usage: smartkey dict remove <lang>".to_string(),
                ));
            }
            let lang = &args[1];
            let user_dir = smartkey_core::default_user_dict_dir();
            let target_path = user_dir.join(format!("{lang}.skdict"));
            if target_path.exists() {
                fs::remove_file(&target_path).map_err(|e| CliError::Runtime(e.to_string()))?;
                let reloaded = notify_fcitx5_reload();
                println!(
                    "Removed {} ({})",
                    target_path.display(),
                    if reloaded {
                        "Fcitx5 reloaded"
                    } else {
                        "will apply on next focus change"
                    }
                );
                Ok(())
            } else {
                Err(CliError::Runtime(format!(
                    "no user dictionary pack found at {}",
                    target_path.display()
                )))
            }
        }
        Some("update") => {
            if args.len() > 2 {
                return Err(CliError::Usage(
                    "usage: smartkey dict update [lang]".to_string(),
                ));
            }
            let lang = args.get(1).map(String::as_str).unwrap_or("en");
            println!("Checking GitHub releases from segfawlt/smartkey-dicts for '{lang}'...");
            let url = format!(
                "https://github.com/segfawlt/smartkey-dicts/releases/latest/download/{lang}.skdict"
            );
            let user_dir = smartkey_core::default_user_dict_dir();
            fs::create_dir_all(&user_dir).map_err(|e| CliError::Runtime(e.to_string()))?;
            let target_path = user_dir.join(format!("{lang}.skdict"));
            let staged_path =
                user_dir.join(format!(".{lang}.skdict.tmp.{}", std::process::id()));

            let status = Command::new("curl")
                .args(["-fsSL", &url, "-o", staged_path.to_str().unwrap()])
                .status();
            match status {
                Ok(s) if s.success() => {
                    validate_and_install_skdict(&staged_path, &target_path)?;
                    let reloaded = notify_fcitx5_reload();
                    println!(
                        "Updated {lang}.skdict at {} ({})",
                        target_path.display(),
                        if reloaded {
                            "Fcitx5 reloaded"
                        } else {
                            "will apply on next focus change"
                        }
                    );
                    Ok(())
                }
                _ => {
                    let _ = fs::remove_file(&staged_path);
                    Err(CliError::Runtime(format!(
                        "could not download from {url} (you can manually place {lang}.skdict in {})",
                        user_dir.display()
                    )))
                }
            }
        }
        Some(other) => Err(CliError::Usage(format!(
            "unknown dict subcommand {other:?} (usage: smartkey dict [list|dir|install|remove|update])"
        ))),
    }
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

fn canonical_plugin_display(key: &str) -> &str {
    match key {
        "vi_lotus" => "vi-lotus",
        other => other,
    }
}

fn cmd_plugin(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut pos: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey plugin [list|enable|disable|toggle] [vi-lotus] [--dir <path>]\n  Alias: plugins\n\n\
                     Inspect or toggle modular input-method bridge plugins."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey plugin [list|enable|disable|toggle] [vi-lotus] [--dir <path>])"
                )));
            }
            other => pos.push(other),
        }
        i += 1;
    }

    let action = pos.first().copied().unwrap_or("list");
    match action {
        "list" | "ls" => {
            if pos.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (usage: smartkey plugin list [--dir <path>])",
                    pos[1]
                )));
            }
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let c = store.compiled();
            let lotus_so = detect_fcitx5_module_so("liblotus.so");
            let host_label = match lotus_so {
                Some(p) => format!("detected ({})", p.display()),
                None => "fcitx5-lotus not detected".to_string(),
            };
            println!("=== SmartKey Modular Plugins ===");
            println!(
                "  {:<10} : {:<8} | host={} | Zero-fork Vietnamese Telex typo rescue & bilingual bridge for fcitx5-lotus",
                "vi-lotus",
                if c.vi_lotus_enabled {
                    "ENABLED"
                } else {
                    "DISABLED"
                },
                host_label
            );
            Ok(())
        }
        "enable" | "on" | "install" | "disable" | "off" | "remove" | "toggle" => {
            if pos.len() > 2 {
                return Err(CliError::Usage(format!(
                    "unexpected extra argument {:?} (usage: smartkey plugin {action} [vi-lotus] [--dir <path>])",
                    pos[2]
                )));
            }
            let raw_name = pos.get(1).copied().unwrap_or("vi-lotus");
            let canon_key = RuleStore::normalize_plugin_key(raw_name).ok_or_else(|| {
                CliError::Usage(format!(
                    "unknown plugin {raw_name:?} (available plugins: vi-lotus)"
                ))
            })?;
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let cur = store.is_plugin_enabled(canon_key).unwrap_or(false);
            let target = match action {
                "enable" | "on" | "install" => true,
                "disable" | "off" | "remove" => false,
                _ => !cur,
            };
            store
                .set_plugin_enabled(canon_key, target)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            let reloaded = notify_fcitx5_reload();
            let display_name = canonical_plugin_display(canon_key);
            println!(
                "Plugin '{display_name}' {} ({})",
                if target { "enabled" } else { "disabled" },
                if reloaded {
                    "Fcitx5 reloaded"
                } else {
                    "saved to rules.toml; will apply on next focus change"
                }
            );
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown plugin subcommand {other:?} (usage: smartkey plugin [list|enable|disable|toggle] [vi-lotus] [--dir <path>])"
        ))),
    }
}

fn cmd_repl(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut in_memory = false;
    let mut lang = LangId::en();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  smartkey repl [--lang en|vi] [--dir <path>] [--in-memory]\n\n\
                     Interactive terminal typing and live backspace-learning sandbox."
                );
                return Ok(());
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            "--in-memory" => {
                in_memory = true;
            }
            "--lang" | "-l" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage("--lang requires an id (en|vi)".to_string())
                })?;
                lang = match raw.as_str() {
                    "vi" | "vi-lotus" | "vi_lotus" | "lotus" => LangId::vi(),
                    other => LangId(other.to_string()),
                };
            }
            other => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (usage: smartkey repl [--lang en|vi] [--dir <path>] [--in-memory])"
                )));
            }
        }
        i += 1;
    }

    let scorer = load_scorer()?;
    let store = if in_memory {
        RuleStore::in_memory(Preset::Balanced)
    } else {
        let mut s = load_rule_store_mut(&dir, Preset::Balanced)?;
        s.set_log_dir(Some(smartkey_core::default_state_log_dir()));
        s
    };

    let rules_label = store
        .rules_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<in-memory>".to_string());

    let mut engine = SmartKeyEngine::new(Some(scorer), store);
    let mut session = SmartKeySession::new();
    session.set_lang(lang);
    session.set_app(Some("smartkey-repl"));
    let t0 = Instant::now();

    println!("SmartKey Interactive REPL — Layer 1 + Layer 2 Learning Active");
    println!("Rules brain: {rules_label}  (Ctrl-C or Esc to quit, Ctrl-W for word-backspace)\n");

    let _guard = RawTermGuard::enter()
        .map_err(|e| CliError::Runtime(format!("REPL requires an interactive TTY ({e})")))?;

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
        if stdin
            .read(&mut byte_buf[..1])
            .map_err(|e| CliError::Runtime(e.to_string()))?
            == 0
        {
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
                    .map_err(|e| CliError::Runtime(e.to_string()))?;
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
smartkey {} ({GIT_VERSION}) — Control & inspection CLI for SmartKey (aliased as smartkeyctl)

USAGE:
  smartkey <COMMAND> [OPTIONS]

COMMANDS:
  status [--json] [--dir <path>]                         Show daemon, addon, IM-env & personal brain status
  enable|on [--dir <path>]                               Enable SmartKey and reload Fcitx5
  disable|off [--dir <path>]                             Disable SmartKey and reload Fcitx5
  toggle [--dir <path>]                                  Toggle SmartKey on/off (ideal for WM keybinds)
  reload                                                 Reload configuration & dictionary packs in Fcitx5
  log [fixed|manual|unfixed] [-n <N>] [--clear] [--dir]  Show autocorrects, undos & missed unknown words
  rules [--dir <path>]                                   Show personal dictionary, blocklist & learned rules
  explain [--json] [--lang en|vi] [--prev <w>] [--punct <c>] [--dialect mixed|us|gb] [--sentence-start] [--dir <path>] <token>
                                                         Explain scoring, proper nouns & candidate ranking
  plugin|plugins [list|enable|disable|toggle] [vi-lotus] [--dir <path>]
                                                         Manage modular input-method & language bridge plugins
  dict|dicts [list|dir|install <lang> <src>|remove <lang>|update [lang]]
                                                         Inspect and manage modular language dictionary packs
  init [--preset <name>] [--dir <path>] [--force]        Initialize ~/.config/smartkey/rules.toml
  repl [--lang en|vi] [--dir <path>] [--in-memory]       Interactive terminal typing & learning sandbox

PATHS:
  Config & brain : ~/.config/smartkey/rules.toml
  Event logs     : ~/.local/state/smartkey/logs/events-YYYY-MM-DD.jsonl
  Pass `<COMMAND> --help` for per-command options. Exit codes: 0 ok, 1 runtime error, 2 usage error.",
        env!("CARGO_PKG_VERSION")
    );
}

fn run_cli(args: &[String]) -> CliResult {
    match args.first().map(String::as_str) {
        Some("status") => cmd_status(&args[1..]),
        Some("enable" | "on") => cmd_set_enabled(&args[1..], Some(true)),
        Some("disable" | "off") => cmd_set_enabled(&args[1..], Some(false)),
        Some("toggle") => cmd_set_enabled(&args[1..], None),
        Some("reload") => cmd_reload(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
        Some("rules") => cmd_rules(&args[1..]),
        Some("log") => cmd_log(&args[1..]),
        Some("explain") => cmd_explain(&args[1..]),
        Some("plugin" | "plugins") => cmd_plugin(&args[1..]),
        Some("dict" | "dicts") => cmd_dict(&args[1..]),
        Some("repl") => cmd_repl(&args[1..]),
        Some("--version" | "-V") => {
            println!("smartkey {} ({GIT_VERSION})", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        Some(other) => Err(CliError::Usage(format!(
            "unknown command {other:?} (run `smartkey --help` for usage)"
        ))),
        None => {
            print_help();
            std::process::exit(2);
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Err(err) = run_cli(&args) {
        match err {
            CliError::Usage(msg) => {
                eprintln!("error: {msg}");
                std::process::exit(2);
            }
            CliError::Runtime(msg) => {
                eprintln!("error: {msg}");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn cli_bug_regressions_b1_through_b8_and_readonly_status() {
        // B1: unknown flag in `explain` must be Usage error (not treated as token)
        assert!(matches!(
            run_cli(&vec_args(&["explain", "--jso"])),
            Err(CliError::Usage(_))
        ));

        // B2: missing `--prev` / `--punct` values must be Usage error
        assert!(matches!(
            run_cli(&vec_args(&["explain", "--prev"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            run_cli(&vec_args(&["explain", "--punct"])),
            Err(CliError::Usage(_))
        ));

        // B3: invalid `--dialect` must be Usage error instead of silently coercing to Mixed
        assert!(matches!(
            run_cli(&vec_args(&["explain", "--dialect", "usx", "teh"])),
            Err(CliError::Usage(_))
        ));

        // B4: `reload --foo` must reject unknown arguments
        assert!(matches!(
            run_cli(&vec_args(&["reload", "--foo"])),
            Err(CliError::Usage(_))
        ));

        // B5: `plugin` rejects unknown extra positional arguments
        assert!(matches!(
            run_cli(&vec_args(&["plugin", "enable", "vi-lotus", "extra"])),
            Err(CliError::Usage(_))
        ));

        // Per-command `--help` exits Ok(())
        for sub in [
            "status", "enable", "reload", "init", "rules", "log", "explain", "plugin", "dict",
            "repl",
        ] {
            assert_eq!(run_cli(&vec_args(&[sub, "--help"])), Ok(()));
        }

        // Read-only `status` / `rules` / `log` must NOT create config directories when missing
        let uninit_dir = env::temp_dir().join(format!(
            "smartkey_cli_readonly_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&uninit_dir);
        let dir_str = uninit_dir.to_str().unwrap();
        assert_eq!(run_cli(&vec_args(&["status", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["rules", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["log", "--dir", dir_str])), Ok(()));
        assert!(
            !uninit_dir.exists(),
            "read-only inspection commands must not create config dir on disk"
        );
    }
}
