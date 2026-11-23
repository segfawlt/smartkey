use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use smartkey_core::event_log::current_epoch_ms;
use smartkey_core::{
    apply_key_to_buffer, normalize_program_id, peek_skdict_header_and_manifest, AppProfile,
    ConfidenceTier, DiagnosticEvent, Dialect, EvalContext, EvalOutcome, EventLog, KeyInput, LangId,
    LatinScorer, LexiconError, LoggedDiagnosticRecord, MatchKind, PackHeader, PackLexicon,
    PackTier, Preset, PruneKinds, PruneOptions, RuleStore, SessionAction, SmartKeyEngine,
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
    trace_log: bool,
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
            trace_log: c.diagnostics.trace_log,
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
        "Trace Recorder       : {}",
        if c.diagnostics.trace_log {
            "ENABLED (flight-recorder active)"
        } else {
            "disabled (run `smartkey trace on` to record)"
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
        other
            if !other.is_empty()
                && other
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
        {
            Ok(LangId(other.to_string()))
        }
        other => Err(CliError::Usage(format!(
            "invalid --lang {other:?} (expected en or vi)"
        ))),
    }
}

#[derive(Serialize)]
struct ConfidenceCountsJson {
    manual: usize,
    confirmed: usize,
    provisional: usize,
    suspect: usize,
}

#[derive(Serialize)]
struct ConfidenceRuleJson {
    kind: &'static str,
    key: String,
    display: String,
    tier: &'static str,
    origin: &'static str,
    hits: u32,
    strikes: u32,
    created_epoch_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_hit_epoch_ms: Option<u64>,
}

#[derive(Serialize)]
struct StagedEntryJson {
    queue: &'static str,
    key: String,
    display: String,
    count: u32,
    threshold: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    canonical: Option<String>,
}

#[derive(Serialize)]
struct StagedJson {
    rules_path: String,
    initialized: bool,
    total: usize,
    staged: Vec<StagedEntryJson>,
}

#[derive(Serialize)]
struct RulesJson {
    rules_path: String,
    initialized: bool,
    parse_locked: bool,
    personal_dict: Vec<String>,
    lang_personal_dict: std::collections::BTreeMap<String, Vec<String>>,
    corrections: std::collections::BTreeMap<String, String>,
    lang_corrections:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    context_corrections: std::collections::BTreeMap<String, String>,
    snippets: std::collections::BTreeMap<String, String>,
    blocklist: Vec<(String, String)>,
    telex_passthrough: Vec<String>,
    confidence_counts: ConfidenceCountsJson,
    confidence: Vec<ConfidenceRuleJson>,
}

fn parse_confidence_tier(raw: &str) -> Result<ConfidenceTier, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "suspect" | "suspicious" => Ok(ConfidenceTier::Suspect),
        "provisional" => Ok(ConfidenceTier::Provisional),
        "confirmed" => Ok(ConfidenceTier::Confirmed),
        "manual" => Ok(ConfidenceTier::Manual),
        other => Err(CliError::Usage(format!(
            "invalid --tier {other:?} (expected suspect, provisional, confirmed, or manual)"
        ))),
    }
}

fn parse_prune_kinds(raw: &str) -> Result<PruneKinds, CliError> {
    let mut kinds = PruneKinds::none();
    let mut any = false;
    for part in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        any = true;
        match part.to_ascii_lowercase().as_str() {
            "fix" | "fixes" => kinds.fix = true,
            "context" | "ctx" => kinds.context = true,
            "word" | "words" | "personal" => kinds.word = true,
            "block" | "blocks" | "blocklist" => kinds.block = true,
            "passthrough" | "pt" | "telex" => kinds.passthrough = true,
            "all" => kinds = PruneKinds::all(),
            other => {
                return Err(CliError::Usage(format!(
                    "invalid --kind token {other:?} (expected fix, context, word, block, passthrough, or all)"
                )));
            }
        }
    }
    if !any {
        return Err(CliError::Usage(
            "--kind requires at least one of fix, context, word, block, passthrough, or all"
                .to_string(),
        ));
    }
    Ok(kinds)
}

fn format_age_ago(now_ms: u64, ts_ms: u64) -> String {
    if ts_ms == 0 {
        return "untracked".to_string();
    }
    let diff_s = now_ms.saturating_sub(ts_ms) / 1000;
    if diff_s < 60 {
        format!("{diff_s}s")
    } else if diff_s < 3600 {
        format!("{}m", diff_s / 60)
    } else if diff_s < 86_400 {
        format!("{}h", diff_s / 3600)
    } else {
        format!("{}d", diff_s / 86_400)
    }
}

fn tier_sort_rank(tier: &str) -> u8 {
    match tier {
        "suspect" => 0,
        "provisional" => 1,
        "confirmed" => 2,
        _ => 3,
    }
}

fn collect_rule_confidence(store: &RuleStore) -> (ConfidenceCountsJson, Vec<ConfidenceRuleJson>) {
    let c = store.compiled();
    let mut raw: Vec<(&'static str, String, String, bool)> = Vec::new();

    for (typed, fixed) in &c.corrections {
        raw.push((
            "fix",
            RuleStore::stat_key_fix("en", typed),
            format!("{typed} -> {fixed}"),
            false,
        ));
    }
    for (lk, map) in &c.lang_corrections {
        for (typed, fixed) in map {
            let display = if lk == "en" {
                format!("{typed} -> {fixed}")
            } else {
                format!("[{lk}] {typed} -> {fixed}")
            };
            raw.push(("fix", RuleStore::stat_key_fix(lk, typed), display, false));
        }
    }
    for ((prev, typed), fixed) in &c.context_corrections {
        raw.push((
            "context",
            RuleStore::stat_key_ctx(prev, typed),
            format!("{prev} {typed} -> {fixed}"),
            false,
        ));
    }
    for w in &c.personal_dict {
        let canon = c
            .personal_dict_canonical
            .get(w)
            .map(String::as_str)
            .unwrap_or(w.as_str());
        raw.push((
            "word",
            RuleStore::stat_key_word("en", w),
            canon.to_string(),
            c.pinned.contains(w),
        ));
    }
    for (lk, set) in &c.lang_personal_dict {
        if lk == "en" {
            continue;
        }
        let canon_map = c.lang_personal_dict_canonical.get(lk);
        for w in set {
            let canon = canon_map
                .and_then(|m| m.get(w))
                .map(String::as_str)
                .unwrap_or(w.as_str());
            raw.push((
                "word",
                RuleStore::stat_key_word(lk, w),
                format!("[{lk}] {canon}"),
                false,
            ));
        }
    }
    for (t, r) in &c.blocklist {
        raw.push((
            "block",
            RuleStore::stat_key_block(t, r),
            format!("{t} !-> {r}"),
            false,
        ));
    }
    for w in &c.telex_passthrough {
        raw.push(("passthrough", RuleStore::stat_key_pt(w), w.clone(), false));
    }

    let mut counts = ConfidenceCountsJson {
        manual: 0,
        confirmed: 0,
        provisional: 0,
        suspect: 0,
    };
    let mut items = Vec::with_capacity(raw.len());
    for (kind, key, display, is_pinned) in raw {
        let (tier, stat, hits, strikes) = store.classify_rule_confidence(&key);
        match tier {
            ConfidenceTier::Manual => counts.manual += 1,
            ConfidenceTier::Confirmed => counts.confirmed += 1,
            ConfidenceTier::Provisional => counts.provisional += 1,
            ConfidenceTier::Suspect => counts.suspect += 1,
        }
        let origin = if is_pinned {
            "pinned"
        } else {
            stat.map(|s| s.origin.as_str()).unwrap_or("untracked")
        };
        items.push(ConfidenceRuleJson {
            kind,
            key,
            display,
            tier: tier.as_str(),
            origin,
            hits,
            strikes,
            created_epoch_ms: stat.map(|s| s.created_epoch_ms).unwrap_or(0),
            last_hit_epoch_ms: stat.and_then(|s| s.last_hit_epoch_ms),
        });
    }
    items.sort_by(|a, b| {
        tier_sort_rank(a.tier)
            .cmp(&tier_sort_rank(b.tier))
            .then_with(|| a.kind.cmp(b.kind))
            .then_with(|| a.display.cmp(&b.display))
    });
    (counts, items)
}

fn collect_staged_entries(st: &smartkey_core::LearnerState) -> Vec<StagedEntryJson> {
    let mut staged = Vec::new();
    for (k, &count) in &st.staged_corrections {
        if let Some(rest) = k.strip_prefix("case:") {
            let display = rest
                .split_once(':')
                .map(|(l, w)| format!("[{l}] {w}"))
                .unwrap_or_else(|| rest.to_string());
            staged.push(StagedEntryJson {
                queue: "case_fix",
                key: k.clone(),
                display,
                count,
                threshold: 2,
                canonical: None,
            });
        } else {
            let display = k
                .split_once(':')
                .map(|(l, pair)| format!("[{l}] {pair}"))
                .unwrap_or_else(|| k.clone());
            staged.push(StagedEntryJson {
                queue: "fix",
                key: k.clone(),
                display,
                count,
                threshold: 2,
                canonical: None,
            });
        }
    }
    for (k, &count) in &st.staged_context {
        staged.push(StagedEntryJson {
            queue: "context",
            key: k.clone(),
            display: format!("[en] {}", k.replace('|', " ")),
            count,
            threshold: 2,
            canonical: None,
        });
    }
    for (k, &count) in &st.untouched_unknowns {
        if let Some(rest) = k.strip_prefix("case_upgrade:") {
            let display = rest
                .split_once(':')
                .map(|(l, w)| format!("[{l}] {w}"))
                .unwrap_or_else(|| rest.to_string());
            staged.push(StagedEntryJson {
                queue: "case_upgrade",
                key: k.clone(),
                display,
                count,
                threshold: 2,
                canonical: None,
            });
        } else if let Some((l, w)) = k.split_once(':') {
            // Skip bare diagnostic hit counters from `log_unfixed_unknown`; only show lang-prefixed staging queues.
            let canon = st.staged_personal_canonical.get(k).cloned();
            let display = match &canon {
                Some(c) => format!("[{l}] {c}"),
                None => format!("[{l}] {w}"),
            };
            staged.push(StagedEntryJson {
                queue: "untouched_word",
                key: k.clone(),
                display,
                count,
                threshold: 3,
                canonical: canon,
            });
        }
    }
    for (k, &count) in &st.staged_undo_keep {
        if let Some(rest) = k.strip_prefix("demote:") {
            let display = rest
                .split_once(':')
                .map(|(l, w)| format!("[{l}] {w}"))
                .unwrap_or_else(|| rest.to_string());
            staged.push(StagedEntryJson {
                queue: "personal_demote",
                key: k.clone(),
                display,
                count,
                threshold: 2,
                canonical: None,
            });
        } else {
            let display = k
                .split_once(':')
                .map(|(l, pair)| format!("[{l}] {pair}"))
                .unwrap_or_else(|| k.clone());
            staged.push(StagedEntryJson {
                queue: "undo_keep",
                key: k.clone(),
                display,
                count,
                threshold: 2,
                canonical: None,
            });
        }
    }
    for (k, &count) in &st.staged_contraction_rejects {
        let display = k
            .split_once(':')
            .map(|(l, pair)| format!("[{l}] {pair}"))
            .unwrap_or_else(|| k.clone());
        staged.push(StagedEntryJson {
            queue: "contraction_reject",
            key: k.clone(),
            display,
            count,
            threshold: 2,
            canonical: None,
        });
    }
    for (k, &count) in &st.staged_telex_passthrough {
        staged.push(StagedEntryJson {
            queue: "telex_passthrough",
            key: k.clone(),
            display: format!("[vi] {k}"),
            count,
            threshold: 2,
            canonical: None,
        });
    }
    staged
}

fn print_confidence_line(item: &ConfidenceRuleJson, now_ms: u64) {
    let ref_ts = item.last_hit_epoch_ms.unwrap_or(item.created_epoch_ms);
    let age_label = if item.last_hit_epoch_ms.is_some() {
        format!("last hit {} ago", format_age_ago(now_ms, ref_ts))
    } else if item.created_epoch_ms > 0 {
        format!("created {} ago", format_age_ago(now_ms, ref_ts))
    } else {
        "untracked".to_string()
    };
    let strike_note = if item.strikes > 0 {
        format!(", strikes={}", item.strikes)
    } else {
        String::new()
    };
    println!(
        "  [{tier:<11}] {kind:<11} {display}  (origin={origin}, hits={hits}{strike_note}, {age_label})",
        tier = item.tier,
        kind = item.kind,
        display = item.display,
        origin = item.origin,
        hits = item.hits
    );
}

fn print_rules_help() {
    println!(
        "USAGE:\n  \
         smartkey rules [summary] [--json] [--dir <path>]\n  \
         smartkey rules list [--suspect | --tier <suspect|provisional|confirmed|manual>] [--json] [--dir <path>]\n  \
         smartkey rules staged [--json] [--dir <path>]\n  \
         smartkey rules prune [--older-than <dur>] [--zero-hits | --max-hits <N>] [--kind <list>] [--include-manual] [--dry-run] [--json] [--dir <path>]\n  \
         smartkey rules add fix <typed> <replacement> [--lang en|vi] [--dir <path>]\n  \
         smartkey rules add context <prev> <typed> <replacement> [--dir <path>]\n  \
         smartkey rules add snippet <trigger> <expansion> [--dir <path>]\n  \
         smartkey rules add word <word> [--lang en|vi] [--dir <path>]\n  \
         smartkey rules add block <typed> <rejected> [--dir <path>]\n  \
         smartkey rules add passthrough <ascii_word> [--dir <path>]\n  \
         smartkey rules remove <fix|context|snippet|word|block|passthrough> ... [--lang en|vi] [--dir <path>]\n\n\
         Inspect, audit confidence, inspect pending staged queues, prune stale learned rules, or edit personal rules."
    );
}

fn cmd_rules(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut lang = LangId::en();
    let mut as_json = false;
    let mut tier_filter: Option<ConfidenceTier> = None;
    let mut older_than_ms: u64 = 30 * 86_400_000;
    let mut max_hits: u32 = 0;
    let mut kinds = PruneKinds::default();
    let mut include_manual = false;
    let mut dry_run = false;
    let mut used_prune_flag = false;
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
            "--suspect" | "--suspicious" => {
                tier_filter = Some(ConfidenceTier::Suspect);
            }
            "--tier" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--tier requires a tier name".to_string()))?;
                tier_filter = Some(parse_confidence_tier(raw)?);
            }
            "--older-than" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage("--older-than requires a duration (e.g. 30d, 12h)".to_string())
                })?;
                older_than_ms = parse_duration_ms(raw)
                    .map_err(|e| CliError::Usage(format!("invalid --older-than {raw:?}: {e}")))?;
                used_prune_flag = true;
            }
            "--zero-hits" => {
                max_hits = 0;
                used_prune_flag = true;
            }
            "--max-hits" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--max-hits requires a number".to_string()))?;
                max_hits = raw.trim().parse::<u32>().map_err(|_| {
                    CliError::Usage(format!("invalid --max-hits {raw:?} (expected integer)"))
                })?;
                used_prune_flag = true;
            }
            "--kind" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--kind requires rule kind(s)".to_string()))?;
                kinds = parse_prune_kinds(raw)?;
                used_prune_flag = true;
            }
            "--include-manual" => {
                include_manual = true;
                used_prune_flag = true;
            }
            "--dry-run" => {
                dry_run = true;
                used_prune_flag = true;
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
    if action != "prune" && used_prune_flag {
        return Err(CliError::Usage(
            "prune flags (--older-than, --zero-hits, --max-hits, --kind, --include-manual, --dry-run) require `smartkey rules prune`"
                .to_string(),
        ));
    }
    if !matches!(action, "list" | "ls") && tier_filter.is_some() {
        return Err(CliError::Usage(
            "--suspect / --tier flags require `smartkey rules list`".to_string(),
        ));
    }

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
            let now_ms = current_epoch_ms();
            let (confidence_counts, all_confidence) = collect_rule_confidence(&store);
            let filtered_confidence: Vec<ConfidenceRuleJson> = match tier_filter {
                Some(t) => all_confidence
                    .into_iter()
                    .filter(|item| item.tier == t.as_str())
                    .collect(),
                None => all_confidence,
            };

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
                    confidence_counts,
                    confidence: filtered_confidence,
                };
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                return Ok(());
            }

            if let Some(t) = tier_filter {
                println!(
                    "=== Rules Matching Confidence Tier: {} ({} found) ===",
                    t.as_str(),
                    filtered_confidence.len()
                );
                println!(
                    "Totals: {} manual, {} confirmed, {} provisional, {} suspect",
                    confidence_counts.manual,
                    confidence_counts.confirmed,
                    confidence_counts.provisional,
                    confidence_counts.suspect
                );
                if filtered_confidence.is_empty() {
                    println!("No rules match tier `{}`.", t.as_str());
                } else {
                    for item in &filtered_confidence {
                        print_confidence_line(item, now_ms);
                    }
                }
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
            println!(
                "Rule Confidence      : {} manual, {} confirmed, {} provisional, {} suspect",
                confidence_counts.manual,
                confidence_counts.confirmed,
                confidence_counts.provisional,
                confidence_counts.suspect
            );
            if confidence_counts.suspect > 0 {
                println!(
                    "Tip: run `smartkey rules list --suspect` to review low-confidence or disputed rules."
                );
            }

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
                    println!(
                        "\nPersonal Words [en] ({}): {}",
                        words.len(),
                        words.join(", ")
                    );
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
                        println!(
                            "\nPersonal Words [{lk}] ({}): {}",
                            words.len(),
                            words.join(", ")
                        );
                    }
                }
                if !c.blocklist.is_empty() {
                    println!("\nBlocklist ({}):", c.blocklist.len());
                    for (t, r) in &c.blocklist {
                        println!("  typed={t:?} rejected={r:?}");
                    }
                }
                if !c.pinned.is_empty() {
                    let words: Vec<&str> = c.pinned.iter().map(String::as_str).collect();
                    println!("\nPinned ({}): {}", words.len(), words.join(", "));
                }
                if !c.telex_passthrough.is_empty() {
                    let words: Vec<&str> = c.telex_passthrough.iter().map(String::as_str).collect();
                    println!(
                        "\nTelex Passthrough ({}): {}",
                        words.len(),
                        words.join(", ")
                    );
                }
                println!("\n=== Confidence Breakdown ===");
                println!(
                    "  manual={} | confirmed={} | provisional={} | suspect={}",
                    confidence_counts.manual,
                    confidence_counts.confirmed,
                    confidence_counts.provisional,
                    confidence_counts.suspect
                );
                let review_items: Vec<&ConfidenceRuleJson> = filtered_confidence
                    .iter()
                    .filter(|it| matches!(it.tier, "suspect" | "provisional"))
                    .collect();
                if review_items.is_empty() {
                    println!("  No suspect or provisional learned rules.");
                } else {
                    for item in review_items {
                        print_confidence_line(item, now_ms);
                    }
                }
            } else if !st.skipped_edits.is_empty() {
                println!(
                    "\nRecent Guard-Rejected Edits ({}):",
                    st.skipped_edits.len()
                );
                for entry in st.skipped_edits.iter().rev().take(10) {
                    println!(
                        "  [{}x] ({}) prev={:?} {:?} -> {:?} ({})",
                        entry.count,
                        entry.lang,
                        entry.prev_word,
                        entry.orig,
                        entry.fixed,
                        entry.reason
                    );
                }
            }
            Ok(())
        }
        "staged" => {
            if pos.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (run `smartkey rules --help` for usage)",
                    pos[1]
                )));
            }
            let (store, initialized) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let staged = collect_staged_entries(store.state());
            let rules_path = dir.join("rules.toml");
            if as_json {
                let payload = StagedJson {
                    rules_path: rules_path.display().to_string(),
                    initialized,
                    total: staged.len(),
                    staged,
                };
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                return Ok(());
            }
            println!("=== Staged Rule Candidates ({} pending) ===", staged.len());
            if staged.is_empty() {
                println!("No staged candidates.");
            } else {
                for item in &staged {
                    println!(
                        "  [{count}/{threshold}] {queue:<18} {display}",
                        count = item.count,
                        threshold = item.threshold,
                        queue = item.queue,
                        display = item.display
                    );
                }
            }
            Ok(())
        }
        "prune" => {
            if pos.len() > 1 {
                return Err(CliError::Usage(format!(
                    "unexpected argument {:?} (run `smartkey rules --help` for usage)",
                    pos[1]
                )));
            }
            let rules_path = dir.join("rules.toml");
            let state_path = dir.join("learner_state.json");
            let mut store = RuleStore::load_or_init(rules_path, state_path, Preset::Balanced)
                .map_err(map_store_err)?;
            let now_ms = current_epoch_ms();
            let opts = PruneOptions {
                older_than_ms,
                max_hits,
                kinds,
                include_manual,
                dry_run,
            };
            let report = store.prune_rules(now_ms, &opts).map_err(map_store_err)?;
            if !report.dry_run && report.removed_count > 0 {
                let _ = notify_fcitx5_reload();
            }
            if as_json {
                println!("{}", serde_json::to_string_pretty(&report).unwrap());
                return Ok(());
            }
            let mode_label = if report.dry_run {
                "Dry-run: would remove"
            } else {
                "Removed"
            };
            println!(
                "{mode_label} {} stale rule(s) ({} untracked hand-edited rule(s) spared).",
                report.matched.len(),
                report.skipped_untracked
            );
            for cand in &report.matched {
                println!(
                    "  [{tier:<11}] {kind:<11} {display}  (origin={origin}, hits={hits})",
                    tier = cand.tier.as_str(),
                    kind = cand.kind,
                    display = cand.display,
                    origin = cand.origin.as_str(),
                    hits = cand.hits
                );
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
                    store.set_snippet(rest[0], rest[1]).map_err(map_store_err)?;
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
                "pin" | "pinned" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules add pin <word>".to_string(),
                        ));
                    }
                    let changed = store.add_user_pinned_word(rest[0]).map_err(map_store_err)?;
                    if changed {
                        format!(
                            "Pinned {:?}: its curated capitalization always applies",
                            rest[0].trim().to_lowercase()
                        )
                    } else {
                        format!("Already pinned: {:?}", rest[0].trim().to_lowercase())
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
                "pin" | "pinned" => {
                    if rest.len() != 1 {
                        return Err(CliError::Usage(
                            "usage: smartkey rules remove pin <word>".to_string(),
                        ));
                    }
                    let r = store
                        .remove_user_pinned_word(rest[0])
                        .map_err(map_store_err)?;
                    (
                        r,
                        format!("pinned word {:?}", rest[0].trim().to_lowercase()),
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
                        format!("Telex passthrough word {:?}", rest[0].trim().to_lowercase()),
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
            let (canon, effective) = store.set_setting(pos[1], pos[2]).map_err(map_store_err)?;
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
        MatchKind::SpaceShift => "space_shift",
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
        if ts < 0 || tm.tm_year != year - 1900 || tm.tm_mon != month - 1 || tm.tm_mday != day {
            return None;
        }
        Some((ts as u64) * 1000)
    }
}

fn parse_duration_ms(raw: &str) -> Result<u64, String> {
    let trimmed = raw.trim();
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
                return Ok(n.saturating_mul(unit));
            }
        }
    }
    Err(format!(
        "invalid duration {raw:?} (expected <N>s, <N>m, <N>h, <N>d, or <N>w)"
    ))
}

fn parse_since_spec(raw: &str, now_ms: u64) -> Result<u64, CliError> {
    let trimmed = raw.trim();
    if let Some(ms) = parse_local_date_midnight_ms(trimmed) {
        return Ok(ms);
    }
    parse_duration_ms(trimmed)
        .map(|dur| now_ms.saturating_sub(dur))
        .map_err(|_| {
            CliError::Usage(format!(
                "invalid --since {raw:?} (expected relative duration like 30m, 2h, 7d or date YYYY-MM-DD)"
            ))
        })
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
        } => {
            let label = if *profile == AppProfile::Bypass {
                "APP_BYPASS"
            } else {
                "APP_PROFILE"
            };
            format!(
                "{:<6} {label} app={app:?} profile={profile:?} reason={reason:?}",
                "[--]"
            )
        }
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
        DiagnosticEvent::PackNotice { notice, detail } => {
            format!("{:<6} PACK_NOTICE ({notice}) {detail}", "[--]")
        }
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
                    CliError::Usage(
                        "--since requires a duration (e.g. 2h, 7d) or YYYY-MM-DD".to_string(),
                    )
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

    let time_flags_count =
        usize::from(today) + usize::from(yesterday) + usize::from(since_raw.is_some());
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

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct TraceRecord {
    v: u8,
    ts_ms: u64,
    mono_ms: u64,
    dt_ms: u64,
    ic: u32,
    app: String,
    fe: String,
    im: String,
    ev: String,
    out: String,
    sym: Option<String>,
    code: Option<u32>,
    mods: Option<u32>,
    ch: Option<String>,
    del: Option<u32>,
    txt: Option<String>,
    s0: String,
    s1: Option<String>,
    burst: u8,
    act: u8,
    pend: Option<String>,
    q: Option<u32>,
    sync: u8,
    scr: Option<String>,
}

#[derive(Serialize)]
struct TraceFileJson {
    name: String,
    bytes: u64,
}

#[derive(Serialize)]
struct TraceStatusJson {
    trace_log: bool,
    rules_path: String,
    initialized: bool,
    log_dir: String,
    file_count: usize,
    total_bytes: u64,
    files: Vec<TraceFileJson>,
}

struct TraceFileEntry {
    utc_days: i64,
    order: u8,
    name: String,
    path: PathBuf,
    bytes: u64,
}

fn discover_trace_files(dir: &Path) -> io::Result<Vec<TraceFileEntry>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(utc_days) = smartkey_core::parse_trace_filename_days(&name) else {
            continue;
        };
        let order = if name.ends_with(".jsonl.1") { 0 } else { 1 };
        let path = entry.path();
        let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        files.push(TraceFileEntry {
            utc_days,
            order,
            name,
            path,
            bytes,
        });
    }
    files.sort_by(|a, b| {
        a.utc_days
            .cmp(&b.utc_days)
            .then_with(|| a.order.cmp(&b.order))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(files)
}

fn format_trace_line(rec: &TraceRecord) -> String {
    let ts_str = format!(
        "{}.{:03}",
        format_local_timestamp(rec.ts_ms),
        rec.ts_ms % 1000
    );
    let app_tag = if rec.app.is_empty() { "-" } else { &rec.app };
    let fe_tag = if rec.fe.is_empty() { "-" } else { &rec.fe };
    let im_tag = if rec.im.is_empty() { "-" } else { &rec.im };

    let mut parts = vec![
        ts_str,
        format!("+{}ms", rec.dt_ms),
        format!("ic#{}", rec.ic),
        format!("{app_tag}({fe_tag}/{im_tag})"),
        rec.ev.to_ascii_uppercase(),
        rec.out.clone(),
    ];

    if let Some(del) = rec.del {
        parts.push(format!("del={del}"));
    }
    if let Some(ref txt) = rec.txt {
        parts.push(format!("txt={txt:?}"));
    }
    if let Some(ref ch) = rec.ch {
        parts.push(format!("ch={ch:?}"));
    } else if let Some(ref sym) = rec.sym {
        if let Some(mods) = rec.mods.filter(|&m| m != 0) {
            parts.push(format!("sym={sym}+0x{mods:x}"));
        } else {
            parts.push(format!("sym={sym}"));
        }
    }

    let state_str = match rec.s1.as_deref() {
        Some(s1) if !s1.is_empty() && s1 != rec.s0 => format!("{} -> {s1}", rec.s0),
        _ => rec.s0.clone(),
    };
    if !state_str.is_empty() {
        parts.push(state_str);
    }

    let mut flags = format!("b={} a={}", rec.burst, rec.act);
    if let Some(ref pend) = rec.pend {
        flags.push_str(&format!(" pend={pend:?}"));
    }
    if let Some(q) = rec.q {
        flags.push_str(&format!(" q={q}"));
    }
    flags.push_str(&format!(" sy={}", rec.sync));
    parts.push(format!("[{flags}]"));

    if let Some(ref scr) = rec.scr {
        parts.push(format!("scr={scr:?}"));
    }

    parts.join(" ")
}

fn query_trace_tail(
    log_dir: &Path,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    app_filter: Option<&str>,
    grep_pat: Option<&str>,
    format_human: bool,
    limit: usize,
) -> Result<(usize, VecDeque<(String, String)>), CliError> {
    let start_days = start_ms.map(|ms| (ms / 86_400_000) as i64 - 1);
    let end_days = end_ms.map(|ms| (ms / 86_400_000) as i64 + 1);

    let files = discover_trace_files(log_dir).map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut total_matching = 0usize;
    let mut ring: VecDeque<(String, String)> = VecDeque::with_capacity(limit.min(4096));

    for file in files {
        if let Some(lo) = start_days {
            if file.utc_days < lo {
                continue;
            }
        }
        if let Some(hi) = end_days {
            if file.utc_days > hi {
                continue;
            }
        }
        let f = fs::File::open(&file.path).map_err(|e| CliError::Runtime(e.to_string()))?;
        let reader = io::BufReader::new(f);
        for line_res in reader.lines() {
            let Ok(raw_line) = line_res else {
                break;
            };
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(rec) = serde_json::from_str::<TraceRecord>(trimmed) else {
                continue;
            };
            if let Some(s) = start_ms {
                if rec.ts_ms < s {
                    continue;
                }
            }
            if let Some(e) = end_ms {
                if rec.ts_ms >= e {
                    continue;
                }
            }
            if let Some(pat) = app_filter {
                if !rec.app.to_lowercase().contains(pat) {
                    continue;
                }
            }
            let formatted = if grep_pat.is_some() || format_human {
                format_trace_line(&rec)
            } else {
                String::new()
            };
            if let Some(pat) = grep_pat {
                if !trimmed.to_lowercase().contains(pat)
                    && !formatted.to_lowercase().contains(pat)
                {
                    continue;
                }
            }

            total_matching += 1;
            if limit > 0 {
                if ring.len() == limit {
                    ring.pop_front();
                }
                ring.push_back((trimmed.to_string(), formatted));
            }
        }
    }

    Ok((total_matching, ring))
}

fn print_trace_help() {
    println!(
        "USAGE:\n  \
         smartkey trace [show|tail] [--today|--yesterday|--since <spec>] [--app <app>] [--grep <substr>] [--json] [-n|--limit <N>] [--dir <path>] [--log-dir <path>]\n  \
         smartkey trace on|off [--dir <path>]\n  \
         smartkey trace status [--json] [--dir <path>] [--log-dir <path>]\n  \
         smartkey trace clear [--dir <path>] [--log-dir <path>]\n\n\
         Control or inspect the ultra-low-overhead Fcitx5 flight-recorder trace log.\n\
         Bypassed apps, terminals, password, URL, email, and numeric inputs are never traced.\n\
         Trace JSONL logs live in: {}",
        smartkey_core::default_state_log_dir().display()
    );
}

fn cmd_trace(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
    let mut log_dir: Option<PathBuf> = None;
    let mut limit: usize = 80;
    let mut limit_set = false;
    let mut as_json = false;
    let mut clear_flag = false;
    let mut today = false;
    let mut yesterday = false;
    let mut since_raw: Option<String> = None;
    let mut grep_pat: Option<String> = None;
    let mut app_filter: Option<String> = None;
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
                print_trace_help();
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
                limit_set = true;
            }
            "--since" => {
                i += 1;
                let raw = args.get(i).ok_or_else(|| {
                    CliError::Usage(
                        "--since requires a duration (e.g. 30m, 2h, 7d) or YYYY-MM-DD".to_string(),
                    )
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
            "--app" => {
                i += 1;
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--app requires an app name".to_string()))?;
                app_filter = Some(raw.to_lowercase());
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
                clear_flag = true;
            }
            other if other.starts_with('-') => {
                return Err(CliError::Usage(format!(
                    "unknown flag {other} (run `smartkey trace --help` for usage)"
                )));
            }
            other => pos.push(other),
        }
        i += 1;
    }

    if pos.len() > 1 {
        return Err(CliError::Usage(format!(
            "unexpected argument {:?} (run `smartkey trace --help` for usage)",
            pos[1]
        )));
    }

    let time_flags_count =
        usize::from(today) + usize::from(yesterday) + usize::from(since_raw.is_some());
    if time_flags_count > 1 {
        return Err(CliError::Usage(
            "--today, --yesterday, and --since are mutually exclusive".to_string(),
        ));
    }

    let action = if clear_flag {
        match pos.first().copied() {
            None | Some("clear") => "clear",
            Some(other) => {
                return Err(CliError::Usage(format!(
                    "--clear cannot be combined with subcommand {other:?}"
                )));
            }
        }
    } else {
        pos.first().copied().unwrap_or("show")
    };

    let effective_log_dir = log_dir
        .clone()
        .unwrap_or_else(smartkey_core::default_state_log_dir);
    let has_show_filters = time_flags_count > 0
        || grep_pat.is_some()
        || app_filter.is_some()
        || limit_set;

    match action {
        "on" | "enable" | "off" | "disable" => {
            if has_show_filters || as_json || log_dir.is_some() {
                return Err(CliError::Usage(format!(
                    "`smartkey trace {action}` only accepts [--dir <path>]"
                )));
            }
            let enable = matches!(action, "on" | "enable");
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let (_, effective) = store
                .set_setting("diagnostics.trace_log", if enable { "true" } else { "false" })
                .map_err(map_store_err)?;
            let reloaded = notify_fcitx5_reload();
            println!(
                "Trace recorder {} (diagnostics.trace_log = {effective}, {})",
                if enable { "enabled" } else { "disabled" },
                if reloaded {
                    "Fcitx5 reloaded"
                } else {
                    "saved to rules.toml; will apply on next focus change"
                }
            );
            if enable {
                println!(
                    "Privacy guarantee: bypass apps, terminals, password, URL, email, and numeric fields are never traced."
                );
                println!("Log directory: {}", effective_log_dir.display());
            }
            Ok(())
        }
        "status" => {
            if has_show_filters {
                return Err(CliError::Usage(
                    "`smartkey trace status` does not accept query/limit filters".to_string(),
                ));
            }
            let (store, initialized) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let enabled = store.compiled().diagnostics.trace_log;
            let expected_rules_path = dir.join("rules.toml");
            let files = discover_trace_files(&effective_log_dir)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            let total_bytes: u64 = files.iter().map(|f| f.bytes).sum();

            if as_json {
                let payload = TraceStatusJson {
                    trace_log: enabled,
                    rules_path: expected_rules_path.display().to_string(),
                    initialized,
                    log_dir: effective_log_dir.display().to_string(),
                    file_count: files.len(),
                    total_bytes,
                    files: files
                        .into_iter()
                        .map(|f| TraceFileJson {
                            name: f.name,
                            bytes: f.bytes,
                        })
                        .collect(),
                };
                println!("{}", serde_json::to_string_pretty(&payload).unwrap());
                return Ok(());
            }

            println!("=== SmartKey Trace Recorder ===");
            println!(
                "State                : {}",
                if enabled { "ENABLED" } else { "DISABLED" }
            );
            println!(
                "Rules File           : {}",
                if initialized {
                    expected_rules_path.display().to_string()
                } else {
                    format!("{} (not initialized yet)", expected_rules_path.display())
                }
            );
            println!("Log Directory        : {}", effective_log_dir.display());
            println!(
                "Trace Files          : {} file(s), {} bytes total",
                files.len(),
                total_bytes
            );
            for f in &files {
                println!("  {}  ({} bytes)", f.name, f.bytes);
            }
            Ok(())
        }
        "clear" => {
            if has_show_filters || as_json {
                return Err(CliError::Usage(
                    "`smartkey trace clear` only accepts [--dir <path>] [--log-dir <path>]"
                        .to_string(),
                ));
            }
            let removed = EventLog::clear_trace_files(&effective_log_dir)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            let _ = notify_fcitx5_reload();
            println!(
                "Removed {removed} trace file(s) from {}",
                effective_log_dir.display()
            );
            Ok(())
        }
        "show" | "tail" => {
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let enabled = store.compiled().diagnostics.trace_log;
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

            let (total_matching, ring) = query_trace_tail(
                &effective_log_dir,
                start_ms,
                end_ms,
                app_filter.as_deref(),
                grep_pat.as_deref(),
                !as_json,
                limit,
            )?;

            if as_json {
                for (raw_line, _) in &ring {
                    println!("{raw_line}");
                }
                return Ok(());
            }

            println!(
                "=== SmartKey Flight-Recorder Trace ({}) ===",
                if enabled { "ENABLED" } else { "DISABLED" }
            );
            println!("Log Directory        : {}", effective_log_dir.display());
            println!(
                "Matching Records     : {} total (showing {}, oldest -> newest)",
                total_matching,
                ring.len()
            );
            if ring.is_empty() {
                println!(
                    "\n(no matching trace records; run `smartkey trace on` to start recording)"
                );
                return Ok(());
            }
            println!();
            for (_, formatted) in &ring {
                println!("  {formatted}");
            }
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown trace subcommand {other:?} (run `smartkey trace --help` for usage)"
        ))),
    }
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
                    "USAGE:\n  smartkey explain [--json] [--lang <code>] [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>\n\n\
                     Evaluate a token against user rules, Vietnamese Telex rescue (with --lang vi), and the loaded .skdict v3 language scorer."
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
                let ch = chars.next().ok_or_else(|| {
                    CliError::Usage("--punct requires a non-empty character".to_string())
                })?;
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
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--lang requires a language code".to_string()))?;
                lang = parse_lang_arg(raw)?;
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

    let cfg_dir = dir.unwrap_or_else(default_config_dir);
    let (mut store, _) = load_rule_store_readonly(&cfg_dir, Preset::Balanced)?;
    if let Some(d) = dialect {
        store.scorer_config_mut().dialect = d;
    }
    store.set_active_lang(lang);

    let engine = SmartKeyEngine::new_auto(store);
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
    let (thresh_score, thresh_margin) = LatinScorer::thresholds(aggr);
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

fn tier_label(tier: PackTier) -> &'static str {
    match tier {
        PackTier::Data => "data",
        PackTier::ImeBridge => "ime-bridge",
    }
}

fn validate_and_install_skdict(
    staged_path: &Path,
    target_path: &Path,
    expected_lang: &str,
) -> Result<PackHeader, CliError> {
    let bytes = fs::read(staged_path).map_err(|e| CliError::Runtime(e.to_string()))?;
    let lex = match PackLexicon::from_owned_bytes(bytes) {
        Ok(lex) => lex,
        Err(err) => {
            let _ = fs::remove_file(staged_path);
            return Err(CliError::Runtime(format!(
                "invalid dictionary format: {err}"
            )));
        }
    };
    let header = lex.header().clone();
    if header.lang != expected_lang {
        let _ = fs::remove_file(staged_path);
        return Err(CliError::Runtime(format!(
            "pack language mismatch: expected '{expected_lang}', found '{}'",
            header.lang
        )));
    }
    fs::rename(staged_path, target_path).map_err(|e| {
        let _ = fs::remove_file(staged_path);
        CliError::Runtime(e.to_string())
    })?;
    Ok(header)
}

fn format_hex_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut s, "{b:02x}");
    }
    s
}

fn render_dict_list_lines(
    scoped_dirs: &[(&str, PathBuf)],
    verbose: bool,
) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut found = false;

    for (scope, dir) in scoped_dirs {
        if !dir.exists() {
            continue;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut paths: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("skdict") && p.is_file())
            .collect();
        paths.sort();

        for path in paths {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
            let sz = fs::metadata(&path)
                .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                .unwrap_or(0.0);
            let bytes = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    lines.push(format!(
                        "  {scope:<8} {stem:<6} ({sz:.2} MiB) [unreadable — {e}] -> {}",
                        path.display()
                    ));
                    found = true;
                    continue;
                }
            };
            match peek_skdict_header_and_manifest(&bytes) {
                Ok((hdr, manifest)) => {
                    lines.push(format!(
                        "  {scope:<8} {:<6} v{}.{}.{} ({}, variants=[{}], {:.2} MiB) -> {}",
                        hdr.lang,
                        hdr.pack_version[0],
                        hdr.pack_version[1],
                        hdr.pack_version[2],
                        tier_label(hdr.tier),
                        hdr.variants.join(", "),
                        sz,
                        path.display()
                    ));
                    if verbose {
                        if manifest.is_empty() {
                            lines.push("           sources: (none)".to_string());
                        } else {
                            for src in manifest {
                                lines.push(format!(
                                    "           source: {} sha256={} license={}",
                                    src.name,
                                    format_hex_bytes(&src.sha256),
                                    src.license
                                ));
                            }
                        }
                    }
                }
                Err(LexiconError::LegacyFormat(v)) => {
                    lines.push(format!(
                        "  {scope:<8} {stem:<6} ({sz:.2} MiB) [legacy v{v} — ignored; run: smartkey dict update {stem}] -> {}",
                        path.display()
                    ));
                }
                Err(e) => {
                    lines.push(format!(
                        "  {scope:<8} {stem:<6} ({sz:.2} MiB) [invalid — ignored: {e}] -> {}",
                        path.display()
                    ));
                }
            }
            found = true;
        }
    }

    (lines, found)
}

fn cmd_dict(args: &[String]) -> CliResult {
    match args.first().map(String::as_str) {
        Some("--help" | "-h") => {
            println!(
                "USAGE:\n  smartkey dict [list [-v|--verbose]|dir|install <lang> <path_or_url>|remove <lang>|update [lang]]\n  Alias: dicts\n\n\
                 Inspect, install, update, or remove modular .skdict language dictionary packs."
            );
            Ok(())
        }
        Some("list" | "ls") | None => {
            let mut verbose = false;
            for arg in args.iter().skip(1) {
                match arg.as_str() {
                    "-v" | "--verbose" => verbose = true,
                    other => {
                        return Err(CliError::Usage(format!(
                            "unexpected argument {other:?} (usage: smartkey dict list [-v|--verbose])"
                        )));
                    }
                }
            }
            println!("=== SmartKey Language Packs ===");
            let user_dir = smartkey_core::default_user_dict_dir();
            println!("User Dict Directory   : {}", user_dir.display());
            println!("System Dict Directory : /usr/share/smartkey/dicts\n");

            let mut scoped_dirs: Vec<(&str, PathBuf)> = Vec::new();
            if let Ok(env_dir) = env::var("SMARTKEY_DICT_DIR") {
                if !env_dir.trim().is_empty() {
                    scoped_dirs.push(("[env]", PathBuf::from(env_dir.trim())));
                }
            }
            scoped_dirs.push(("[user]", user_dir));
            for sys in ["/usr/local/share/smartkey/dicts", "/usr/share/smartkey/dicts"] {
                let p = PathBuf::from(sys);
                if !scoped_dirs.iter().any(|(_, d)| d == &p) {
                    scoped_dirs.push(("[system]", p));
                }
            }

            let (lines, found) = render_dict_list_lines(&scoped_dirs, verbose);
            for line in lines {
                println!("{line}");
            }
            if !found {
                #[cfg(feature = "bundled-en")]
                println!("  [bundled] en (built into binary fallback)");
                #[cfg(feature = "bundled-vi")]
                println!("  [bundled] vi (built into binary fallback)");
                #[cfg(not(any(feature = "bundled-en", feature = "bundled-vi")))]
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

            let hdr = validate_and_install_skdict(&staged_path, &target_path, lang)?;

            let reloaded = notify_fcitx5_reload();
            println!(
                "Successfully installed {lang}.skdict (v{}.{}.{}, tier={}, variants=[{}]) to {} ({})",
                hdr.pack_version[0],
                hdr.pack_version[1],
                hdr.pack_version[2],
                tier_label(hdr.tier),
                hdr.variants.join(", "),
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
            println!("Checking GitHub releases from segfawlt/smartkey for '{lang}'...");
            let url = format!(
                "https://github.com/segfawlt/smartkey/releases/latest/download/{lang}.skdict"
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
                    let hdr = validate_and_install_skdict(&staged_path, &target_path, lang)?;
                    let reloaded = notify_fcitx5_reload();
                    println!(
                        "Updated {lang}.skdict (v{}.{}.{}, tier={}, variants=[{}]) at {} ({})",
                        hdr.pack_version[0],
                        hdr.pack_version[1],
                        hdr.pack_version[2],
                        tier_label(hdr.tier),
                        hdr.variants.join(", "),
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

fn parse_app_profile(raw: &str) -> Result<AppProfile, CliError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "bypass" => Ok(AppProfile::Bypass),
        "browser" => Ok(AppProfile::Browser),
        "chat" => Ok(AppProfile::Chat),
        "prose" => Ok(AppProfile::Prose),
        other => Err(CliError::Usage(format!(
            "unknown app profile {other:?} (expected bypass, browser, chat, or prose)"
        ))),
    }
}

fn app_profile_label(profile: AppProfile) -> &'static str {
    match profile {
        AppProfile::Bypass => "bypass",
        AppProfile::Browser => "browser",
        AppProfile::Chat => "chat",
        AppProfile::Prose => "prose",
    }
}

fn print_app_help() {
    println!(
        "\
USAGE:
  smartkey app list [--json] [--dir <path>]
  smartkey app classify <program_id> [--json] [--dir <path>]
  smartkey app add <bypass|browser|chat|prose> <program_id> [--json] [--dir <path>]
  smartkey app remove [bypass|browser|chat|prose] <program_id> [--json] [--dir <path>]
  Aliases: apps

Inspect resolved application profiles or manage `[app_profiles]` overrides in rules.toml."
    );
}

#[derive(Serialize)]
struct AppListJson<'a> {
    unknown_app_policy: Option<&'static str>,
    bypass: Vec<&'a str>,
    browser: Vec<&'a str>,
    chat: Vec<&'a str>,
    prose: Vec<&'a str>,
}

#[derive(Serialize)]
struct AppClassifyJson<'a> {
    program_id: &'a str,
    normalized_id: String,
    profile: &'static str,
    reason: &'static str,
    unknown_app_policy: Option<&'static str>,
}

#[derive(Serialize)]
struct AppEditJson {
    action: &'static str,
    profile: Option<&'static str>,
    program_id: String,
    changed: bool,
    fcitx5_reloaded: bool,
}

fn cmd_app(args: &[String]) -> CliResult {
    let mut dir = default_config_dir();
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
                print_app_help();
                return Ok(());
            }
            "--" => {
                end_of_flags = true;
            }
            "--json" => {
                as_json = true;
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
                    "unknown flag {other} (try `smartkey app --help`)"
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
                    "unexpected argument {:?} (usage: smartkey app list [--json] [--dir <path>])",
                    pos[1]
                )));
            }
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let apps = &store.compiled().app_profiles;
            let pol = apps.unknown_app_policy.map(app_profile_label);
            if as_json {
                let payload = AppListJson {
                    unknown_app_policy: pol,
                    bypass: apps.bypass.iter().map(String::as_str).collect(),
                    browser: apps.browser.iter().map(String::as_str).collect(),
                    chat: apps.chat.iter().map(String::as_str).collect(),
                    prose: apps.prose.iter().map(String::as_str).collect(),
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| CliError::Runtime(e.to_string()))?
                );
                return Ok(());
            }

            println!("=== SmartKey Application Profiles ===");
            println!(
                "  unknown_app_policy : {}",
                pol.unwrap_or("default (prose)")
            );
            for (name, set) in [
                ("bypass", &apps.bypass),
                ("browser", &apps.browser),
                ("chat", &apps.chat),
                ("prose", &apps.prose),
            ] {
                if set.is_empty() {
                    println!("  {name:<8} : (none)");
                } else {
                    let joined: Vec<&str> = set.iter().map(String::as_str).collect();
                    println!("  {name:<8} : {}", joined.join(", "));
                }
            }
            println!(
                "  tip      : test any app ID via `smartkey app classify <program_id>`, or set policy via `smartkey config set app_profiles.unknown_app_policy bypass`"
            );
            Ok(())
        }
        "classify" | "check" => {
            if pos.len() != 2 {
                return Err(CliError::Usage(
                    "usage: smartkey app classify <program_id> [--json] [--dir <path>]".to_string(),
                ));
            }
            let raw_app = pos[1];
            let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced)?;
            let apps = &store.compiled().app_profiles;
            let (profile, reason) = apps.classify_app(raw_app);
            let norm = normalize_program_id(raw_app);
            let pol = apps.unknown_app_policy.map(app_profile_label);
            if as_json {
                let payload = AppClassifyJson {
                    program_id: raw_app,
                    normalized_id: norm,
                    profile: app_profile_label(profile),
                    reason,
                    unknown_app_policy: pol,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| CliError::Runtime(e.to_string()))?
                );
                return Ok(());
            }
            println!(
                "program={raw_app:?} normalized={norm:?} profile={} reason={reason} unknown_app_policy={}",
                app_profile_label(profile),
                pol.unwrap_or("default")
            );
            Ok(())
        }
        "add" => {
            if pos.len() != 3 {
                return Err(CliError::Usage(
                    "usage: smartkey app add <bypass|browser|chat|prose> <program_id> [--json] [--dir <path>]"
                        .to_string(),
                ));
            }
            let profile = parse_app_profile(pos[1])?;
            let raw_app = pos[2];
            let norm = normalize_program_id(raw_app);
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let changed = store
                .add_app_profile_override(profile, raw_app)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            let reloaded = if changed {
                notify_fcitx5_reload()
            } else {
                false
            };
            let label = app_profile_label(profile);
            if as_json {
                let payload = AppEditJson {
                    action: "add",
                    profile: Some(label),
                    program_id: norm,
                    changed,
                    fcitx5_reloaded: reloaded,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| CliError::Runtime(e.to_string()))?
                );
                return Ok(());
            }
            if changed {
                println!(
                    "Assigned app {norm:?} to profile '{label}' ({})",
                    if reloaded {
                        "Fcitx5 reloaded"
                    } else {
                        "saved to rules.toml"
                    }
                );
            } else {
                println!("App {norm:?} already assigned to profile '{label}'.");
            }
            Ok(())
        }
        "remove" | "rm" | "del" => {
            let (profile, raw_app) = match pos.len() {
                2 => (None, pos[1]),
                3 => (Some(parse_app_profile(pos[1])?), pos[2]),
                _ => {
                    return Err(CliError::Usage(
                        "usage: smartkey app remove [bypass|browser|chat|prose] <program_id> [--json] [--dir <path>]"
                            .to_string(),
                    ));
                }
            };
            let norm = normalize_program_id(raw_app);
            let mut store = load_rule_store_mut(&dir, Preset::Balanced)?;
            let removed = store
                .remove_app_profile_override(profile, raw_app)
                .map_err(|e| CliError::Runtime(e.to_string()))?;
            if !removed {
                return Err(CliError::Runtime(format!(
                    "no [app_profiles] override found for {norm:?}"
                )));
            }
            let reloaded = notify_fcitx5_reload();
            let prof_label = profile.map(app_profile_label);
            if as_json {
                let payload = AppEditJson {
                    action: "remove",
                    profile: prof_label,
                    program_id: norm,
                    changed: true,
                    fcitx5_reloaded: reloaded,
                };
                println!(
                    "{}",
                    serde_json::to_string_pretty(&payload)
                        .map_err(|e| CliError::Runtime(e.to_string()))?
                );
                return Ok(());
            }
            println!(
                "Removed [app_profiles] override for {norm:?} ({})",
                if reloaded {
                    "Fcitx5 reloaded"
                } else {
                    "saved to rules.toml"
                }
            );
            Ok(())
        }
        other => Err(CliError::Usage(format!(
            "unknown app subcommand {other:?} (usage: smartkey app [list|classify|add|remove] ...)"
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
                let raw = args
                    .get(i)
                    .ok_or_else(|| CliError::Usage("--lang requires an id (en|vi)".to_string()))?;
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

    let mut engine = SmartKeyEngine::new_auto(store);
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
  trace [show|on|off|status|clear] [--app <a>] [--since <spec>] [--grep <p>] [--json] [-n <N>]
                                                         Control or inspect the low-overhead Fcitx5 flight-recorder trace log
  rules [list|staged|prune|add|remove] ... [--json] [--dir <path>]
                                                         Inspect confidence, staged queues, prune stale rules, or edit rules
  config [list|get <key>|set <key> <val>] [--json]       Inspect or update settings & diagnostics in rules.toml
  explain [--json] [--lang en|vi] [--prev <w>] [--punct <c>] [--dialect mixed|us|gb] [--sentence-start] [--dir <path>] <token>
                                                         Explain scoring, proper nouns & candidate ranking
  app|apps [list|classify|add|remove] ... [--json] [--dir <path>]
                                                         Inspect resolved app profiles or edit [app_profiles] overrides
  plugin|plugins [list|enable|disable|toggle] [vi-lotus] [--dir <path>]
                                                         Manage modular input-method & language bridge plugins
  dict|dicts [list|dir|install <lang> <src>|remove <lang>|update [lang]]
                                                         Inspect and manage modular language dictionary packs
  init [--preset <name>] [--dir <path>] [--force]        Initialize ~/.config/smartkey/rules.toml
  repl [--lang en|vi] [--dir <path>] [--in-memory]       Interactive terminal typing & learning sandbox

PATHS:
  Config & brain : ~/.config/smartkey/rules.toml
  Event logs     : ~/.local/state/smartkey/logs/events-YYYY-MM-DD.jsonl
  Trace logs     : ~/.local/state/smartkey/logs/trace-YYYY-MM-DD.jsonl
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
        Some("trace") => cmd_trace(&args[1..]),
        Some("explain") => cmd_explain(&args[1..]),
        Some("app" | "apps") => cmd_app(&args[1..]),
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
            "status", "enable", "reload", "init", "rules", "config", "log", "trace", "explain",
            "app", "plugin", "dict", "repl",
        ] {
            assert_eq!(run_cli(&vec_args(&[sub, "--help"])), Ok(()));
        }

        // Read-only `status` / `rules` / `config` / `log` / `trace` / `app` must NOT create config directories when missing
        let uninit_dir =
            env::temp_dir().join(format!("smartkey_cli_readonly_{}", std::process::id()));
        let _ = fs::remove_dir_all(&uninit_dir);
        let dir_str = uninit_dir.to_str().unwrap();
        assert_eq!(run_cli(&vec_args(&["status", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["rules", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["config", "--dir", dir_str])), Ok(()));
        assert_eq!(run_cli(&vec_args(&["log", "--dir", dir_str])), Ok(()));
        assert_eq!(
            run_cli(&vec_args(&[
                "trace", "status", "--dir", dir_str, "--log-dir", dir_str
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "trace", "--app", "zen", "--dir", dir_str, "--log-dir", dir_str
            ])),
            Ok(())
        );
        assert_eq!(run_cli(&vec_args(&["app", "--dir", dir_str])), Ok(()));
        assert_eq!(
            run_cli(&vec_args(&["app", "classify", "zcode", "--dir", dir_str])),
            Ok(())
        );
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
            run_cli(&vec_args(&[
                "rules", "add", "fix", "teh", "the", "--dir", d
            ])),
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
            run_cli(&vec_args(&["rules", "add", "word", "FastAPI", "--dir", d])),
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
            run_cli(&vec_args(&["rules", "add", "word", "Hyprland", "--dir", d])),
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

    #[test]
    fn cli_confidence_staged_and_prune_commands() {
        let dir = env::temp_dir().join(format!(
            "smartkey_cli_conf_{}_{}",
            std::process::id(),
            current_epoch_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules_path = dir.join("rules.toml");
        let state_path = dir.join("learner_state.json");
        {
            let mut store =
                RuleStore::load_or_init(rules_path, state_path, Preset::Balanced).unwrap();
            store
                .set_user_correction(&LangId::en(), "adn", "and")
                .unwrap();
            store.add_correction(&LangId::en(), "staletypo", "fixed");
            store.add_personal_word_with_stage(&LangId::en(), "domixi", "untouched_3x");
            store
                .state_mut()
                .untouched_unknowns
                .insert("en:xqztest".to_string(), 1);
            store
                .state_mut()
                .untouched_unknowns
                .insert("rawdiag".to_string(), 4);
            store
                .state_mut()
                .staged_corrections
                .insert("case:en:pytorch".to_string(), 1);
            store.flush_state().unwrap();

            let staged = collect_staged_entries(store.state());
            assert!(staged
                .iter()
                .any(|e| e.key == "en:xqztest" && e.count == 1 && e.threshold == 3));
            assert!(staged
                .iter()
                .any(|e| e.key == "case:en:pytorch" && e.count == 1 && e.threshold == 2));
            assert!(
                !staged.iter().any(|e| e.key == "rawdiag"),
                "bare diagnostic log keys must not clutter `rules staged`"
            );
        }

        let dir_str = dir.to_str().unwrap();
        assert_eq!(
            run_cli(&vec_args(&["rules", "--json", "--dir", dir_str])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&["rules", "list", "--suspect", "--dir", dir_str])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "rules", "list", "--tier", "manual", "--dir", dir_str
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&["rules", "staged", "--json", "--dir", dir_str])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&[
                "rules",
                "summary",
                "--suspect",
                "--dir",
                dir_str
            ])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            run_cli(&vec_args(&["rules", "list", "--dry-run", "--dir", dir_str])),
            Err(CliError::Usage(_))
        ));

        assert_eq!(
            run_cli(&vec_args(&[
                "rules",
                "prune",
                "--older-than",
                "0s",
                "--dry-run",
                "--dir",
                dir_str
            ])),
            Ok(())
        );
        let (reloaded_dry, _) = load_rule_store_readonly(&dir, Preset::Balanced).unwrap();
        assert!(reloaded_dry
            .compiled()
            .corrections
            .contains_key("staletypo"));

        assert_eq!(
            run_cli(&vec_args(&[
                "rules",
                "prune",
                "--older-than",
                "0s",
                "--dir",
                dir_str
            ])),
            Ok(())
        );
        let (reloaded, _) = load_rule_store_readonly(&dir, Preset::Balanced).unwrap();
        assert!(!reloaded.compiled().corrections.contains_key("staletypo"));
        assert!(reloaded.compiled().corrections.contains_key("adn"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_app_classify_and_override_crud_respects_unknown_policy_and_comments() {
        let dir = env::temp_dir().join(format!(
            "smartkey_cli_app_{}_{}",
            std::process::id(),
            current_epoch_ms()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let rules_path = dir.join("rules.toml");
        fs::write(
            &rules_path,
            "# keep user app comments\n[settings]\nenabled = true\n",
        )
        .unwrap();
        let d = dir.to_str().unwrap();

        assert_eq!(
            run_cli(&vec_args(&["app", "list", "--json", "--dir", d])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "app", "classify", "zcode", "--json", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "app", "add", "prose", "obsidian", "--json", "--dir", d
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&["app", "add", "bypass", "obsidian", "--dir", d])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "config",
                "set",
                "app_profiles.unknown_app_policy",
                "bypass",
                "--dir",
                d
            ])),
            Ok(())
        );

        let (store, _) = load_rule_store_readonly(&dir, Preset::Balanced).unwrap();
        assert_eq!(
            store.compiled().app_profiles.classify_app("obsidian"),
            (AppProfile::Bypass, "user-app-override")
        );
        assert_eq!(
            store.compiled().app_profiles.classify_app("unknown-editor"),
            (AppProfile::Bypass, "unknown-app-policy")
        );

        assert_eq!(
            run_cli(&vec_args(&[
                "app", "remove", "bypass", "obsidian", "--json", "--dir", d
            ])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&["app", "remove", "obsidian", "--dir", d])),
            Err(CliError::Runtime(_))
        ));
        assert!(matches!(
            run_cli(&vec_args(&["app", "add", "invalidprof", "foo", "--dir", d])),
            Err(CliError::Usage(_))
        ));

        let raw = fs::read_to_string(&rules_path).unwrap();
        assert!(raw.contains("# keep user app comments"));

        let bypass_line = format_diagnostic_event_line(&DiagnosticEvent::AppBypass {
            app: "zcode".to_string(),
            profile: AppProfile::Bypass,
            reason: "program-list".to_string(),
        });
        assert!(bypass_line.contains("APP_BYPASS"));

        let prose_line = format_diagnostic_event_line(&DiagnosticEvent::AppBypass {
            app: "obsidian".to_string(),
            profile: AppProfile::Prose,
            reason: "user-app-override".to_string(),
        });
        assert!(prose_line.contains("APP_PROFILE"));
        assert!(!prose_line.contains("APP_BYPASS"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_dict_install_validates_v3_header_lang_and_lists_legacy_and_manifest() {
        use std::collections::BTreeMap;
        use smartkey_core::lexicon::{
            build_words_fst_bytes, serialize_skdict_v3, LanguageRules, PackHeader, PackTier,
            SkdictV3Payload, SourceManifestEntry, WordMeta,
        };

        let dir = env::temp_dir().join(format!("smartkey_cli_dict_v3_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let staged = dir.join("staged_es.skdict");
        let target = dir.join("installed_es.skdict");
        let legacy = dir.join("en.skdict");

        let words_fst = build_words_fst_bytes(&[(
            "hola",
            WordMeta {
                freq0: 210,
                freq1: 0,
                variant_mask: 0x01,
                flags: 0,
                word_id: 1,
            },
        )]);
        let es_hdr = PackHeader {
            lang: "es".to_string(),
            pack_version: [0, 3, 4],
            tier: PackTier::Data,
            variants: vec!["es_ES".to_string()],
            freq_slots: vec![0x01],
            default_variant_idx: 0,
            feature_flags: 0,
        };
        let manifest = vec![SourceManifestEntry {
            name: "es_aosp".to_string(),
            sha256: [0x5A; 32],
            license: "Apache-2.0".to_string(),
        }];
        let es_bytes = serialize_skdict_v3(&SkdictV3Payload {
            header: &es_hdr,
            words_fst: &words_fst,
            typos_fst: &[],
            typo_pool: &[],
            variant_rewrites: &[],
            bigrams: &BTreeMap::new(),
            smash_entries: &[],
            proper_nouns: &[],
            shorthand: &[],
            slang_set: &[],
            rules: Some(&LanguageRules::default()),
            syllable_dict: &[],
            source_manifest: &manifest,
        });

        // Wrong expected lang must fail and clean up staged file.
        fs::write(&staged, &es_bytes).unwrap();
        let err = validate_and_install_skdict(&staged, &target, "fr").unwrap_err();
        assert!(
            matches!(err, CliError::Runtime(ref m) if m.contains("pack language mismatch")),
            "got {err:?}"
        );
        assert!(!staged.exists());
        assert!(!target.exists());

        // Legacy SKDICT02 must fail with unsupported legacy v2 message.
        fs::write(&legacy, b"SKDICT02olddata").unwrap();
        fs::write(&staged, b"SKDICT02olddata").unwrap();
        let err_v2 = validate_and_install_skdict(&staged, &target, "en").unwrap_err();
        assert!(
            matches!(err_v2, CliError::Runtime(ref m) if m.contains("legacy .skdict v2 format is unsupported")),
            "got {err_v2:?}"
        );
        assert!(!staged.exists());

        // Matching expected lang installs cleanly and returns PackHeader.
        fs::write(&staged, &es_bytes).unwrap();
        let installed_hdr = validate_and_install_skdict(&staged, &target, "es").unwrap();
        assert_eq!(installed_hdr, es_hdr);
        assert!(target.exists());

        // `dict list -v` displays legacy notice and SOURCE_MANIFEST.
        let (lines, found) = render_dict_list_lines(&[("[user]", dir.clone())], true);
        assert!(found);
        let joined = lines.join("\n");
        assert!(
            joined.contains("[legacy v2 — ignored; run: smartkey dict update en]"),
            "lines:\n{joined}"
        );
        assert!(
            joined.contains("es     v0.3.4 (data, variants=[es_ES]"),
            "lines:\n{joined}"
        );
        assert!(
            joined.contains("source: es_aosp sha256=") && joined.contains("license=Apache-2.0"),
            "lines:\n{joined}"
        );

        let notice_line = format_diagnostic_event_line(&DiagnosticEvent::PackNotice {
            notice: "legacy_pack_ignored".to_string(),
            detail: "/tmp/en.skdict (skdict v2)".to_string(),
        });
        assert!(notice_line.contains("PACK_NOTICE (legacy_pack_ignored)"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_trace_subcommand_lifecycle_and_filtering() {
        let base = env::temp_dir().join(format!(
            "smartkey_cli_trace_{}_{}",
            std::process::id(),
            current_epoch_ms()
        ));
        let cfg_dir = base.join("cfg");
        let log_dir = base.join("logs");
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&cfg_dir).unwrap();
        fs::create_dir_all(&log_dir).unwrap();

        let cfg_str = cfg_dir.to_str().unwrap();
        let log_str = log_dir.to_str().unwrap();

        // 1. Toggle trace recorder on -> verify rules.toml & status
        assert_eq!(run_cli(&vec_args(&["trace", "on", "--dir", cfg_str])), Ok(()));
        let (store_on, _) = load_rule_store_readonly(&cfg_dir, Preset::Balanced).unwrap();
        assert!(store_on.compiled().diagnostics.trace_log);
        assert_eq!(
            run_cli(&vec_args(&["status", "--json", "--dir", cfg_str])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "trace",
                "status",
                "--json",
                "--dir",
                cfg_str,
                "--log-dir",
                log_str
            ])),
            Ok(())
        );

        // 2. Write rotated `.1` + active `.jsonl` trace files plus an untouched `events-*.jsonl`
        let rot_file = log_dir.join("trace-2026-11-18.jsonl.1");
        let active_file = log_dir.join("trace-2026-11-18.jsonl");
        let events_file = log_dir.join("events-2026-11-18.jsonl");
        fs::write(
            &rot_file,
            concat!(
                r#"{"v":1,"ts_ms":1794960000100,"mono_ms":100,"dt_ms":0,"ic":1,"app":"firefox","fe":"wayland_v2","im":"lotus","ev":"pre_key","out":"lotus_char","ch":"a","s0":"Idle","s1":"Composing(a)","burst":0,"act":0,"sync":1}"#,
                "\n",
                r#"{"v":1,"ts_ms":1794960000200,"mono_ms":200,"dt_ms":100,"ic":2,"app":"zen","fe":"wayland_v2","im":"lotus","ev":"commit","out":"lotus_boundary_replace","txt":"nsinh ","s0":"Composing(nsinh)","s1":"JustAutocorrected(nính<-nsinh,tb=1)","burst":0,"act":1,"sync":0,"scr":"cần |nsinh"}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(
            &active_file,
            concat!(
                r#"{"v":1,"ts_ms":1794960000300,"mono_ms":300,"dt_ms":100,"ic":2,"app":"zen","fe":"wayland_v2","im":"lotus","ev":"replace","out":"forward_bs_delay15ms","del":6,"txt":"cần nính ","s0":"JustAutocorrected(nính<-nsinh,tb=1)","burst":0,"act":0,"sync":0,"scr":"cần |sinh"}"#,
                "\n"
            ),
        )
        .unwrap();
        fs::write(&events_file, "{\"v\":1}\n").unwrap();

        // Chronological read across .1 then .jsonl, plus app/grep/limit filters
        let (tot_all, all_rows) = query_trace_tail(&log_dir, None, None, None, None, true, 80).unwrap();
        assert_eq!(tot_all, 3);
        assert_eq!(all_rows.len(), 3);
        assert!(all_rows[0].1.contains("firefox(wayland_v2/lotus)"));
        assert!(all_rows[1].1.contains("nsinh") && all_rows[1].1.contains("scr=\"cần |nsinh\""));
        assert!(all_rows[2].1.contains("scr=\"cần |sinh\""));

        let (tot_zen, zen_rows) =
            query_trace_tail(&log_dir, None, None, Some("zen"), Some("nsinh"), true, 1).unwrap();
        assert_eq!(tot_zen, 2);
        assert_eq!(zen_rows.len(), 1);
        assert!(zen_rows[0].1.contains("forward_bs_delay15ms"));

        // CLI invocation paths for filtering & mutually exclusive flags
        assert_eq!(
            run_cli(&vec_args(&[
                "trace", "show", "--dir", cfg_str, "--log-dir", log_str
            ])),
            Ok(())
        );
        assert_eq!(
            run_cli(&vec_args(&[
                "trace", "--app", "zen", "--grep", "nsinh", "-n", "1", "--json", "--dir",
                cfg_str, "--log-dir", log_str
            ])),
            Ok(())
        );
        assert!(matches!(
            run_cli(&vec_args(&["trace", "--today", "--yesterday"])),
            Err(CliError::Usage(_))
        ));

        // 3. `trace clear` removes trace files while preserving `events-*.jsonl`
        assert_eq!(
            run_cli(&vec_args(&[
                "trace", "clear", "--dir", cfg_str, "--log-dir", log_str
            ])),
            Ok(())
        );
        assert!(!rot_file.exists());
        assert!(!active_file.exists());
        assert!(events_file.exists());

        // 4. Turn trace off
        assert_eq!(
            run_cli(&vec_args(&["trace", "off", "--dir", cfg_str])),
            Ok(())
        );
        let (store_off, _) = load_rule_store_readonly(&cfg_dir, Preset::Balanced).unwrap();
        assert!(!store_off.compiled().diagnostics.trace_log);

        let _ = fs::remove_dir_all(&base);
    }
}
