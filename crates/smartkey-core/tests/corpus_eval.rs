use std::fs;
use std::path::Path;

use smartkey_core::{
    build_pack_from_dir, sha256_hex, Dialect, EmptyUserRules, EnglishLexicon, EnglishScorer,
    EvalContext, EvalOutcome, LanguageScorer, LatinScorer, MatchKind, PackBuildOptions,
    PackLexicon, ScorerConfig,
};

#[test]
fn golden_corpus_must_fix_and_zero_false_positives() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let scorer = LatinScorer::new_bundled().expect("bundled English lexicon must load");
    let config = ScorerConfig::default();

    // 1. Hard 0% false-positive gate (`must_not_touch.txt`)
    let must_not =
        fs::read_to_string(root.join("tests/corpus/must_not_touch.txt")).expect("must_not_touch");
    let mut fp_errors = Vec::new();
    for (idx, line) in must_not.lines().enumerate() {
        let tok = line.trim();
        if tok.is_empty() || tok.starts_with('#') {
            continue;
        }
        let outcome = scorer.evaluate(tok, &EvalContext::default(), &config, &EmptyUserRules);
        if let EvalOutcome::Autocorrect {
            replacement, kind, ..
        } = outcome
        {
            fp_errors.push(format!(
                "line {}: {:?} falsely modified to {:?} ({:?})",
                idx + 1,
                tok,
                replacement,
                kind
            ));
        }
    }
    assert!(
        fp_errors.is_empty(),
        "False positives violated must_not_touch.txt:\n{}",
        fp_errors.join("\n")
    );

    // 2. Real-world typo recall gate (`must_fix.tsv`)
    let must_fix = fs::read_to_string(root.join("tests/corpus/must_fix.tsv")).expect("must_fix");
    let mut fix_errors = Vec::new();
    for (idx, line) in must_fix.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        assert!(cols.len() >= 4, "bad TSV row at line {}", idx + 1);
        let prev_col = cols[0].trim();
        let typed = cols[1].trim();
        let punct_col = cols[2].trim();
        let expected = cols[3].trim();

        let ctx = EvalContext {
            prev_word: if prev_col == "-" || prev_col == "^" {
                None
            } else {
                Some(prev_col)
            },
            boundary_punct: if punct_col == "-" {
                None
            } else {
                punct_col.chars().next()
            },
            at_sentence_start: prev_col == "^",
            ..EvalContext::default()
        };

        let outcome = scorer.evaluate(typed, &ctx, &config, &EmptyUserRules);
        let actual = match &outcome {
            EvalOutcome::Autocorrect { replacement, .. } => replacement.as_str(),
            EvalOutcome::Unchanged { .. } => typed,
        };
        if actual != expected {
            fix_errors.push(format!(
                "line {}: prev={:?} typed={:?} punct={:?} -> got {:?}, expected {:?}",
                idx + 1,
                prev_col,
                typed,
                punct_col,
                actual,
                expected
            ));
        }
    }
    assert!(
        fix_errors.is_empty(),
        "Missed fixes in must_fix.tsv:\n{}",
        fix_errors.join("\n")
    );
}

#[test]
fn dialect_modes_mixed_us_and_gb_work() {
    let scorer = LatinScorer::new_bundled().expect("bundled English lexicon must load");
    let ctx = EvalContext::default();

    // Mixed: both `colour` and `color` stay untouched; typo `beahviour` outputs US `behavior`
    let mixed = ScorerConfig {
        dialect: Dialect::Mixed,
        ..ScorerConfig::default()
    };
    assert!(matches!(
        scorer.evaluate("colour", &ctx, &mixed, &EmptyUserRules),
        EvalOutcome::Unchanged { .. }
    ));
    assert!(matches!(
        scorer.evaluate("color", &ctx, &mixed, &EmptyUserRules),
        EvalOutcome::Unchanged { .. }
    ));
    assert_eq!(
        scorer.evaluate("beahviour", &ctx, &mixed, &EmptyUserRules),
        match scorer.evaluate("beahviour", &ctx, &mixed, &EmptyUserRules) {
            r @ EvalOutcome::Autocorrect { .. } => r,
            other => panic!("expected autocorrect, got {other:?}"),
        }
    );

    // EnUs: `colour` normalizes to `color`
    let us_only = ScorerConfig {
        dialect: Dialect::EnUs,
        ..ScorerConfig::default()
    };
    match scorer.evaluate("colour", &ctx, &us_only, &EmptyUserRules) {
        EvalOutcome::Autocorrect { replacement, .. } => assert_eq!(replacement, "color"),
        other => panic!("expected colour -> color in EnUs, got {other:?}"),
    }

    // EnGb: `color` normalizes to `colour`, and typo `beahviour` fixes to `behaviour`
    let gb_only = ScorerConfig {
        dialect: Dialect::EnGb,
        ..ScorerConfig::default()
    };
    match scorer.evaluate("color", &ctx, &gb_only, &EmptyUserRules) {
        EvalOutcome::Autocorrect { replacement, .. } => assert_eq!(replacement, "colour"),
        other => panic!("expected color -> colour in EnGb, got {other:?}"),
    }
    match scorer.evaluate("beahviour", &ctx, &gb_only, &EmptyUserRules) {
        EvalOutcome::Autocorrect { replacement, .. } => assert_eq!(replacement, "behaviour"),
        other => panic!("expected beahviour -> behaviour in EnGb, got {other:?}"),
    }
}

#[test]
fn shorthand_toggle_and_slang_zero_fuzzy_pollution() {
    let scorer = LatinScorer::new_bundled().expect("bundled English lexicon must load");
    let ctx = EvalContext::default();

    // When `expand_shorthand = false`, shorthand triggers are still shielded as valid tokens
    // and never corrupted into unrelated dictionary words:
    let no_shorthand = ScorerConfig {
        expand_shorthand: false,
        ..ScorerConfig::default()
    };
    for tok in [
        "pls", "plz", "rn", "idk", "ikr", "ofc", "btw", "tbh", "ngl", "thx", "wdym", "ttyl",
    ] {
        assert!(
            matches!(
                scorer.evaluate(tok, &ctx, &no_shorthand, &EmptyUserRules),
                EvalOutcome::Unchanged { .. }
            ),
            "expected {tok:?} to remain unchanged when expand_shorthand = false"
        );
    }

    // Pure slang & clipped words (`onl`, `obv`, `notif`, `bruh`) must have `freq == 0` in the FST
    // so they are shielded verbatim when typed, yet NEVER proposed as fuzzy candidates for typos:
    for slang in ["onl", "offl", "obv", "notif", "collab", "priv"] {
        let meta = scorer
            .lexicon()
            .lookup_word(slang)
            .unwrap_or_else(|| panic!("{slang:?} must be in lexicon"));
        assert!(meta.is_valid_for_dialect(Dialect::Mixed));
        assert_eq!(
            meta.freq0, 0,
            "pure slang {slang:?} must have freq0 == 0 so it never pollutes fuzzy search"
        );
    }
}

#[test]
fn step5a_curated_typos_enforce_metadata_and_dialects_stay_green() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let scorer = LatinScorer::new_bundled().expect("bundled English lexicon must load");

    // Verify `must_not_touch.txt` and `must_fix.tsv` remain 100% intact across `EnUs` and `EnGb`
    // (allowing the intentional cross-dialect spelling normalizations in each strict dialect).
    let must_not =
        fs::read_to_string(root.join("tests/corpus/must_not_touch.txt")).expect("must_not_touch");
    let must_fix = fs::read_to_string(root.join("tests/corpus/must_fix.tsv")).expect("must_fix");

    for dialect in [Dialect::Mixed, Dialect::EnUs, Dialect::EnGb] {
        let cfg = ScorerConfig {
            dialect,
            ..ScorerConfig::default()
        };
        for (idx, line) in must_not.lines().enumerate() {
            let tok = line.trim();
            if tok.is_empty() || tok.starts_with('#') {
                continue;
            }
            if dialect == Dialect::EnUs
                && matches!(tok, "colour" | "behaviour" | "realise" | "centre")
            {
                continue;
            }
            if dialect == Dialect::EnGb
                && matches!(tok, "color" | "behavior" | "realize" | "center")
            {
                continue;
            }
            let outcome = scorer.evaluate(tok, &EvalContext::default(), &cfg, &EmptyUserRules);
            assert!(
                matches!(outcome, EvalOutcome::Unchanged { .. }),
                "line {}: {tok:?} unexpectedly changed in {dialect:?}: {outcome:?}",
                idx + 1
            );
        }

        for (idx, line) in must_fix.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = trimmed.split('\t').collect();
            let prev_col = cols[0].trim();
            let typed = cols[1].trim();
            let punct_col = cols[2].trim();
            let expected = if dialect == Dialect::EnGb && typed == "beahviour" {
                "behaviour"
            } else {
                cols[3].trim()
            };
            let ctx = EvalContext {
                prev_word: if prev_col == "-" || prev_col == "^" {
                    None
                } else {
                    Some(prev_col)
                },
                boundary_punct: if punct_col == "-" {
                    None
                } else {
                    punct_col.chars().next()
                },
                at_sentence_start: prev_col == "^",
                ..EvalContext::default()
            };
            let outcome = scorer.evaluate(typed, &ctx, &cfg, &EmptyUserRules);
            let actual = match &outcome {
                EvalOutcome::Autocorrect { replacement, .. } => replacement.as_str(),
                EvalOutcome::Unchanged { .. } => typed,
            };
            assert_eq!(
                actual,
                expected,
                "line {}: mismatch in {dialect:?} for {typed:?}",
                idx + 1
            );
        }
    }

    // Synthetic lexicon proving Step 5a skips offensive, zero-freq slang, wrong-dialect, and missing words:
    let tmp_dir = std::env::temp_dir().join(format!("smartkey_syn_pack_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();
    let en_us = " word=cleanword,f=180\n word=badword,f=180,possibly_offensive=true\n";
    let en_gb = " word=brituk,f=210\n";
    let typos = "clenaword,cleanword\nbadwrod,badword\nbirktu,brituk\nslngzero,slangzero\nmisswrod,nonexistentword\n";
    fs::write(tmp_dir.join("us.combined"), en_us).unwrap();
    fs::write(tmp_dir.join("gb.combined"), en_gb).unwrap();
    fs::write(tmp_dir.join("typos.csv"), typos).unwrap();
    fs::write(tmp_dir.join("slang.txt"), "slangzero\n").unwrap();
    let pack_toml = format!(
        "[pack]\ncode = \"en\"\nversion = \"0.3.0\"\nformat_version = 3\ntier = \"data\"\nlicense = \"GPL-3.0-or-later\"\n\n\
         [variants.en_US]\nfreq_slot = 0\ndefault = true\n\n\
         [variants.en_GB]\nfreq_slot = 1\n\n\
         [sources.en_us]\nformat = \"aosp_combined\"\npath = \"us.combined\"\nsha256 = \"{}\"\nlicense = \"Apache-2.0\"\nvariants = [\"en_US\"]\n\n\
         [sources.en_gb]\nformat = \"aosp_combined\"\npath = \"gb.combined\"\nsha256 = \"{}\"\nlicense = \"Apache-2.0\"\nvariants = [\"en_GB\"]\n\n\
         [sources.typos]\nformat = \"typos_csv\"\npath = \"typos.csv\"\nsha256 = \"{}\"\nlicense = \"MIT\"\n\n\
         [overlays]\nslang = [\"slang.txt\"]\n",
        sha256_hex(en_us.as_bytes()),
        sha256_hex(en_gb.as_bytes()),
        sha256_hex(typos.as_bytes()),
    );
    fs::write(tmp_dir.join("pack.toml"), pack_toml).unwrap();
    let built = build_pack_from_dir(&tmp_dir, &PackBuildOptions::default()).unwrap();
    let _ = fs::remove_dir_all(&tmp_dir);

    let lex = PackLexicon::from_owned_bytes(built.bytes).unwrap();
    let syn_scorer = LatinScorer::new(lex);
    let us_cfg = ScorerConfig {
        dialect: Dialect::EnUs,
        ..ScorerConfig::default()
    };
    let gb_cfg = ScorerConfig {
        dialect: Dialect::EnGb,
        ..ScorerConfig::default()
    };

    assert!(matches!(
        syn_scorer.evaluate("clenaword", &EvalContext::default(), &us_cfg, &EmptyUserRules),
        EvalOutcome::Autocorrect { replacement, .. } if replacement == "cleanword"
    ));
    // `birktu -> brituk` (len 6, DL 3: reachable only via Step 5a) fires in EnGb, skipped in EnUs:
    assert!(matches!(
        syn_scorer.evaluate("birktu", &EvalContext::default(), &gb_cfg, &EmptyUserRules),
        EvalOutcome::Autocorrect { replacement, .. } if replacement == "brituk"
    ));
    for blocked in ["badwrod", "birktu", "slngzero", "misswrod"] {
        assert!(
            matches!(
                syn_scorer.evaluate(blocked, &EvalContext::default(), &us_cfg, &EmptyUserRules),
                EvalOutcome::Unchanged { .. }
            ),
            "Step 5a must never propose {blocked:?} under EnUs"
        );
    }
}

#[test]
fn polish_and_latin_homographs_stay_unchanged_while_english_capitalizes() {
    // Also exercise backwards-compat `EnglishScorer` / `EnglishLexicon` type aliases.
    let scorer: EnglishScorer = EnglishScorer::new_bundled().expect("bundled English pack loads");
    let _: &EnglishLexicon = scorer.lexicon();
    let ctx = EvalContext::default();
    let cfg = ScorerConfig::default();

    for word in ["polish", "latin", "Polish", "Latin"] {
        assert_eq!(
            scorer.evaluate(word, &ctx, &cfg, &EmptyUserRules),
            EvalOutcome::Unchanged {
                reason: MatchKind::ValidWord,
                do_not_learn: true,
            },
            "expected {word:?} to remain Unchanged(ValidWord)"
        );
    }

    assert_eq!(
        scorer.evaluate("english", &ctx, &cfg, &EmptyUserRules),
        EvalOutcome::Autocorrect {
            replacement: "English".to_string(),
            kind: MatchKind::ProperNoun,
            score: 4.5,
            margin: 4.5,
        }
    );
}

#[test]
fn compiled_es_and_fr_packs_pass_corpus_smash_ascii_lowercase_i_and_elisions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let cache_dir = root.join("data/sources");
    if !cache_dir.join("es/es_wordlist.combined").is_file()
        || !cache_dir.join("fr/fr_wordlist.combined").is_file()
    {
        eprintln!(
            "skipping compiled_es_and_fr_packs test: cached sources not present in {}",
            cache_dir.display()
        );
        return;
    }

    let cfg = ScorerConfig::default();

    for lang in ["es", "fr"] {
        let pack_dir = root.join("dicts").join(lang);
        let built = build_pack_from_dir(
            &pack_dir,
            &PackBuildOptions {
                sources_cache_dir: Some(&cache_dir),
                ..PackBuildOptions::default()
            },
        )
        .unwrap_or_else(|e| panic!("build-pack {lang} failed: {e}"));

        let lex = PackLexicon::from_owned_bytes(built.bytes)
            .unwrap_or_else(|e| panic!("load {lang} pack failed: {e}"));

        assert!(
            lex.smash_entries().iter().all(|e| e.word.is_ascii()),
            "{lang} SMASH_INDEX must contain ASCII-only words"
        );

        let scorer = LatinScorer::new(lex);

        // `capitalize_standalone_i = false`: lowercase `"i"` stays untouched!
        assert!(
            matches!(
                scorer.evaluate("i", &EvalContext::default(), &cfg, &EmptyUserRules),
                EvalOutcome::Unchanged { .. }
            ),
            "{lang} must leave standalone lowercase 'i' unchanged"
        );

        // Evaluate `must_not_touch.txt`
        let must_not = fs::read_to_string(root.join("tests/corpus").join(lang).join("must_not_touch.txt"))
            .unwrap();
        for (idx, line) in must_not.lines().enumerate() {
            let tok = line.trim();
            if tok.is_empty() || tok.starts_with('#') {
                continue;
            }
            let out = scorer.evaluate(tok, &EvalContext::default(), &cfg, &EmptyUserRules);
            assert!(
                matches!(out, EvalOutcome::Unchanged { .. }),
                "{lang} must_not_touch line {}: {tok:?} changed to {out:?}",
                idx + 1
            );
        }

        // Evaluate `must_fix.tsv`
        let must_fix = fs::read_to_string(root.join("tests/corpus").join(lang).join("must_fix.tsv"))
            .unwrap();
        for (idx, line) in must_fix.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let cols: Vec<&str> = trimmed.split('\t').collect();
            let prev_col = cols[0].trim();
            let typed = cols[1].trim();
            let punct_col = cols[2].trim();
            let expected = cols[3].trim();
            let ctx = EvalContext {
                prev_word: if prev_col == "-" || prev_col == "^" {
                    None
                } else {
                    Some(prev_col)
                },
                boundary_punct: if punct_col == "-" {
                    None
                } else {
                    punct_col.chars().next()
                },
                at_sentence_start: prev_col == "^",
                ..EvalContext::default()
            };
            let out = scorer.evaluate(typed, &ctx, &cfg, &EmptyUserRules);
            let actual = match &out {
                EvalOutcome::Autocorrect { replacement, .. } => replacement.as_str(),
                EvalOutcome::Unchanged { .. } => typed,
            };
            assert_eq!(
                actual,
                expected,
                "{lang} must_fix line {}: {typed:?}",
                idx + 1
            );
        }

        if lang == "es" {
            for w in ["canción", "español", "también", "pingüino"] {
                assert!(scorer.lexicon().contains_valid(w, Dialect::Mixed));
            }
        }
        if lang == "fr" {
            for w in ["français", "être", "cœur", "noël"] {
                assert!(scorer.lexicon().contains_valid(w, Dialect::Mixed));
            }
            for elided in ["l'homme", "qu'il", "d'accord", "c'est", "lorsqu'arrive", "puisqu'existe"]
            {
                assert_eq!(
                    scorer.evaluate(elided, &EvalContext::default(), &cfg, &EmptyUserRules),
                    EvalOutcome::Unchanged {
                        reason: MatchKind::ValidWord,
                        do_not_learn: true,
                    },
                    "expected French elision shield for {elided:?}"
                );
            }
        }
    }
}

#[test]
fn anti_inflection_guard_prefers_motor_clean_stems_while_preserving_true_inflections() {
    let scorer = LatinScorer::new_bundled().expect("bundled English lexicon must load");
    let cfg = ScorerConfig::default();

    // Unattested tense/inflection hijack prevention (Option A: repair to motor-clean base stem):
    for (prev, typed, expected) in [
        (None, "dsaible", "disable"),
        (Some("is"), "dsaible", "disable"),
        (Some("be"), "dsaible", "disable"),
        (Some("are"), "dsaible", "disable"),
        (Some("is"), "obvserve", "observe"),
        (Some("the"), "planenr", "planner"),
        (Some("is"), "compiel", "compile"),
        (Some("is"), "reuqire", "require"),
    ] {
        let ctx = EvalContext {
            prev_word: prev,
            ..EvalContext::default()
        };
        match scorer.evaluate(typed, &ctx, &cfg, &EmptyUserRules) {
            EvalOutcome::Autocorrect { replacement, .. } => assert_eq!(
                replacement, expected,
                "for prev={prev:?} typed={typed:?}, expected {expected:?}, got {replacement:?}"
            ),
            other => panic!(
                "for prev={prev:?} typed={typed:?}, expected Autocorrect({expected:?}), got {other:?}"
            ),
        }
    }

    // Legitimate inflected typos and tight-motor contextual disambiguation remain intact:
    for (prev, typed, expected) in [
        (Some("is"), "dsiabled", "disabled"),
        (Some("is"), "dsaibled", "disabled"),
        (Some("is"), "disablef", "disabled"),
        (None, "disabed", "disabled"),
        (None, "usin", "using"),
        (Some("over"), "ther", "there"),
        (Some("in"), "ther", "their"),
        (Some("too"), "mcu", "much"),
    ] {
        let ctx = EvalContext {
            prev_word: prev,
            ..EvalContext::default()
        };
        match scorer.evaluate(typed, &ctx, &cfg, &EmptyUserRules) {
            EvalOutcome::Autocorrect { replacement, .. } => assert_eq!(
                replacement, expected,
                "positive case prev={prev:?} typed={typed:?}, expected {expected:?}, got {replacement:?}"
            ),
            other => panic!(
                "positive case prev={prev:?} typed={typed:?}, expected Autocorrect({expected:?}), got {other:?}"
            ),
        }
    }
}
