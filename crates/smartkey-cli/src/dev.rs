use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use smartkey_core::{
    build_pack_from_dir, EmptyUserRules, EvalContext, EvalOutcome, LanguageScorer, LatinScorer,
    PackBuildOptions, PackLexicon, PackTier, ScorerConfig,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn default_pack_output_path(root: &Path, lang: &str) -> PathBuf {
    match lang {
        "en" => root.join("crates/smartkey-core/assets/en_lexicon.skdict"),
        other => root.join("target/packs").join(format!("{other}.skdict")),
    }
}

fn corpus_paths(root: &Path, lang: &str) -> (PathBuf, PathBuf) {
    if lang == "en" {
        (
            root.join("tests/corpus/must_fix.tsv"),
            root.join("tests/corpus/must_not_touch.txt"),
        )
    } else {
        let dir = root.join("tests/corpus").join(lang);
        (dir.join("must_fix.tsv"), dir.join("must_not_touch.txt"))
    }
}

fn load_en_scorer() -> Result<LatinScorer, String> {
    #[cfg(feature = "bundled-en")]
    {
        if let Ok(s) = LatinScorer::new_bundled() {
            return Ok(s);
        }
    }
    let skdict_path = workspace_root().join("crates/smartkey-core/assets/en_lexicon.skdict");
    let bytes = fs::read(&skdict_path).map_err(|e| {
        format!(
            "failed to read {}: {e} (run `smartkey-dev build-lexicon` first)",
            skdict_path.display()
        )
    })?;
    let lex = PackLexicon::from_owned_bytes(bytes).map_err(|e| e.to_string())?;
    Ok(LatinScorer::new(lex))
}

fn build_single_pack(
    root: &Path,
    pack_dir: &Path,
    out_path: &Path,
    opts: &PackBuildOptions<'_>,
    verify_corpus: bool,
) -> Result<(), String> {
    let t0 = Instant::now();
    let built = build_pack_from_dir(pack_dir, opts)?;
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(out_path, &built.bytes).map_err(|e| e.to_string())?;

    println!(
        "Built {} ({:.2} MiB, tier={:?}, words={}, typos={}, bigrams={}, smash={}, proper={}, shorthand={}, slang={}, syllables={}) in {:.2}s",
        out_path.display(),
        built.bytes.len() as f64 / (1024.0 * 1024.0),
        built.header.tier,
        built.word_count,
        built.typo_count,
        built.bigram_count,
        built.smash_count,
        built.proper_noun_count,
        built.shorthand_count,
        built.slang_count,
        built.syllable_count,
        t0.elapsed().as_secs_f64()
    );

    if verify_corpus && built.header.tier == PackTier::Data {
        let (must_fix_path, must_not_path) = corpus_paths(root, &built.header.lang);
        if must_fix_path.is_file() && must_not_path.is_file() {
            let lex = PackLexicon::from_owned_bytes(built.bytes).map_err(|e| e.to_string())?;
            let scorer = LatinScorer::new(lex);
            eval_with_scorer(&scorer, &built.header.lang, false, &must_fix_path, &must_not_path)?;
        } else {
            println!(
                "Verified binary roundtrip for {:?} (no corpus at {})",
                built.header.lang,
                must_fix_path.display()
            );
        }
    }
    Ok(())
}

fn discover_pack_langs(dicts_dir: &Path) -> Result<Vec<String>, String> {
    let rd = fs::read_dir(dicts_dir)
        .map_err(|e| format!("failed to read dicts dir {}: {e}", dicts_dir.display()))?;
    let mut langs = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() && p.join("pack.toml").is_file() {
            if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                langs.push(name.to_string());
            }
        }
    }
    langs.sort();
    if langs.is_empty() {
        return Err(format!("no pack.toml found under {}", dicts_dir.display()));
    }
    Ok(langs)
}

fn cmd_build_pack(args: &[String]) -> Result<(), String> {
    let root = workspace_root();
    let mut target_lang: Option<String> = None;
    let mut dicts_dir = root.join("dicts");
    let mut out_path: Option<PathBuf> = None;
    let mut fetch = false;
    let mut verify = false;
    let mut without_count2w = false;
    let mut allow_ambiguous = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--dicts-dir" => {
                i += 1;
                let val = args.get(i).ok_or("--dicts-dir requires a path")?;
                dicts_dir = PathBuf::from(val);
            }
            "--out" => {
                i += 1;
                let val = args.get(i).ok_or("--out requires a path")?;
                out_path = Some(PathBuf::from(val));
            }
            "--fetch" => fetch = true,
            "--verify" => verify = true,
            "--without-count2w" => without_count2w = true,
            "--allow-ambiguous" => allow_ambiguous = true,
            "--help" | "-h" => {
                print_dev_help();
                return Ok(());
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown flag {other:?} for build-pack"));
            }
            positional => {
                if target_lang.is_some() {
                    return Err(format!("unexpected extra argument {positional:?}"));
                }
                target_lang = Some(positional.to_string());
            }
        }
        i += 1;
    }

    let Some(lang) = target_lang else {
        return Err("usage: smartkey-dev build-pack <lang|all> [--dicts-dir <path>] [--out <path>] [--fetch] [--verify] [--without-count2w] [--allow-ambiguous]".to_string());
    };

    let cache_dir = root.join("data/sources");
    let skip_sources: &[&str] = if without_count2w {
        &["bigram_counts"]
    } else {
        &[]
    };
    let opts = PackBuildOptions {
        sources_cache_dir: Some(&cache_dir),
        fetch,
        allow_ambiguous,
        skip_sources,
    };

    if lang == "all" {
        let langs = discover_pack_langs(&dicts_dir)?;
        for l in langs {
            let pack_dir = dicts_dir.join(&l);
            let out = match &out_path {
                Some(dir) => dir.join(format!("{l}.skdict")),
                None => default_pack_output_path(&root, &l),
            };
            build_single_pack(&root, &pack_dir, &out, &opts, verify)?;
        }
    } else {
        let pack_dir = dicts_dir.join(&lang);
        if !pack_dir.join("pack.toml").is_file() {
            return Err(format!(
                "missing pack recipe at {}",
                pack_dir.join("pack.toml").display()
            ));
        }
        let out = out_path.unwrap_or_else(|| default_pack_output_path(&root, &lang));
        build_single_pack(&root, &pack_dir, &out, &opts, verify)?;
    }

    Ok(())
}

fn cmd_build_lexicon() -> Result<(), String> {
    let root = workspace_root();
    let cache_dir = root.join("data/sources");
    let opts = PackBuildOptions {
        sources_cache_dir: Some(&cache_dir),
        ..PackBuildOptions::default()
    };
    let en_dir = root.join("dicts/en");
    let en_out = default_pack_output_path(&root, "en");
    build_single_pack(&root, &en_dir, &en_out, &opts, false)
}

fn eval_with_scorer(
    scorer: &LatinScorer,
    lang: &str,
    without_count2w: bool,
    must_fix_path: &Path,
    must_not_path: &Path,
) -> Result<(), String> {
    let config = ScorerConfig::default();
    let must_fix_raw = fs::read_to_string(must_fix_path)
        .map_err(|e| format!("failed to read {}: {e}", must_fix_path.display()))?;
    let must_not_raw = fs::read_to_string(must_not_path)
        .map_err(|e| format!("failed to read {}: {e}", must_not_path.display()))?;

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
                "invalid TSV at {}:{}: {line}",
                must_fix_path.display(),
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

    let ablation_note = if without_count2w {
        " [without-count2w]"
    } else {
        ""
    };
    println!("=== SmartKey Golden Corpus Evaluation ({lang}{ablation_note}) ===");
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
        return Err(format!("corpus evaluation failed for {lang}"));
    }
    Ok(())
}

fn cmd_eval(args: &[String]) -> Result<(), String> {
    let root = workspace_root();
    let mut lang = "en".to_string();
    let mut without_count2w = false;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--lang" => {
                i += 1;
                lang = args
                    .get(i)
                    .ok_or("--lang requires a language code")?
                    .to_ascii_lowercase();
            }
            "--without-count2w" => without_count2w = true,
            "--help" | "-h" => {
                print_dev_help();
                return Ok(());
            }
            other => {
                return Err(format!(
                    "unexpected argument {other:?} (usage: smartkey-dev eval [--lang <code>] [--without-count2w])"
                ));
            }
        }
        i += 1;
    }

    let scorer = if lang == "en" && !without_count2w {
        load_en_scorer()?
    } else {
        let pack_dir = root.join("dicts").join(&lang);
        let cache_dir = root.join("data/sources");
        let skip_sources: &[&str] = if without_count2w {
            &["bigram_counts"]
        } else {
            &[]
        };
        let built = build_pack_from_dir(
            &pack_dir,
            &PackBuildOptions {
                sources_cache_dir: Some(&cache_dir),
                skip_sources,
                ..PackBuildOptions::default()
            },
        )?;
        let lex = PackLexicon::from_owned_bytes(built.bytes).map_err(|e| e.to_string())?;
        LatinScorer::new(lex)
    };

    let (must_fix_path, must_not_path) = corpus_paths(&root, &lang);
    eval_with_scorer(&scorer, &lang, without_count2w, &must_fix_path, &must_not_path)
}

const GIT_VERSION: &str = env!("SMARTKEY_GIT_VERSION");

fn print_dev_help() {
    println!(
        "\
smartkey-dev {} ({GIT_VERSION}) — Maintainer corpus & dictionary pack tools

USAGE:
  smartkey-dev <COMMAND>

COMMANDS:
  eval [--lang <code>] [--without-count2w]
                  Run golden must-fix / must-not-touch corpus gate & latency benchmark
  build-pack <lang|all> [--dicts-dir <path>] [--out <path>] [--fetch] [--verify] [--without-count2w] [--allow-ambiguous]
                  Compile a .skdict v3 pack from dicts/<lang>/pack.toml
  build-lexicon   Compile bundled dictionary pack asset(s) via PackBuilder

OPTIONS:
  -h, --help      Show this help message
  -V, --version   Show version information",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let res = match args.first().map(String::as_str) {
        Some("build-pack") => cmd_build_pack(&args[1..]),
        Some("build-lexicon") if args.len() == 1 => cmd_build_lexicon(),
        Some("eval") => cmd_eval(&args[1..]),
        Some("--help" | "-h" | "help") => {
            print_dev_help();
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("smartkey-dev {} ({GIT_VERSION})", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some(other) => {
            eprintln!(
                "error: unexpected argument {other:?} (usage: smartkey-dev <eval | build-pack | build-lexicon>)"
            );
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
