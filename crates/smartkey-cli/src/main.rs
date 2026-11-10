use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use smartkey_core::event_log::current_epoch_ms;
use smartkey_core::{
    apply_key_to_buffer, DiagnosticEvent, Dialect, EnglishScorer, EvalContext, EvalOutcome,
    EventLog, KeyInput, LangId, LoggedDiagnosticRecord, MatchKind, Preset, RuleStore,
    SessionAction, SmartKeyEngine, SmartKeySession, UndoResolution,
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

fn map_store_err(err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::InvalidInput {
        CliError::Usage(err.to_string())
    } else {
        CliError::Runtime(err.to_string())
    }
}

fn parse_lang_arg(raw: &str) -> Result<LangId, CliError> {
    match raw.trim() {
        "en" | "en_US" | "en_GB" => Ok(LangId::en()),
        "vi" | "vi-lotus" | "vi_lotus" | "lotus" => Ok(LangId::vi()),
        other if !other.is_empty() && other.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') => {
            Ok(LangId(other.to_string()))
        }
        other => Err(CliError::Usage(format!(
            "invalid --lang {other:?} (expected en or vi)"
        ))),
    }
}

#[derive(Serialize)]
struct RulesJson {
    rules_path: String,
    initialized: bool,
    parse_locked: bool,
    personal_dict: Vec<String>,
    lang_personal_dict: std::collections::BTreeMap<String, Vec<String>>,
    corrections: std::collections::BTreeMap<String, String>,
    lang_corrections: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    context_corrections: std::collections::BTreeMap<String, String>,
    snippets: std::collections::BTreeMap<String, String>,
    blocklist: Vec<(String, String)>,
    telex_passthrough: Vec<String>,
}

fn print_rules_help() {
    println!(
        "USAGE:\n  \
         smartkey rules [summary|list] [--json] [--dir <path>]\n  \
         smartkey rules add fix <typed> <replacement> [--lang en|vi] [--dir <path>]\n  \
         smartkey rules add context <prev> <typed> <replacement> [--dir <path>]\n  \
         smartkey rules add snippet <trigger> <expansion> [--dir <path>]\n  \
         smartkey rules add word <word> [--lang en|vi] [--dir <path>]\n  \
         smartkey rules add block <typed> <rejected> [--dir <path>]\n  \
         smartkey rules add passthrough <ascii_word> [--dir <path>]\n  \
         smartkey rules remove <fix|context|snippet|word|block|passthrough> ... [--lang en|vi] [--dir <path>]\n\n\
         Inspect or edit personal dictionary words, corrections, snippets, blocklist pairs, and Telex passthrough rules."
    );
}

fn cmd_rules(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut lang = LangId::en();
    let mut as_json = false;
    let mut pos: Vec<&str> = Vec::new();
    let mut end_of_flags = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if end_of_flags {
            pos.push(arg);
            i += 1;
            continue;
        }
        match arg {
            "--help" | "-h" => {
                print_rules_help();
                return Ok(());
            }
            "--" => {
                end_of_flags = true;
            }
            "--dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--dir requires a path".to_string()))?;
                dir = PathBuf::from(p);
            }
            "--lang" | "-l" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--lang requires an id (en|vi)".to_string()))?;
                lang = parse_lang_arg(raw)?;
            }
            "--json" => {
                as_json = true;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (run `smartkey rules --help` for usage)"
                )));
            }
            other => pos.push(other),
        }
        i += 1;
    }

    let action = pos.first().copied().unwrap_or("summary");
    match action {
        "summary" | "list" | "ls" => {
            if pos.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (run `smartkey rules --help` for usage)",
                    pos[1]
                )));
            }
            let (store, initialized) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let c = store.compiled();
            let st = store.state();
            let expected_rules_path = dir.join("rules.toml");

            if as_json {
                let mut personal_words: Vec<String> = c
                    .personal_dict
                    .iter()
                    .map(|w| {
                        c.personal_dict_canonical
                            .get(w)
                            .cloned()
                            .unwrap_or_else(|| w.clone())
                    })
                    .collect();
                personal_words.sort();

                let mut lang_personal_dict = std::collections::BTreeMap::new();
                for (lk, set) in &c.lang_personal_dict {
                    let canon_map = c.lang_personal_dict_canonical.get(lk);
                    let mut words: Vec<String> = set
                        .iter()
                        .map(|w| {
                            canon_map
                                .and_then(|m| m.get(w))
                                .cloned()
                                .unwrap_or_else(|| w.clone())
                        })
                        .collect();
                    words.sort();
                    lang_personal_dict.insert(lk.clone(), words);
                }

                let context_corrections: std::collections::BTreeMap<String, String> = c
                    .context_corrections
                    .iter()
                    .map(|((p, t), v)| (format!("{p} {t}"), v.clone()))
                    .collect();

                let payload = RulesJson {
                    rules_path: expected_rules_path.display().to_string(),
                    initialized,
                    parse_locked: store.is_parse_locked(),
                    personal_dict: personal_words,
                    lang_personal_dict,
                    corrections: c.corrections.clone(),
                    lang_corrections: c.lang_corrections.clone(),
                    context_corrections,
                    snippets: c.snippets.clone(),
                    blocklist: c.blocklist.iter().cloned().collect(),
                    telex_passthrough: c.telex_passthrough.iter().cloned().collect(),
                };
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                return Ok(());
            }

            let rules_label = if initialized {
                expected_rules_path.display().to_string()
            } else {
                format!("{} (not initialized yet)", expected_rules_path.display())
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

            if action == "list" || action == "ls" {
                if !c.corrections.is_empty() {
                    println!("\nCorrections [en] ({}):", c.corrections.len());
                    for (k, v) in &c.corrections {
                        println!("  {k:?} -> {v:?}");
                    }
                }
                for (lk, map) in &c.lang_corrections {
                    if !map.is_empty() {
                        println!("\nCorrections [{lk}] ({}):", map.len());
                        for (k, v) in map {
                            println!("  {k:?} -> {v:?}");
                        }
                    }
                }
                if !c.context_corrections.is_empty() {
                    println!("\nContext Corrections ({}):", c.context_corrections.len());
                    for ((prev, typed), v) in &c.context_corrections {
                        println!("  \"{prev} {typed}\" -> {v:?}");
                    }
                }
                if !c.snippets.is_empty() {
                    println!("\nSnippets ({}):", c.snippets.len());
                    for (k, v) in &c.snippets {
                        println!("  {k:?} -> {v:?}");
                    }
                }
                if !c.personal_dict.is_empty() {
                    let words: Vec<&str> = c
                        .personal_dict
                        .iter()
                        .map(|w| {
                            c.personal_dict_canonical
                                .get(w)
                                .map(String::as_str)
                                .unwrap_or(w.as_str())
                        })
                        .collect();
                    println!("\nPersonal Words [en] ({}): {}", words.len(), words.join(", "));
                }
                for (lk, set) in &c.lang_personal_dict {
                    if !set.is_empty() {
                        let canon_map = c.lang_personal_dict_canonical.get(lk);
                        let words: Vec<&str> = set
                            .iter()
                            .map(|w| {
                                canon_map
                                    .and_then(|m| m.get(w))
                                    .map(String::as_str)
                                    .unwrap_or(w.as_str())
                            })
                            .collect();
                        println!("\nPersonal Words [{lk}] ({}): {}", words.len(), words.join(", "));
                    }
                }
                if !c.blocklist.is_empty() {
                    println!("\nBlocklist ({}):", c.blocklist.len());
                    for (t, r) in &c.blocklist {
                        println!("  typed={t:?} rejected={r:?}");
                    }
                }
                if !c.telex_passthrough.is_empty() {
                    let words: Vec<&str> = c.telex_passthrough.iter().map(String::as_str).collect();
                    println!("\nTelex Passthrough ({}): {}", words.len(), words.join(", "));
                }
            } else if !st.skipped_edits.is_empty() {
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
        "add" => {
            let Some(&kind) = pos.get(1) else {
                return Err(CliError::Usage(
                    "usage: smartkey rules add <fix|context|snippet|word|block|passthrough> ..."
                        .to_string(),
                ));
            };
            let rest = &pos[2..];
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let summary = match kind {
                "fix" | "correction" => {
                    if rest.len() != 2 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add fix <typed> <replacement> [--lang en|vi]"
                                .to_string(),
                        ));
                    }
                    store
                        .set_user_correction(&lang, rest[0], rest[1])
                        .map_err(map_store_err)?;
                    format!(
                        "Saved correction [{}]: {:?} -> {:?}",
                        lang.as_str(),
                        rest[0].trim().to_lowercase(),
                        rest[1].trim()
                    )
                }
                "context" => {
                    if rest.len() != 3 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add context <prev> <typed> <replacement>"
                                .to_string(),
                        ));
                    }
                    store
                        .set_user_context_correction(rest[0], rest[1], rest[2])
                        .map_err(map_store_err)?;
                    format!(
                        "Saved context correction: \"{} {}\" -> {:?}",
                        rest[0].trim().to_lowercase(),
                        rest[1].trim().to_lowercase(),
                        rest[2].trim()
                    )
                }
                "snippet" => {
                    if rest.len() != 2 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add snippet <trigger> <expansion>".to_string(),
                        ));
                    }
                    store
                        .set_snippet(rest[0], rest[1])
                        .map_err(map_store_err)?;
                    format!("Saved snippet: {:?} -> {:?}", rest[0].trim(), rest[1])
                }
                "word" | "personal" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add word <word> [--lang en|vi]".to_string(),
                        ));
                    }
                    let changed = store
                        .add_user_personal_word(&lang, rest[0])
                        .map_err(map_store_err)?;
                    if changed {
                        format!(
                            "Added personal word [{}]: {:?}",
                            lang.as_str(),
                            rest[0].trim()
                        )
                    } else {
                        format!(
                            "Personal word [{}] already present: {:?}",
                            lang.as_str(),
                            rest[0].trim()
                        )
                    }
                }
                "block" | "blocklist" => {
                    if rest.len() != 2 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add block <typed> <rejected>".to_string(),
                        ));
                    }
                    let changed = store
                        .add_user_blocklist_pair(rest[0], rest[1])
                        .map_err(map_store_err)?;
                    if changed {
                        format!(
                            "Blocked pair: {:?} -> {:?}",
                            rest[0].trim().to_lowercase(),
                            rest[1].trim().to_lowercase()
                        )
                    } else {
                        format!(
                            "Blocklist pair already present: {:?} -> {:?}",
                            rest[0].trim().to_lowercase(),
                            rest[1].trim().to_lowercase()
                        )
                    }
                }
                "passthrough" | "telex" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add passthrough <ascii_word>".to_string(),
                        ));
                    }
                    let changed = store
                        .add_user_telex_passthrough(rest[0])
                        .map_err(map_store_err)?;
                    if changed {
                        format!(
                            "Added Telex passthrough word: {:?}",
                            rest[0].trim().to_lowercase()
                        )
                    } else {
                        format!(
                            "Telex passthrough word already present: {:?}",
                            rest[0].trim().to_lowercase()
                        )
                    }
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown rules add target {other:?} (expected fix, context, snippet, word, block, or passthrough)"
                    )));
                }
            };
            let _ = notify_fcitx5_reload();
            println!("{summary}");
            Ok(())
        }
        "remove" | "rm" | "del" | "delete" => {
            let Some(&kind) = pos.get(1) else {
                return Err(CliError::Usage(
                    "usage: smartkey rules remove <fix|context|snippet|word|block|passthrough> ..."
                        .to_string(),
                ));
            };
            let rest = &pos[2..];
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let (removed, desc) = match kind {
                "fix" | "correction" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove fix <typed> [--lang en|vi]".to_string(),
                        ));
                    }
                    let r = store
                        .remove_correction(&lang, rest[0])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!(
                            "correction [{}] {:?}",
                            lang.as_str(),
                            rest[0].trim().to_lowercase()
                        ),
                    )
                }
                "context" => {
                    if rest.len() != 2 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove context <prev> <typed>".to_string(),
                        ));
                    }
                    let r = store
                        .remove_context_correction(rest[0], rest[1])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!(
                            "context correction \"{} {}\"",
                            rest[0].trim().to_lowercase(),
                            rest[1].trim().to_lowercase()
                        ),
                    )
                }
                "snippet" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove snippet <trigger>".to_string(),
                        ));
                    }
                    let r = store.remove_snippet(rest[0]).map_err(map_store_err)?;
                    (r, format!("snippet {:?}", rest[0].trim()))
                }
                "word" | "personal" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove word <word> [--lang en|vi]".to_string(),
                        ));
                    }
                    let r = store
                        .remove_user_personal_word(&lang, rest[0])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!(
                            "personal word [{}] {:?}",
                            lang.as_str(),
                            rest[0].trim().to_lowercase()
                        ),
                    )
                }
                "block" | "blocklist" => {
                    if rest.len() != 2 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove block <typed> <rejected>".to_string(),
                        ));
                    }
                    let r = store
                        .remove_blocklist_pair(rest[0], rest[1])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!(
                            "blocklist pair {:?} -> {:?}",
                            rest[0].trim().to_lowercase(),
                            rest[1].trim().to_lowercase()
                        ),
                    )
                }
                "passthrough" | "telex" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove passthrough <ascii_word>".to_string(),
                        ));
                    }
                    let r = store
                        .remove_telex_passthrough(rest[0])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!(
                            "Telex passthrough word {:?}",
                            rest[0].trim().to_lowercase()
                        ),
                    )
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "unknown rules remove target {other:?} (expected fix, context, snippet, word, block, or passthrough)"
                    )));
                }
            };
            if !removed {
                return Err(CliError::Runtime(format!("no matching {desc} found")));
            }
            let _ = notify_fcitx5_reload();
            println!("Removed {desc}");
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown rules subcommand {other:?} (run `smartkey rules --help` for usage)"
        ))),
    }
}

fn cmd_config(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut as_json = false;
    let mut pos: Vec<&str> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  \
                     smartkey config [list] [--json] [--dir <path>]\n  \
                     smartkey config get <key> [--json] [--dir <path>]\n  \
                     smartkey config set <key> <value> [--dir <path>]\n\n\
                     Inspect or update [settings], [plugins], [diagnostics], and [app_profiles] keys in rules.toml."
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
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (run `smartkey config --help` for usage)"
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
                    "unexpected argument {:?} (usage: smartkey config list [--json] [--dir <path>])",
                    pos[1]
                )));
            }
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let items = store.list_settings();
            if as_json {
                let map: std::collections::BTreeMap<&str, String> = items.into_iter().collect();
                println!("{}", serde_json::to_string_pretty(&map).unwrap());
                return Ok(());
            }
            for (k, v) in items {
                println!("{k} = {v}");
            }
            Ok(())
        }
        "get" => {
            if pos.len() != 2 {
                return Err(CliError::Usage(
                    "usage: smartkey config get <key> [--json] [--dir <path>]".to_string(),
                ));
            }
            let key = pos[1];
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let canon = RuleStore::normalize_setting_key(key).ok_or_else(|| {
                CliError::Usage(format!(
                    "unknown config key {key:?} (run `smartkey config list` to see valid keys)"
                ))
            })?;
            let val = store.get_setting(canon).unwrap_or_default();
            if as_json {
                let payload = serde_json::json!({ "key": canon, "value": val });
                println!("{}", serde_json::to_string(&payload).unwrap());
            } else {
                println!("{val}");
            }
            Ok(())
        }
        "set" => {
            if pos.len() != 3 {
                return Err(CliError::Usage(
                    "usage: smartkey config set <key> <value> [--dir <path>]".to_string(),
                ));
            }
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let (canon, effective) = store
                .set_setting(pos[1], pos[2])
                .map_err(map_store_err)?;
            let reloaded = notify_fcitx5_reload();
            println!(
                "Updated {canon} = {effective} ({})",
                if reloaded {
                    "Fcitx5 reloaded"
                } else {
                    "saved to rules.toml; will apply on next focus change"
                }
            );
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown config subcommand {other:?} (usage: smartkey config [list|get|set])"
        ))),
    }
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

impl LogFilter {
    fn matches(self, ev: &DiagnosticEvent) -> bool {
        match self {
            Self::All => true,
            Self::FixedOnly => matches!(ev, DiagnosticEvent::AutocorrectApplied { .. }),
            Self::ManualOnly => matches!(
                ev,
                DiagnosticEvent::ManualBackspaceFix { .. } | DiagnosticEvent::BackspaceUndo { .. }
            ),
            Self::UnfixedOnly => matches!(ev, DiagnosticEvent::UnfixedUnknown { .. }),
        }
    }
}

fn local_midnight_epoch_ms(now_ms: u64, day_offset: i32) -> u64 {
    let secs: libc::time_t = (now_ms / 1000) as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_mday = tm.tm_mday.saturating_add(day_offset);
        tm.tm_isdst = -1;
        let midnight = libc::mktime(&mut tm);
        if midnight < 0 {
            0
        } else {
            (midnight as u64) * 1000
        }
    }
}

fn parse_local_date_midnight_ms(ymd: &str) -> Option<u64> {
    let mut parts = ymd.trim().split('-');
    let y_str = parts.next()?;
    let m_str = parts.next()?;
    let d_str = parts.next()?;
    if parts.next().is_some() || y_str.len() != 4 || m_str.len() != 2 || d_str.len() != 2 {
        return None;
    }
    let year: i32 = y_str.parse().ok()?;
    let month: i32 = m_str.parse().ok()?;
    let day: i32 = d_str.parse().ok()?;
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        tm.tm_year = year - 1900;
        tm.tm_mon = month - 1;
        tm.tm_mday = day;
        tm.tm_hour = 0;
        tm.tm_min = 0;
        tm.tm_sec = 0;
        tm.tm_isdst = -1;
        let ts = libc::mktime(&mut tm);
        if ts < 0
            || tm.tm_year != year - 1900
            || tm.tm_mon != month - 1
            || tm.tm_mday != day
        {
            return None;
        }
        Some((ts as u64) * 1000)
    }
}

fn parse_since_spec(raw: &str, now_ms: u64) -> Result<u64, CliError> {
    let trimmed = raw.trim();
    if let Some(ms) = parse_local_date_midnight_ms(trimmed) {
        return Ok(ms);
    }
    if trimmed.len() >= 2 {
        let (num_part, unit_part) = trimmed.split_at(trimmed.len() - 1);
        if let Ok(n) = num_part.parse::<u64>() {
            let mult_ms = match unit_part {
                "s" | "S" => Some(1_000u64),
                "m" | "M" => Some(60_000u64),
                "h" | "H" => Some(3_600_000u64),
                "d" | "D" => Some(86_400_000u64),
                "w" | "W" => Some(7 * 86_400_000u64),
                _ => None,
            };
            if let Some(unit) = mult_ms {
                return Ok(now_ms.saturating_sub(n.saturating_mul(unit)));
            }
        }
    }
    Err(CliError::Usage(format!(
        "invalid --since {raw:?} (expected relative duration like 30m, 2h, 7d or date YYYY-MM-DD)"
    )))
}

fn format_local_timestamp(ts_ms: u64) -> String {
    let secs: libc::time_t = (ts_ms / 1000) as libc::time_t;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

fn format_diagnostic_event_line(ev: &DiagnosticEvent) -> String {
    match ev {
        DiagnosticEvent::AutocorrectApplied {
            typed,
            applied,
            kind,
            total_count,
        } => {
            let tag = format!("[{total_count}x]");
            format!(
                "{tag:<6} AUTOCORRECT ({}) {typed:?} -> {applied:?}",
                match_kind_slug(*kind)
            )
        }
        DiagnosticEvent::BackspaceUndo {
            typed,
            rejected,
            resolution,
            undo_count,
        } => {
            let tag = format!("[{undo_count}x]");
            format!(
                "{tag:<6} BACKSPACE_UNDO ({}) {typed:?} -> {rejected:?}",
                undo_resolution_label(resolution)
            )
        }
        DiagnosticEvent::ManualBackspaceFix {
            orig,
            fixed,
            outcome,
            attempt_count,
        } => {
            let tag = format!("[{attempt_count}x]");
            format!("{tag:<6} MANUAL_FIX ({outcome}) {orig:?} -> {fixed:?}")
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
                format!(
                    "{tag:<6} UNFIXED {word:?} (closest: {cand:?} score={best_score:.2} < thresh={threshold:.2})"
                )
            } else {
                format!("{tag:<6} UNFIXED {word:?} (no valid candidates)")
            }
        }
        DiagnosticEvent::RealWordDeclined {
            prev,
            word,
            candidate,
            candidate_score,
            typed_score,
        } => format!(
            "{:<6} REAL_WORD_DECLINED prev={prev:?} {word:?} (candidate: {candidate:?} score={candidate_score:.2} vs typed={typed_score:.2})",
            "[--]"
        ),
        DiagnosticEvent::AppBypass {
            app,
            profile,
            reason,
        } => format!(
            "{:<6} APP_BYPASS app={app:?} profile={profile:?} reason={reason:?}",
            "[--]"
        ),
        DiagnosticEvent::RuleLearned {
            rule,
            stage,
            previous,
        } => format!(
            "{:<6} RULE_LEARNED ({stage}) {rule} (previous={previous:?})",
            "[--]"
        ),
        DiagnosticEvent::WordUnchanged { word, reason } => format!(
            "{:<6} WORD_UNCHANGED ({}) {word:?}",
            "[--]",
            match_kind_slug(*reason)
        ),
    }
}

fn cmd_log(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut log_dir: Option<PathBuf> = None;
    let mut limit: usize = 50;
    let mut clear = false;
    let mut as_json = false;
    let mut today = false;
    let mut yesterday = false;
    let mut since_raw: Option<String> = None;
    let mut grep_pat: Option<String> = None;
    let mut filter = LogFilter::All;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => {
                println!(
                    "USAGE:\n  \
                     smartkey log [fixed|manual|unfixed] [--today|--yesterday|--since <spec>] [--grep <substr>] [--json] [-n|--limit <N>] [--clear] [--dir <path>] [--log-dir <path>]\n\n\
                     Inspect autocorrect, backspace-undo, manual-fix, and missed-unknown diagnostic events.\n\
                     `--since` accepts relative windows (`30m`, `2h`, `7d`) or local dates (`YYYY-MM-DD`).\n\
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
            "--log-dir" => {
                i += 1;
                let p = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--log-dir requires a path".to_string()))?;
                log_dir = Some(PathBuf::from(p));
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
            "--since" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage("--since requires a duration (e.g. 2h, 7d) or YYYY-MM-DD".to_string())
                })?;
                since_raw = Some(raw.clone());
            }
            "--grep" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--grep requires a pattern".to_string()))?;
                grep_pat = Some(raw.to_lowercase());
            }
            "--today" => {
                today = true;
            }
            "--yesterday" => {
                yesterday = true;
            }
            "--json" => {
                as_json = true;
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
                    "unknown argument {other} (run `smartkey log --help` for usage)"
                )));
            }
        }
        i += 1;
    }

    let time_flags_count = usize::from(today) + usize::from(yesterday) + usize::from(since_raw.is_some());
    if time_flags_count > 1 {
        return Err(CliError::Usage(
            "--today, --yesterday, and --since are mutually exclusive".to_string(),
        ));
    }

    let effective_log_dir = log_dir
        .clone()
        .unwrap_or_else(smartkey_core::default_state_log_dir);
    let expected_state_path = dir.join("learner_state.json");

    if clear {
        if expected_state_path.exists() || dir.join("rules.toml").exists() {
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            store
                .clear_diagnostic_logs()
                .map_err(|e| CliError::Runtime(e.to_string()))?;
        }
        let removed_jsonl = EventLog::clear_all_files(&effective_log_dir)
            .map_err(|e| CliError::Runtime(e.to_string()))?;
        let _ = notify_fcitx5_reload();
        println!(
            "Cleared SmartKey diagnostic logs ({} and {removed_jsonl} JSONL day file(s) in {})",
            expected_state_path.display(),
            effective_log_dir.display()
        );
        return Ok(());
    }

    let query_jsonl = today
        || yesterday
        || since_raw.is_some()
        || grep_pat.is_some()
        || as_json
        || log_dir.is_some();

    if query_jsonl {
        let now_ms = current_epoch_ms();
        let (start_ms, end_ms) = if today {
            (Some(local_midnight_epoch_ms(now_ms, 0)), None)
        } else if yesterday {
            (
                Some(local_midnight_epoch_ms(now_ms, -1)),
                Some(local_midnight_epoch_ms(now_ms, 0)),
            )
        } else if let Some(ref raw_since) = since_raw {
            (Some(parse_since_spec(raw_since, now_ms)?), None)
        } else {
            (None, None)
        };

        let records = EventLog::read_records_in_range(&effective_log_dir, start_ms, end_ms)
            .map_err(|e| CliError::Runtime(e.to_string()))?;

        let matching: Vec<&LoggedDiagnosticRecord> = records
            .iter()
            .filter(|rec| filter.matches(&rec.event))
            .filter(|rec| {
                let Some(ref pat) = grep_pat else {
                    return true;
                };
                rec.raw_line.to_lowercase().contains(pat)
                    || format_diagnostic_event_line(&rec.event)
                        .to_lowercase()
                        .contains(pat)
            })
            .collect();

        // Fallback when bare `smartkey log --json` is called on a machine with ring-buffer events
        // in `learner_state.json` before any JSONL file was written.
        if matching.is_empty()
            && as_json
            && !today
            && !yesterday
            && since_raw.is_none()
            && grep_pat.is_none()
            && log_dir.is_none()
            && expected_state_path.exists()
        {
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let ring: Vec<&DiagnosticEvent> = store
                .state()
                .recent_events
                .iter()
                .filter(|ev| filter.matches(ev))
                .collect();
            let start = ring.len().saturating_sub(limit);
            for ev in &ring[start..] {
                println!("{}", serde_json::to_string(ev).unwrap());
            }
            return Ok(());
        }

        let total = matching.len();
        let start = total.saturating_sub(limit);
        let shown = &matching[start..];

        if as_json {
            for rec in shown {
                println!("{}", rec.raw_line);
            }
            return Ok(());
        }

        println!("=== SmartKey Event Log ({:?}) ===", filter);
        println!("Log Directory        : {}", effective_log_dir.display());
        println!(
            "Matching Events      : {} total (showing {}, oldest -> newest)",
            total,
            shown.len()
        );
        if shown.is_empty() {
            println!("\n(no matching events in selected window)");
            return Ok(());
        }
        println!();
        for rec in shown {
            let ts_str = format_local_timestamp(rec.ts_ms);
            let app_tag = rec.app.as_deref().unwrap_or("-");
            println!(
                "  {ts_str}  ({:<4} {:<14})  {}",
                rec.lang,
                app_tag,
                format_diagnostic_event_line(&rec.event)
            );
        }
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
    println!("JSONL Event Log Dir  : {}", effective_log_dir.display());
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
        .filter(|ev| filter.matches(ev))
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
        println!("  {}", format_diagnostic_event_line(ev));
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
  log [fixed|manual|unfixed] [--today|--yesterday|--since <spec>] [--grep <p>] [--json] [-n <N>] [--clear]
                                                         Query diagnostic counters & durable JSONL event logs
  rules [list|add|remove] ... [--json] [--dir <path>]    Inspect or edit personal words, rules, snippets & blocks
  config [list|get <key>|set <key> <val>] [--json]       Inspect or update settings & diagnostics in rules.toml
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
        Some("config" | "cfg") => cmd_config(&args[1..]),
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
    use smartkey_core::DiagnosticsConfig;

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
            "status", "enable", "reload", "init", "rules", "config", "log", "explain", "plugin",
            "dict", "repl",
        ] {
            assert_eq!(run_cli(&vec_args(&[sub, "--help"])), Ok(()));
        }

        // Read-only `status` / `rules` / `config` / `log` must NOT create config directories when missing
        let uninit_dir = env::temp_dir().join(format!(
            "smartkey_cli_readonly_{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&uninit_dir);
        let dir_str = uninit_dir.to_str().unwrap();
        assert_eq!(run_cli(&vec_args(&["status", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["rules", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["config", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["log", "--dir", dir_str])), Ok(()));
        assert!(
            !uninit_dir.exists(),
            "read-only inspection commands must not create config dir on disk"
        );
    }

    #[test]
    fn cli_rules_crud_and_config_preserve_comments_and_respect_parse_lock() {
        let dir = env::temp_dir().join(format!(
            "smartkey_cli_crud_{}_{}",
            std::process::id(),
            current_epoch_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules_path = dir.join("rules.toml");
        fs::write(
            &rules_path,
            "# Custom user header comment\n[settings]\nenabled = true\n",
        )
        .unwrap();
        let d = dir.to_str().unwrap();

        // Add & remove across all 6 rule collections
        assert_eq!(
            run_cli(&vec_args(&["rules", "add", "fix", "teh", "the", "--dir", d])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "add", "context", "in", "teh", "the", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules",
                "add",
                "snippet",
                "addr;",
                "123 Main St",
                "--dir",
                d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "add", "word", "FastAPI", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "add", "word", "bánh", "--lang", "vi", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "add", "block", "form", "from", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules",
                "add",
                "passthrough",
                "was",
                "--dir",
                d
            ])),
            Ok(())
        );

        // Config set/get
        assert_eq!(
            run_cli(&vec_args(&[
                "config",
                "set",
                "aggressiveness",
                "conservative",
                "--dir",
                d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "config",
                "set",
                "diagnostics.keep_days",
                "30",
                "--dir",
                d
            ])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&[
                "config",
                "set",
                "aggressiveness",
                "ultra",
                "--dir",
                d
            ])),
            Err(CliError::Usage(_))
        ));

        let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced).unwrap();
        assert_eq!(
            store.compiled().corrections.get("teh").map(String::as_str),
            Some("the")
        );
        assert_eq!(
            store
                .compiled()
                .context_corrections
                .get(&("in".to_string(), "teh".to_string()))
                .map(String::as_str),
            Some("the")
        );
        assert_eq!(
            store.compiled().snippets.get("addr;").map(String::as_str),
            Some("123 Main St")
        );
        assert_eq!(
            store
                .compiled()
                .personal_dict_canonical
                .get("fastapi")
                .map(String::as_str),
            Some("FastAPI")
        );
        assert!(store.is_lang_only_personal_word("vi", "bánh"));
        assert!(store
            .compiled()
            .blocklist
            .contains(&("form".to_string(), "from".to_string())));
        assert!(store.compiled().telex_passthrough.contains("was"));
        assert_eq!(
            store.get_setting("aggressiveness").as_deref(),
            Some("conservative")
        );
        assert_eq!(
            store.get_setting("diagnostics.keep_days").as_deref(),
            Some("30")
        );

        let raw_after_adds = fs::read_to_string(&rules_path).unwrap();
        assert!(
            raw_after_adds.contains("# Custom user header comment"),
            "CLI CRUD and config set must preserve user TOML comments"
        );
        assert!(
            !raw_after_adds.contains("# learned 2x"),
            "explicit CLI rules must not stamp machine-learned provenance comments"
        );

        // Remove rules and verify missing entry returns Runtime error
        assert_eq!(
            run_cli(&vec_args(&["rules", "remove", "fix", "teh", "--dir", d])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&["rules", "remove", "fix", "teh", "--dir", d])),
            Err(CliError::Runtime(_))
        ));
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "remove", "context", "in", "teh", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "remove", "snippet", "addr;", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "remove", "word", "FastAPI", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "remove", "block", "form", "from", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules",
                "remove",
                "passthrough",
                "was",
                "--dir",
                d
            ])),
            Ok(())
        );

        // Corrupting `rules.toml` must lock writes and fail CLI mutations with Runtime error
        fs::write(&rules_path, "[settings\nenabled = false\n").unwrap();
        assert!(matches!(
            run_cli(&vec_args(&[
                "rules", "add", "word", "Hyprland", "--dir", d
            ])),
            Err(CliError::Runtime(_))
        ));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_log_jsonl_utc_boundary_queries_and_clear() {
        let base = env::temp_dir().join(format!(
            "smartkey_cli_jsonl_{}_{}",
            std::process::id(),
            current_epoch_ms()
        ));
        let cfg_dir = base.join("cfg");
        let log_dir = base.join("logs");
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        let mut elog = EventLog::new();
        elog.set_dir(Some(log_dir.clone()));
        elog.set_app(Some("firefox"));
        let diag = DiagnosticsConfig::default();

        let now = current_epoch_ms();
        let today_local_midnight = local_midnight_epoch_ms(now, 0);
        let yesterday_local_midnight = local_midnight_epoch_ms(now, -1);

        // Stamp an event 5 seconds after local midnight today (in UTC+ timezones like +07:00,
        // this lands in yesterday's UTC `events-YYYY-MM-DD.jsonl` file!).
        elog.record_at(
            &diag,
            "en",
            &DiagnosticEvent::AutocorrectApplied {
                typed: "teh".to_string(),
                applied: "the".to_string(),
                kind: MatchKind::KnownTypoDict,
                total_count: 1,
            },
            today_local_midnight + 5_000,
        );
        // Stamp a second event in yesterday's local window.
        elog.record_at(
            &diag,
            "vi",
            &DiagnosticEvent::AutocorrectApplied {
                typed: "nuowcss".to_string(),
                applied: "nước".to_string(),
                kind: MatchKind::VietnameseTelexRescue,
                total_count: 1,
            },
            yesterday_local_midnight + 3_600_000,
        );

        let today_records =
            EventLog::read_records_in_range(&log_dir, Some(today_local_midnight), None).unwrap();
        assert_eq!(
            today_records.len(),
            1,
            "--today range must capture events right after local midnight even when stored in prior UTC day file"
        );
        assert_eq!(today_records[0].app.as_deref(), Some("firefox"));

        let yesterday_records = EventLog::read_records_in_range(
            &log_dir,
            Some(yesterday_local_midnight),
            Some(today_local_midnight),
        )
        .unwrap();
        assert_eq!(yesterday_records.len(), 1);
        assert_eq!(yesterday_records[0].lang, "vi");

        let cfg_str = cfg_dir.to_str().unwrap();
        let log_str = log_dir.to_str().unwrap();
        assert_eq!(
            run_cli(&vec_args(&[
                "log",
                "--today",
                "--grep",
                "teh",
                "--json",
                "--dir",
                cfg_str,
                "--log-dir",
                log_str
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "log",
                "--since",
                "48h",
                "--dir",
                cfg_str,
                "--log-dir",
                log_str
            ])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&["log", "--today", "--yesterday"])),
            Err(CliError::Usage(_))
        ));

        // `--clear` must delete durable `events-*.jsonl` files too!
        assert_eq!(
            run_cli(&vec_args(&[
                "log",
                "--clear",
                "--dir",
                cfg_str,
                "--log-dir",
                log_str
            ])),
            Ok(())
        );
        let remaining = EventLog::read_records_in_range(&log_dir, None, None).unwrap();
        assert!(
            remaining.is_empty(),
            "`smartkey log --clear` must delete durable JSONL event files"
        );

        let _ = fs::remove_dir_all(&base);
    }
}
