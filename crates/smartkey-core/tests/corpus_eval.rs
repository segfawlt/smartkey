use std::fs;
use std::path::Path;

use smartkey_core::{
    Dialect, EmptyUserRules, EnglishScorer, EvalContext, EvalOutcome, LanguageScorer, ScorerConfig,
};

#[test]
fn golden_corpus_must_fix_and_zero_false_positives() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let scorer = EnglishScorer::new_bundled().expect("bundled English lexicon must load");
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
    let scorer = EnglishScorer::new_bundled().expect("bundled English lexicon must load");
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
