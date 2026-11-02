use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use smartkey_core::{
    compile_skdict_from_sources, Dialect, EmptyUserRules, EnglishLexicon, EnglishScorer,
    EvalContext, EvalOutcome, LanguageScorer, ScorerConfig,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
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

    // Smoke-verify generated image loads cleanly
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
    let mut dialect = Dialect::Mixed;
    let mut sentence_start = false;
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
                dialect = match args.get(i).map(String::as_str) {
                    Some("us" | "en_US") => Dialect::EnUs,
                    Some("gb" | "uk" | "en_GB") => Dialect::EnGb,
                    _ => Dialect::Mixed,
                };
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
            "usage: smartkey explain [--prev <word>] [--punct <char>] [--dialect mixed|us|gb] [--sentence-start] <token>"
                .to_string(),
        );
    };

    let scorer = load_scorer()?;
    let config = ScorerConfig {
        dialect,
        ..ScorerConfig::default()
    };
    let ctx = EvalContext {
        prev_word: prev.as_deref(),
        boundary_punct: punct,
        at_sentence_start: sentence_start,
        ..EvalContext::default()
    };
    let exp = scorer.explain(&tok, &ctx, &config, &EmptyUserRules);
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

fn cmd_repl() -> Result<(), String> {
    let scorer = load_scorer()?;
    let config = ScorerConfig::default();

    println!("SmartKey Interactive REPL (Press Ctrl-C or Esc to quit)");
    println!("Type naturally — Space/punctuation triggers Layer 1 autocorrect; 1x BackSpace right after an autocorrect snaps back!\n");

    let _guard =
        RawTermGuard::enter().map_err(|e| format!("REPL requires an interactive TTY ({e})"))?;

    let mut line_buf = String::new();
    let mut composing = String::new();
    let mut prev_word: Option<String> = None;
    let mut last_event_note = String::from("ready");
    // `(raw_typed, replacement_with_punct_len_chars)`
    let mut undo_slot: Option<(String, usize)> = None;

    let redraw = |line: &str, note: &str| {
        print!("\r\x1b[2K> {line}    \x1b[90m[{note}]\x1b[0m");
        let _ = io::stdout().flush();
    };
    redraw(&line_buf, &last_event_note);

    let mut stdin = io::stdin();
    let mut byte_buf = [0u8; 4];

    loop {
        if stdin.read(&mut byte_buf[..1]).map_err(|e| e.to_string())? == 0 {
            break;
        }
        let b0 = byte_buf[0];
        if b0 == 3 || b0 == 27 {
            println!("\nBye!");
            break;
        }
        if b0 == b'\r' || b0 == b'\n' {
            line_buf.clear();
            composing.clear();
            prev_word = None;
            undo_slot = None;
            println!();
            last_event_note = "newline".to_string();
            redraw(&line_buf, &last_event_note);
            continue;
        }
        // BackSpace (`0x7f` or `0x08`)
        if b0 == 0x7f || b0 == 0x08 {
            if let Some((raw_word, replaced_chars)) = undo_slot.take() {
                for _ in 0..replaced_chars {
                    line_buf.pop();
                }
                line_buf.push_str(&raw_word);
                last_event_note = format!("UNDO -> restored \"{raw_word}\"");
                composing = raw_word;
            } else {
                line_buf.pop();
                composing.pop();
                last_event_note = "backspace".to_string();
            }
            redraw(&line_buf, &last_event_note);
            continue;
        }

        // Decode UTF-8 scalar char
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

        undo_slot = None;

        let is_word_char =
            ch.is_alphabetic() || ch == '\'' || ch == '’' || (ch == ';' && !composing.is_empty());

        if is_word_char {
            composing.push(ch);
            line_buf.push(ch);
            redraw(&line_buf, &last_event_note);
        } else {
            // Word boundary (`Space`, `,`, `.`, `:`, `!`, `?`, etc.)
            if !composing.is_empty() {
                let boundary_punct = if ch == ' ' { None } else { Some(ch) };
                let ctx = EvalContext {
                    prev_word: prev_word.as_deref(),
                    boundary_punct,
                    at_sentence_start: prev_word.is_none(),
                    ..EvalContext::default()
                };
                let outcome = scorer.evaluate(&composing, &ctx, &config, &EmptyUserRules);
                match outcome {
                    EvalOutcome::Autocorrect {
                        replacement, kind, ..
                    } => {
                        let old_chars = composing.chars().count();
                        for _ in 0..old_chars {
                            line_buf.pop();
                        }
                        line_buf.push_str(&replacement);
                        line_buf.push(ch);
                        let replaced_total_chars = replacement.chars().count() + 1;
                        last_event_note = format!("{composing} -> {replacement} ({kind:?})");
                        undo_slot = Some((composing.clone(), replaced_total_chars));
                        prev_word = if ch == ' ' {
                            Some(
                                replacement
                                    .split_whitespace()
                                    .last()
                                    .unwrap_or(&replacement)
                                    .to_string(),
                            )
                        } else {
                            None
                        };
                    }
                    EvalOutcome::Unchanged { reason, .. } => {
                        line_buf.push(ch);
                        last_event_note = format!("kept \"{composing}\" ({reason:?})");
                        prev_word = if ch == ' ' {
                            Some(composing.clone())
                        } else {
                            None
                        };
                    }
                }
                composing.clear();
            } else {
                line_buf.push(ch);
                if ch != ' ' {
                    prev_word = None;
                }
            }
            redraw(&line_buf, &last_event_note);
        }
    }

    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("build-lexicon") => cmd_build_lexicon(),
        Some("eval") => cmd_eval(),
        Some("explain") => cmd_explain(&args[1..]),
        Some("repl") => cmd_repl(),
        _ => {
            eprintln!("Usage: smartkey <build-lexicon | eval | explain | repl>");
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
