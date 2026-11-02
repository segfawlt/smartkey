use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use smartkey_core::{
    apply_key_to_buffer, compile_skdict_from_sources, Dialect, EmptyUserRules, EnglishLexicon,
    EnglishScorer, EvalContext, EvalOutcome, KeyInput, LanguageScorer, Preset, RuleStore,
    ScorerConfig, SessionAction, SmartKeyEngine, SmartKeySession,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

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

fn load_scorer() -> Result<EnglishScorer, String> {
    #[cfg(feature = "bundled-en")]
    {
        if let Ok(s) = EnglishScorer::new_bundled() {
            return Ok(s);
        }
    }
    let skdict_path = workspace_root().join("crates/smartkey-core/assets/en_lexicon.skdict");
    let bytes = fs::read(&skdict_path).map_err(|e| {
        format!(
            "failed to read {}: {e} (run `smartkey build-lexicon` first)",
            skdict_path.display()
        )
    })?;
    let lex = EnglishLexicon::from_owned_bytes(bytes).map_err(|e| e.to_string())?;
    Ok(EnglishScorer::new(lex))
}

fn load_rule_store(dir: &Path, preset: Preset) -> Result<RuleStore, String> {
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");
    RuleStore::load_or_init(rules_path, state_path, preset).map_err(|e| e.to_string())
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
                    "unknown flag {other} (usage: smartkey init [--preset balanced|chat_casual|developer_strict] [--dir <path>] [--force])"
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
                    "unknown flag {other} (usage: smartkey rules [--dir <path>])"
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

fn cmd_build_lexicon() -> Result<(), String> {
    let root = workspace_root();
    let src_dir = root.join("data/sources");
    let read_req = |p: &Path| -> Result<String, String> {
        fs::read_to_string(p).map_err(|e| {
            format!(
                "missing {} ({e}); run `./scripts/fetch_sources.sh` first",
                p.display()
            )
        })
    };

    let t0 = Instant::now();
    println!("Loading raw dictionary & bigram sources...");
    let en_us = read_req(&src_dir.join("en_US.combined"))?;
    let en_gb = read_req(&src_dir.join("en_GB.combined"))?;
    let dev_terms = read_req(&root.join("data/dev_terms.txt"))?;
    let typos_csv = read_req(&src_dir.join("typos_words.csv"))?;
    let uk_json = read_req(&src_dir.join("british_to_american.json"))?;
    let count_2w = read_req(&src_dir.join("count_2w.txt"))?;

    println!("Compiling FST lexicon, 96k known smashes, dialect map & bigrams...");
    let image =
        compile_skdict_from_sources(&en_us, &en_gb, &dev_terms, &typos_csv, &uk_json, &count_2w);

    let out_path = root.join("crates/smartkey-core/assets/en_lexicon.skdict");
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&out_path, &image).map_err(|e| e.to_string())?;

    let loaded = EnglishLexicon::from_owned_bytes(image.clone()).map_err(|e| e.to_string())?;
    println!(
        "Built {} ({:.2} MiB, {} smash-rescue words) in {:.2}s",
        out_path.display(),
        image.len() as f64 / (1024.0 * 1024.0),
        loaded.smash_entries().len(),
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

fn cmd_eval() -> Result<(), String> {
    let root = workspace_root();
    let scorer = load_scorer()?;
    let config = ScorerConfig::default();

    let must_fix_raw =
        fs::read_to_string(root.join("tests/corpus/must_fix.tsv")).map_err(|e| e.to_string())?;
    let must_not_raw = fs::read_to_string(root.join("tests/corpus/must_not_touch.txt"))
        .map_err(|e| e.to_string())?;

    let mut fix_total = 0usize;
    let mut fix_passed = 0usize;
    let mut fix_failures = Vec::new();
    let mut latencies_ns = Vec::new();

    for (line_no, line) in must_fix_raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        if cols.len() < 4 {
            return Err(format!(
                "invalid TSV at must_fix.tsv:{}: {line}",
                line_no + 1
            ));
        }
        let prev_col = cols[0].trim();
        let typed = cols[1].trim();
        let punct_col = cols[2].trim();
        let expected = cols[3].trim();

        let at_sentence_start = prev_col == "^";
        let prev_word = if prev_col == "-" || prev_col == "^" {
            None
        } else {
            Some(prev_col)
        };
        let boundary_punct = if punct_col == "-" {
            None
        } else {
            punct_col.chars().next()
        };

        let ctx = EvalContext {
            prev_word,
            boundary_punct,
            at_sentence_start,
            ..EvalContext::default()
        };

        let t0 = Instant::now();
        let outcome = scorer.evaluate(typed, &ctx, &config, &EmptyUserRules);
        latencies_ns.push(t0.elapsed().as_nanos() as u64);

        fix_total += 1;
        let actual = match &outcome {
            EvalOutcome::Autocorrect { replacement, .. } => replacement.as_str(),
            EvalOutcome::Unchanged { .. } => typed,
        };
        if actual == expected {
            fix_passed += 1;
        } else {
            fix_failures.push(format!(
                "  line {}: prev={:?} typed={:?} punct={:?} -> got {:?}, expected {:?}",
                line_no + 1,
                prev_col,
                typed,
                punct_col,
                actual,
                expected
            ));
        }
    }

    let mut keep_total = 0usize;
    let mut keep_passed = 0usize;
    let mut keep_failures = Vec::new();

    for (line_no, line) in must_not_raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let ctx = EvalContext::default();
        let t0 = Instant::now();
        let outcome = scorer.evaluate(trimmed, &ctx, &config, &EmptyUserRules);
        latencies_ns.push(t0.elapsed().as_nanos() as u64);

        keep_total += 1;
        match outcome {
            EvalOutcome::Unchanged { .. } => keep_passed += 1,
            EvalOutcome::Autocorrect {
                replacement, kind, ..
            } => {
                keep_failures.push(format!(
                    "  line {}: {:?} falsely changed to {:?} ({:?})",
                    line_no + 1,
                    trimmed,
                    replacement,
                    kind
                ));
            }
        }
    }

    latencies_ns.sort_unstable();
    let pct = |p: f64| -> f64 {
        if latencies_ns.is_empty() {
            return 0.0;
        }
        let idx = (((latencies_ns.len() - 1) as f64) * p).round() as usize;
        latencies_ns[idx] as f64 / 1000.0
    };

    println!("=== SmartKey Golden Corpus Evaluation ===");
    println!(
        "Must-Fix Recall    : {fix_passed}/{fix_total} ({:.1}%)",
        100.0 * fix_passed as f64 / fix_total.max(1) as f64
    );
    println!(
        "Must-Not-Touch Gate: {keep_passed}/{keep_total} ({:.1}% unchanged, {} false positives)",
        100.0 * keep_passed as f64 / keep_total.max(1) as f64,
        keep_failures.len()
    );
    println!(
        "Latency per Token  : p50 = {:.1} µs | p95 = {:.1} µs | p99 = {:.1} µs",
        pct(0.50),
        pct(0.95),
        pct(0.99)
    );

    if !fix_failures.is_empty() {
        println!("\nMust-Fix Misses ({}):", fix_failures.len());
        for f in &fix_failures {
            println!("{f}");
        }
    }
    if !keep_failures.is_empty() {
        println!("\nFalse-Positive Violations ({}):", keep_failures.len());
        for f in &keep_failures {
            println!("{f}");
        }
    }

    if !fix_failures.is_empty() || !keep_failures.is_empty() {
        return Err("corpus evaluation failed".to_string());
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
            "usage: smartkey explain [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--dir <path>] [--sentence-start] <token>"
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
                    "unknown flag {other} (usage: smartkey repl [--dir <path>] [--in-memory])"
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

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("init") => cmd_init(&args[1..]),
        Some("rules") => cmd_rules(&args[1..]),
        Some("build-lexicon") => cmd_build_lexicon(),
        Some("eval") => cmd_eval(),
        Some("explain") => cmd_explain(&args[1..]),
        Some("repl") => cmd_repl(&args[1..]),
        _ => {
            eprintln!("Usage: smartkey <init | rules | build-lexicon | eval | explain | repl>");
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
