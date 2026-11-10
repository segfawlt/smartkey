use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use smartkey_core::{
    compile_skdict_from_sources, EmptyUserRules, EnglishLexicon, EnglishScorer, EvalContext,
    EvalOutcome, LanguageScorer, ScorerConfig,
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
            "failed to read {}: {e} (run `cargo run --release -p smartkey-cli --bin smartkey-dev -- build-lexicon` first)",
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
    let slang_terms = fs::read_to_string(root.join("data/slang.txt")).unwrap_or_default();
    let proper_nouns = fs::read_to_string(root.join("data/proper_nouns.tsv")).unwrap_or_default();
    let typos_csv = read_req(&src_dir.join("typos_words.csv"))?;
    let uk_json = read_req(&src_dir.join("british_to_american.json"))?;
    let count_2w = read_req(&src_dir.join("count_2w.txt"))?;

    println!("Compiling FST lexicon, 96k known smashes, proper nouns, dialect map & bigrams...");
    let image = compile_skdict_from_sources(
        &en_us,
        &en_gb,
        &dev_terms,
        &slang_terms,
        &proper_nouns,
        &typos_csv,
        &uk_json,
        &count_2w,
    );

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

const GIT_VERSION: &str = env!("SMARTKEY_GIT_VERSION");

fn print_dev_help() {
    println!(
        "\
smartkey-dev {} ({GIT_VERSION}) — Maintainer corpus & dictionary build tools

USAGE:
  smartkey-dev <COMMAND>

COMMANDS:
  eval            Run golden must-fix / must-not-touch corpus gate & latency benchmark
  build-lexicon   Compile raw dictionary & bigram sources into en_lexicon.skdict

OPTIONS:
  -h, --help      Show this help message
  -V, --version   Show version information",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("build-lexicon") if args.len() == 1 => cmd_build_lexicon(),
        Some("eval") if args.len() == 1 => cmd_eval(),
        Some("--help" | "-h" | "help") => {
            print_dev_help();
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("smartkey-dev {} ({GIT_VERSION})", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!("error: unexpected argument {other:?} (usage: smartkey-dev <eval | build-lexicon>)");
            std::process::exit(2);
        }
        None => {
            print_dev_help();
            std::process::exit(2);
        }
    };
    if let Err(e) = res {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
