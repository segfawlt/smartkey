use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use serde::Deserialize;
use smartkey_core::{
    apply_key_to_buffer, Candidate, DiagnosticEvent, KeyInput, LangId, LearnEvent, MatchKind,
    Preset, RuleStore, SessionAction, SkipReason, SmartKeyEngine, SmartKeySession, UndoResolution,
    UserRuleView,
};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn parse_key_stream(spec: &str) -> Vec<KeyInput> {
    let mut out = Vec::new();
    let mut rest = spec;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("<BS>") {
            out.push(KeyInput::BackSpace);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("<CBS>") {
            out.push(KeyInput::CtrlBackSpace);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("<ENTER>") {
            out.push(KeyInput::Enter);
            rest = after;
        } else if let Some(after) = rest.strip_prefix("<NAV>") {
            out.push(KeyInput::NavOrClick);
            rest = after;
        } else {
            let ch = rest.chars().next().unwrap();
            out.push(KeyInput::Char(ch));
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

fn feed_keys(
    session: &mut SmartKeySession,
    engine: &mut SmartKeyEngine,
    client_buf: &mut String,
    spec: &str,
) -> Vec<LearnEvent> {
    let mut events = Vec::new();
    for key in parse_key_stream(spec) {
        let out = session.process_key(engine, key, 100);
        if let SessionAction::Replace { delete_chars, .. } = &out.action {
            assert!(
                *delete_chars <= client_buf.chars().count(),
                "delete_chars={} exceeded client_buf char len={} ({client_buf:?})",
                delete_chars,
                client_buf.chars().count()
            );
        }
        apply_key_to_buffer(client_buf, key, &out.action);
        events.extend(out.learn_events);
    }
    events
}

#[derive(Debug, Deserialize)]
struct TraceFile {
    trace: Vec<TraceCase>,
}

#[derive(Debug, Deserialize)]
struct TraceCase {
    name: String,
    steps: Vec<TraceStep>,
}

#[derive(Debug, Deserialize)]
struct TraceStep {
    keys: String,
    expect_buf: String,
}

#[test]
fn golden_learning_traces_pass() {
    let raw = fs::read_to_string(workspace_root().join("tests/corpus/learning_traces.toml"))
        .expect("read learning_traces.toml");
    let parsed: TraceFile = toml_edit::de::from_str(&raw).expect("parse learning_traces.toml");

    for case in &parsed.trace {
        let mut engine = SmartKeyEngine::in_memory_default().expect("bundled engine");
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        for (idx, step) in case.steps.iter().enumerate() {
            feed_keys(&mut session, &mut engine, &mut buf, &step.keys);
            assert_eq!(
                buf,
                step.expect_buf,
                "trace {:?} failed at step {} (keys={:?})",
                case.name,
                idx + 1,
                step.keys
            );
        }
    }
}

#[test]
fn six_row_matrix_and_guards_behave_exact_to_spec() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let tmp_dir = std::env::temp_dir().join(format!(
        "smartkey_test_{}_{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let rules_path = tmp_dir.join("rules.toml");
    let state_path = tmp_dir.join("learner_state.json");

    // Seed with a user comment to verify comment-preserving `toml_edit` writes
    let custom_header = "# MY CUSTOM HEADER COMMENT — MUST SURVIVE GRADUATIONS!\n";
    fs::write(
        &rules_path,
        format!(
            "{custom_header}{}\naddr = \"123 Main St\" # inline snippet comment\n",
            Preset::Balanced.initial_toml()
        ),
    )
    .unwrap();

    // --- Session 1 ---
    {
        let store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        // 1. Snippet undo (`1x BackSpace`) reverts WITHOUT blocklisting `addr`
        feed_keys(&mut session, &mut engine, &mut buf, "addr <BS> ");
        assert_eq!(buf, "addr ");
        assert!(engine.store().compiled().blocklist.is_empty());

        // Next `addr ` still expands because snippet undo does NOT blocklist!
        buf.clear();
        feed_keys(&mut session, &mut engine, &mut buf, "addr ");
        assert_eq!(buf, "123 Main St ");

        // 2. Row 3: Self-Confirm Cancel (`chagnes -> changes -> BS -> changes`)
        buf.clear();
        let evs = feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "chagnes <BS><BS><BS><BS><BS>nges ",
        );
        assert_eq!(buf, "changes ");
        assert!(evs.contains(&LearnEvent::SelfConfirmCancelled));
        assert!(engine.store().compiled().blocklist.is_empty());

        // 3. Mind-change input clear (`chagnes -> changes -> clear to "" -> hello`) must NOT blocklist
        buf.clear();
        feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "chagnes <BS><BS><BS><BS><BS><BS><BS><BS>hello ",
        );
        assert_eq!(buf, "hello ");
        assert!(
            !engine.store().is_pair_blocked("chagnes", "changes"),
            "clearing input to write a different word must not poison blocklist"
        );

        // 4. Row 2: Undo + Keep Verbatim (`1x` immediately blocks the pair on disk so the user
        //    never has to backspace a 2nd time, while `personal_dict` waits for a 2nd confirmation)
        buf.clear();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "alot <BS> ");
        assert_eq!(buf, "alot ");
        assert!(evs.contains(&LearnEvent::ShieldedWordSession {
            word: "alot".to_string()
        }));
        assert!(
            engine.store().is_pair_blocked("alot", "a lot"),
            "1x undo+keep must immediately block (alot, a lot) in blocklist on disk"
        );
        assert!(
            engine.store().compiled().personal_dict.is_empty(),
            "1x undo+keep must NOT permanently write personal_dict on the first occurrence"
        );

        // 5. Guard 2: Standalone valid-word edit (`form -> from`) must NEVER learn unigram!
        buf.clear();
        let evs = feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "<ENTER>form <CBS>from ",
        );
        assert!(evs.contains(&LearnEvent::Ignored(SkipReason::ValidWordNoContext)));
        let evs = feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "<ENTER>form <CBS>from ",
        );
        assert!(evs.contains(&LearnEvent::Ignored(SkipReason::ValidWordNoContext)));
        assert!(!engine.store().compiled().corrections.contains_key("form"));

        // 6. Row 6: `Delta-Span` (`prodcut -> production`, `Monday -> Friday`) logged to `skipped_edits`, never learned
        buf.clear();
        let evs = feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "prodcut<BS><BS><BS>uction ",
        );
        assert!(evs.contains(&LearnEvent::Ignored(SkipReason::DeltaSpanRewrite)));
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "Monday <CBS>Friday ");
        assert!(evs.contains(&LearnEvent::Ignored(SkipReason::MindChangeDistance)));
        assert!(!engine.store().state().skipped_edits.is_empty());

        // 7. Expressive stretches (`soooo`, `trờiiii`) never enter `personal_dict` even after 5x
        buf.clear();
        feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "soooo soooo soooo soooo soooo trờiiii trờiiii trờiiii trờiiii <ENTER>",
        );
        assert!(!engine.store().compiled().personal_dict.contains("soooo"));
        assert!(!engine.store().compiled().personal_dict.contains("trờiiii"));

        // 8. Non-ASCII target word (`dak -> Đắk` 2x) routes automatically to `[lang.vi.corrections]`!
        buf.clear();
        feed_keys(&mut session, &mut engine, &mut buf, "dak <CBS>Đắk ");
        feed_keys(&mut session, &mut engine, &mut buf, "dak <CBS>Đắk ");
        assert_eq!(
            engine
                .store()
                .compiled()
                .lang_corrections
                .get("vi")
                .and_then(|m| m.get("dak"))
                .map(String::as_str),
            Some("Đắk")
        );
        assert!(!engine.store().compiled().corrections.contains_key("dak"));

        // 9. Active `lang = "vi"` scopes `3x` untouched custom words (`vinuni`) to `[lang.vi]`, NOT English!
        //    Built-in Vietnamese slang (`khum`) is already shielded out-of-the-box.
        session.set_lang(LangId("vi".to_string()));
        feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "vinuni mot vinuni hai vinuni ba <ENTER>",
        );
        assert!(engine
            .store()
            .compiled()
            .lang_personal_dict
            .get("vi")
            .is_some_and(|s| s.contains("vinuni")));
        assert!(!engine.store().compiled().personal_dict.contains("vinuni"));
    }

    // --- Session 2 (Reload from disk: verifies `1x` pair-block persistence & 2nd-use `personal_dict` graduation) ---
    {
        let store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        // In Session 2, `("alot", "a lot")` is ALREADY blocked from the `1x` Undo+Keep in Session 1!
        assert!(engine.store().is_pair_blocked("alot", "a lot"));
        assert!(!engine.store().compiled().personal_dict.contains("alot"));

        // Typing `alot ` a 2nd time stays `alot ` without needing another Backspace, and graduates `alot` to `personal_dict`!
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "alot <ENTER>");
        assert_eq!(buf, "alot \n");
        assert!(evs.contains(&LearnEvent::GraduatedPersonalWord {
            lang: "en".to_string(),
            word: "alot".to_string(),
        }));
        assert!(engine.store().compiled().personal_dict.contains("alot"));
    }

    // Verify custom comments survived all automatic TOML graduations!
    let saved_toml = fs::read_to_string(&rules_path).unwrap();
    assert!(
        saved_toml.contains("# MY CUSTOM HEADER COMMENT — MUST SURVIVE GRADUATIONS!"),
        "user header comment was clobbered:\n{saved_toml}"
    );
    assert!(
        saved_toml.contains("# inline snippet comment"),
        "inline comment was clobbered:\n{saved_toml}"
    );

    // Verify syntax-error write lock: broken user edit is NEVER clobbered by background learning
    let broken_toml = format!("{saved_toml}\n[broken_table_unclosed\n");
    fs::write(&rules_path, &broken_toml).unwrap();
    {
        let mut locked_store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        locked_store.add_permanent_block("teh", "the");
    }
    let after_broken = fs::read_to_string(&rules_path).unwrap();
    assert_eq!(
        after_broken, broken_toml,
        "syntax-locked rules.toml must never be overwritten"
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn heliboard_mobile_word_hooks_work_portably() {
    let mut engine = SmartKeyEngine::in_memory_default().unwrap();
    let mut session = SmartKeySession::new();

    // 1. Mobile auto-commit followed by Undo + Edit (`fomr -> from` reverted to `form`)
    session.on_mobile_auto_commit("fomr", "from");
    let evs = session.on_mobile_manual_fix(&mut engine, "fomr", "form");
    assert!(evs.contains(&LearnEvent::BlockedPairPermanent {
        typed: "fomr".to_string(),
        rejected: "from".to_string(),
    }));

    // 2. Candidate filtering drops blocked `from` and boosts trouble word `form`
    let mut cands = vec![
        Candidate {
            replacement: "from".to_string(),
            score: 1.2,
            edit_cost: 0.5,
            freq: 190,
            bigram_boost: 0.0,
            kind: MatchKind::FuzzyLexicon,
        },
        Candidate {
            replacement: "form".to_string(),
            score: 1.0,
            edit_cost: 0.5,
            freq: 160,
            bigram_boost: 0.0,
            kind: MatchKind::FuzzyLexicon,
        },
    ];
    session.filter_and_rerank_candidates(&engine, "fomr", &mut cands);
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].replacement, "form");

    // 3. `telex_passthrough` 2x graduation for `fcitx5-lotus`
    assert!(!engine.is_telex_passthrough("aur"));
    session.on_lotus_passthrough_repair(&mut engine, "aur");
    assert!(!engine.is_telex_passthrough("aur"));
    session.on_lotus_passthrough_repair(&mut engine, "aur");
    assert!(engine.is_telex_passthrough("aur"));
}

#[test]
fn diagnostic_logger_tracks_autocorrects_and_1x_2x_3x_backspace_events() {
    let custom_toml = r#"
version = 1
[settings]
enabled = true

[snippets]
";addr" = "123 Le Loi"
"#;
    let store = RuleStore::from_toml_str(custom_toml).unwrap();
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. Normal untouched words must NEVER be logged
    feed_keys(&mut session, &mut engine, &mut buf, "hello world ");
    assert!(engine.store().state().recent_events.is_empty());

    // 2. 1x and 2x AutocorrectApplied (`"ahve" -> "have"`)
    feed_keys(&mut session, &mut engine, &mut buf, "ahve ahve ");
    assert_eq!(
        engine.store().state().applied_counts.get("ahve->have"),
        Some(&2)
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::AutocorrectApplied {
            typed: "ahve".to_string(),
            applied: "have".to_string(),
            kind: MatchKind::KnownTypoDict,
            total_count: 2,
        }));

    // 3. Snippet undo 1x, 2x, 3x (`;addr -> 123 Le Loi` -> `<BS>` -> kept verbatim)
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        ";addr <BS> ;addr <BS> ;addr <BS> ",
    );
    assert_eq!(
        engine.store().state().undo_counts.get(";addr->123 le loi"),
        Some(&3)
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::BackspaceUndo {
            typed: ";addr".to_string(),
            rejected: "123 Le Loi".to_string(),
            resolution: UndoResolution::KeptVerbatim,
            undo_count: 3,
        }));

    // 4. Undo resolutions: SelfConfirmCancelled, ClauseErased, and EditedToThird
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "chagnes <BS><BS><BS><BS><BS>nges ",
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::BackspaceUndo {
            typed: "chagnes".to_string(),
            rejected: "changes".to_string(),
            resolution: UndoResolution::SelfConfirmCancelled,
            undo_count: 1,
        }));

    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "teh <BS><BS><BS><BS><BS>",
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::BackspaceUndo {
            typed: "teh".to_string(),
            rejected: "the".to_string(),
            resolution: UndoResolution::ClauseErased,
            undo_count: 1,
        }));

    // 5. 1x + 2x ManualBackspaceFix (`adpatr -> adapter`) + skipped DeltaSpanRewrite
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "adpatr <BS><BS><BS><BS><BS>apter adpatr <BS><BS><BS><BS><BS>apter ",
    );
    assert_eq!(
        engine
            .store()
            .state()
            .manual_fix_counts
            .get("adpatr->adapter"),
        Some(&2)
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::ManualBackspaceFix {
            orig: "adpatr".to_string(),
            fixed: "adapter".to_string(),
            outcome: "graduated".to_string(),
            attempt_count: 2,
        }));

    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "prodcut<BS><BS><BS>uction ",
    );
    assert!(engine
        .store()
        .state()
        .recent_events
        .contains(&DiagnosticEvent::ManualBackspaceFix {
            orig: "prodcut".to_string(),
            fixed: "production".to_string(),
            outcome: "skipped:delta_span_rewrite".to_string(),
            attempt_count: 1,
        }));

    // 6. Clearing diagnostic logs preserves graduated rules while wiping event feed
    engine.store_mut().clear_diagnostic_logs().unwrap();
    assert!(engine.store().state().recent_events.is_empty());
    assert!(engine.store().state().undo_counts.is_empty());
    assert_eq!(
        engine
            .store()
            .compiled()
            .corrections
            .get("adpatr")
            .map(String::as_str),
        Some("adapter")
    );
}

fn arb_key_input() -> impl Strategy<Value = KeyInput> {
    prop_oneof![
        // Common prose ASCII letters, spaces, punctuation, quotes, backticks
        prop::sample::select(vec![
            'a', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'm', 'n', 'o', 'p', 'r', 's', 't', 'u', ' ',
            ',', '.', ':', ';', '!', '?', '\'', '"', '`', '(', ')', '_', '-', '2',
        ])
        .prop_map(KeyInput::Char),
        // Multi-byte Unicode Vietnamese / emoji characters to stress `.chars().count()` invariants
        prop::sample::select(vec!['đ', 'ắ', 'ờ', 'ư', 'ệ', 'Đ', '’', '”']).prop_map(KeyInput::Char),
        Just(KeyInput::BackSpace),
        Just(KeyInput::CtrlBackSpace),
        Just(KeyInput::Enter),
        Just(KeyInput::NavOrClick),
        Just(KeyInput::FocusLost),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn fuzz_random_key_streams_never_panic_or_underflow_char_bounds(
        keys in prop::collection::vec(arb_key_input(), 1..160)
    ) {
        let store = RuleStore::in_memory(Preset::Balanced);
        // Reuse bundled lexicon without disk I/O
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut client_buf = String::new();

        for (step, key) in keys.into_iter().enumerate() {
            let out = session.process_key(&mut engine, key, (step as u64) * 50);
            if let SessionAction::Replace { delete_chars, ref insert } = out.action {
                let cur_chars = client_buf.chars().count();
                prop_assert!(
                    delete_chars <= cur_chars,
                    "step {step} ({key:?}): delete_chars={delete_chars} > client_buf chars={cur_chars} ({client_buf:?}), insert={insert:?}"
                );
            }
            apply_key_to_buffer(&mut client_buf, key, &out.action);
            // Verify `client_buf` remains valid UTF-8 (guaranteed by `String`, and `.chars().count()` never splits codepoints)
            prop_assert!(client_buf.len() >= client_buf.chars().count());
        }
    }
}

#[test]
fn test_proper_noun_capitalization_and_undo() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // "english " -> "English "
    feed_keys(&mut session, &mut engine, &mut buf, "english ");
    assert_eq!(buf, "English ");

    // 1x BackSpace undo -> snaps back to "english"
    feed_keys(&mut session, &mut engine, &mut buf, "<BS>");
    assert_eq!(buf, "english");

    // Keep it verbatim
    feed_keys(&mut session, &mut engine, &mut buf, " ");
    assert_eq!(buf, "english ");

    // "monday " -> "Monday "
    feed_keys(&mut session, &mut engine, &mut buf, "monday ");
    assert_eq!(buf, "english Monday ");
}

#[test]
fn test_slang_shielding_and_shorthand_expansion() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // Pure slang & clipped words (`onl`, `obv`, `notif`, `bruh`) must stay verbatim:
    feed_keys(&mut session, &mut engine, &mut buf, "onl obv notif bruh ");
    assert_eq!(buf, "onl obv notif bruh ");

    // Shorthand triggers (`ofc`, `rn`) auto-expand, and `1x BackSpace` reverts the entire phrase
    // and blocks that pair permanently so the next `rn ` stays `rn `:
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "ofc rn <BS> rn ");
    assert_eq!(buf, "of course rn rn ");
}

#[test]
fn test_unfixed_unknown_logging() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    feed_keys(&mut session, &mut engine, &mut buf, "xkzqw ");
    assert_eq!(buf, "xkzqw ");

    let st = engine.store().state();
    assert!(st.untouched_unknowns.contains_key("xkzqw"));
    assert!(st.recent_events.iter().any(|ev| matches!(
        ev,
        DiagnosticEvent::UnfixedUnknown { word, .. } if word == "xkzqw"
    )));
}

#[test]
fn test_diagnostics_config_and_jsonl_event_log() {
    static NEXT_DIAG_ID: AtomicU64 = AtomicU64::new(100);
    let tmp_dir = std::env::temp_dir().join(format!(
        "smartkey_diag_test_{}_{}",
        std::process::id(),
        NEXT_DIAG_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let rules_path = tmp_dir.join("rules.toml");
    let state_path = tmp_dir.join("learner_state.json");
    let log_dir = tmp_dir.join("logs");

    // Pre-create a stale log file that should be pruned on init
    fs::create_dir_all(&log_dir).unwrap();
    let stale_log = log_dir.join("events-2020-01-01.jsonl");
    let unrelated_file = log_dir.join("notes.txt");
    fs::write(&stale_log, "{}\n").unwrap();
    fs::write(&unrelated_file, "keep me\n").unwrap();

    // Configure log_level = "all" so WordUnchanged and RealWordDeclined are recorded
    let toml_content = Preset::Balanced
        .initial_toml()
        .replace("log_level = \"interesting\"", "log_level = \"all\"")
        .replace("near_miss_band = 0.5", "near_miss_band = 0.25")
        .replace("keep_days = 14", "keep_days = 7");
    fs::write(&rules_path, toml_content).unwrap();

    let mut store =
        RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced).unwrap();
    assert_eq!(
        store.compiled().diagnostics.log_level,
        smartkey_core::EventLogLevel::All
    );
    store.set_log_dir(Some(log_dir.clone()));

    store.log_app_bypass("kitty", smartkey_core::AppProfile::Bypass, "class_match");

    // Stale log file should be pruned on first write; unrelated file preserved
    assert!(!stale_log.exists(), "stale events-2020-01-01.jsonl should be pruned");
    assert!(unrelated_file.exists(), "non-event files must not be deleted");

    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    session.set_app(Some("kitty"));
    let mut buf = String::new();

    // 1. "sent form " -> "sent" is WordUnchanged, "form" triggers RealWordDeclined (candidate "from")
    feed_keys(&mut session, &mut engine, &mut buf, "sent form ");
    assert_eq!(buf, "sent form ");

    // 2. "teh " -> AutocorrectApplied ("the")
    feed_keys(&mut session, &mut engine, &mut buf, "teh ");
    assert_eq!(buf, "sent form the ");

    // Verify JSONL file was created and contains valid envelopes
    let entries: Vec<PathBuf> = fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with("events-") && s.ends_with(".jsonl"))
        })
        .collect();
    assert_eq!(entries.len(), 1, "expected exactly 1 daily JSONL file");

    let raw_jsonl = fs::read_to_string(&entries[0]).unwrap();
    let lines: Vec<serde_json::Value> = raw_jsonl
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid JSONL line"))
        .collect();

    assert!(lines.iter().all(|v| v["v"] == 1 && v["ts_ms"].is_u64()));
    assert!(lines.iter().any(|v| v["event"] == "app_bypass"
        && v["app"] == "kitty"
        && v["profile"] == "bypass"
        && v["reason"] == "class_match"));
    assert!(lines.iter().any(|v| v["event"] == "word_unchanged"
        && v["word"] == "sent"
        && v["app"] == "kitty"));
    assert!(lines.iter().any(|v| v["event"] == "real_word_declined"
        && v["prev"] == "sent"
        && v["word"] == "form"
        && v["candidate"] == "from"
        && v["candidate_score"].as_f64().unwrap() > v["typed_score"].as_f64().unwrap()));
    assert!(lines.iter().any(|v| v["event"] == "autocorrect_applied"
        && v["typed"] == "teh"
        && v["applied"] == "the"));

    // Verify learner_state.json recent_events only contains the 4 core variants (not bloated by WordUnchanged/RealWordDeclined)
    assert!(engine.store().state().recent_events.iter().all(|ev| !matches!(
        ev,
        DiagnosticEvent::WordUnchanged { .. }
            | DiagnosticEvent::RealWordDeclined { .. }
            | DiagnosticEvent::AppBypass { .. }
            | DiagnosticEvent::RuleLearned { .. }
    )));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_rule_supersession_and_toml_provenance_annotation() {
    static NEXT_SUP_ID: AtomicU64 = AtomicU64::new(200);
    let tmp_dir = std::env::temp_dir().join(format!(
        "smartkey_sup_test_{}_{}",
        std::process::id(),
        NEXT_SUP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let rules_path = tmp_dir.join("rules.toml");
    let state_path = tmp_dir.join("learner_state.json");
    let log_dir = tmp_dir.join("logs");

    let mut store =
        RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced).unwrap();
    store.set_log_dir(Some(log_dir.clone()));

    store.add_correction(&LangId::en(), "adpatr", "adapter");
    store.add_correction(&LangId::en(), "adpatr", "adaptor");

    let rules_raw = fs::read_to_string(&rules_path).unwrap();
    assert!(
        rules_raw.contains("# learned 2x ") && rules_raw.contains("— was: \"adapter\""),
        "expected provenance comment in rules.toml, got:\n{rules_raw}"
    );

    let entries: Vec<PathBuf> = fs::read_dir(&log_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|s| s.starts_with("events-") && s.ends_with(".jsonl"))
        })
        .collect();
    let raw_jsonl = fs::read_to_string(&entries[0]).unwrap();
    let lines: Vec<serde_json::Value> = raw_jsonl
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();

    assert!(lines.iter().any(|v| v["event"] == "rule_learned"
        && v["rule"] == "adpatr->adapter"
        && v["stage"] == "graduated_2x"
        && v["previous"].is_null()));
    assert!(lines.iter().any(|v| v["event"] == "rule_learned"
        && v["rule"] == "adpatr->adaptor"
        && v["stage"] == "superseded"
        && v["previous"] == "adapter"));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_unknown_target_2x_promotion_step5_personal_dict_and_2x_undictionary() {
    static NEXT_PROMO_ID: AtomicU64 = AtomicU64::new(300);
    let tmp_dir = std::env::temp_dir().join(format!(
        "smartkey_promo_test_{}_{}",
        std::process::id(),
        NEXT_PROMO_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let rules_path = tmp_dir.join("rules.toml");
    let state_path = tmp_dir.join("learner_state.json");
    let log_dir = tmp_dir.join("logs");

    let mut store =
        RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced).unwrap();
    store.set_log_dir(Some(log_dir.clone()));
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. 1x manual fix of unknown word `protmpcord -> promptcord`:
    // Must stage (1/2) and NOT immediately add `promptcord` to `personal_dict`!
    let evs1 = feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "protmpcord <CBS>promptcord ",
    );
    assert!(evs1.iter().any(|e| matches!(
        e,
        LearnEvent::StagedCorrection {
            orig,
            fixed,
            count: 1,
            ..
        } if orig == "protmpcord" && fixed == "promptcord"
    )));
    assert!(
        !engine.store().compiled().personal_dict.contains("promptcord"),
        "1x manual fix must not promote unknown target to personal_dict yet"
    );
    assert!(
        !engine.store().compiled().corrections.contains_key("protmpcord"),
        "1x manual fix must not graduate correction yet"
    );

    // 2. 2x manual fix of `protmpcord -> promptcord`:
    // Promotes `promptcord` to `personal_dict` AND graduates `protmpcord -> promptcord`!
    buf.clear();
    let evs2 = feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "protmpcord <CBS>promptcord ",
    );
    assert!(evs2.iter().any(|e| matches!(
        e,
        LearnEvent::GraduatedCorrection { orig, fixed, .. }
            if orig == "protmpcord" && fixed == "promptcord"
    )));
    assert!(engine.store().compiled().personal_dict.contains("promptcord"));
    assert_eq!(
        engine
            .store()
            .compiled()
            .corrections
            .get("protmpcord")
            .map(String::as_str),
        Some("promptcord")
    );

    // 3. Step 5f PersonalDict candidate generation:
    // Typing a brand-new adjacent-key typo `promptxord ` (never seen before) autocorrects to `promptcord `!
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "promptxord ");
    assert_eq!(buf, "promptcord ");

    // 4. Guards on unknown-target promotion:
    // Known typo `recieve` is rejected by `known_typos`
    buf.clear();
    let evs_typo = feed_keys(&mut session, &mut engine, &mut buf, "reciev <CBS>recieve ");
    assert!(evs_typo.contains(&LearnEvent::Ignored(SkipReason::TargetNotValidWord)));
    assert!(!engine.store().compiled().personal_dict.contains("recieve"));

    // Key-bounce extension `zyxqwert -> zyxqwertt` is rejected
    buf.clear();
    let evs_bounce = feed_keys(&mut session, &mut engine, &mut buf, "zyxqwert <CBS>zyxqwertt ");
    assert!(evs_bounce.contains(&LearnEvent::Ignored(SkipReason::TargetNotValidWord)));
    assert!(!engine.store().compiled().personal_dict.contains("zyxqwertt"));

    // 5. Automatic 2x "Un-dictionary":
    // Suppose `promptcord` in `personal_dict` was itself a mistake and the user fixes `promptcord -> promptcard` 2x:
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "promptcord <CBS>promptcard ",
    );
    // After 1x, `promptcord` is still in `personal_dict`
    assert!(engine.store().compiled().personal_dict.contains("promptcord"));

    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "promptcord <CBS>promptcard ",
    );
    // After 2x, `promptcord` is automatically un-dictionaried and replaced by `promptcard`,
    // and `promptcord -> promptcard` graduates to `[corrections]`!
    assert!(
        !engine.store().compiled().personal_dict.contains("promptcord"),
        "2x manual fix away from a personal_dict word must un-dictionary it"
    );
    assert!(engine.store().compiled().personal_dict.contains("promptcard"));
    assert_eq!(
        engine
            .store()
            .compiled()
            .corrections
            .get("promptcord")
            .map(String::as_str),
        Some("promptcard")
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_learner_state_serde_backward_compatibility() {
    static NEXT_COMPAT_ID: AtomicU64 = AtomicU64::new(400);
    let tmp_dir = std::env::temp_dir().join(format!(
        "smartkey_compat_test_{}_{}",
        std::process::id(),
        NEXT_COMPAT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let rules_path = tmp_dir.join("rules.toml");
    let state_path = tmp_dir.join("learner_state.json");

    let legacy_json = r#"{
  "version": 1,
  "staged_corrections": {},
  "staged_context": {},
  "untouched_unknowns": {},
  "trouble_words": {},
  "skipped_edits": [],
  "applied_counts": { "teh->the": 2 },
  "undo_counts": {},
  "manual_fix_counts": {},
  "recent_events": [
    {
      "event": "autocorrect_applied",
      "typed": "teh",
      "applied": "the",
      "kind": "known_typo_dict",
      "total_count": 2
    }
  ]
}"#;
    fs::write(&state_path, legacy_json).unwrap();

    let store =
        RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced).unwrap();
    assert_eq!(store.state().recent_events.len(), 1);
    assert_eq!(store.state().applied_counts.get("teh->the"), Some(&2));

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_undo_recovery_purges_bad_learned_correction_and_guard_vi_ascii_telex() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. Seed a bad learned rule `fomr -> forn` in `[corrections]`:
    engine
        .store_mut()
        .add_correction(&LangId::en(), "fomr", "forn");
    assert_eq!(
        engine.store().compiled().corrections.get("fomr").map(String::as_str),
        Some("forn")
    );

    // Next time `fomr ` autocorrects to `forn `, user presses `<BS>` and edits to `form `:
    // `add_permanent_block("fomr", "forn")` immediately deletes the bad `"fomr" = "forn"` rule!
    feed_keys(&mut session, &mut engine, &mut buf, "fomr <BS><BS><BS>rm ");
    assert_eq!(buf, "form ");
    assert!(
        !engine.store().compiled().corrections.contains_key("fomr"),
        "1x undo+edit must immediately purge bad learned [corrections] rule"
    );
    assert!(engine.store().is_pair_blocked("fomr", "forn"));

    // 2. In `lang = "vi"`, raw ASCII Telex tone-strip edits (`nhir -> nhi`, `maf -> ma`, `quas -> qua`)
    //    must NEVER be learned into `[lang.vi.corrections]`:
    session.set_lang(LangId::vi());
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "nhir <CBS>nhi nhir <CBS>nhi maf <CBS>ma maf <CBS>ma ",
    );
    assert!(
        engine
            .store()
            .compiled()
            .lang_corrections
            .get("vi")
            .is_none_or(|m| m.is_empty()),
        "raw ASCII Telex edits in vi mode must never pollute [lang.vi.corrections]"
    );
}

#[test]
fn test_canonical_casing_learning_and_typo_repair() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. MixedCaseWord ("PyTorch", "OpenClaw") and TitleCase tech word ("Hyprland") learn canonical casing at 3x untouched
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "PyTorch, PyTorch, PyTorch.<ENTER>",
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("pytorch")
            .map(String::as_str),
        Some("PyTorch")
    );
    assert!(engine.store().to_toml_string().contains("\"PyTorch\""));

    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "Hyprland, Hyprland; Hyprland.<ENTER>",
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("hyprland")
            .map(String::as_str),
        Some("Hyprland")
    );

    // 2. Mid-word edit before space ("OpenClaww<BS> ") still counts toward 3x untouched graduation
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "OpenClaww<BS> OpenClaw `code` OpenClaw <ENTER>",
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("openclaw")
            .map(String::as_str),
        Some("OpenClaw")
    );

    // 3. Typos ("pytroch", "Pytroch", "hypland", "Hypland") and lowercase ("pytorch", "hyprland")
    //    autocorrect to the canonical form ("PyTorch", "Hyprland") regardless of typo casing!
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "pytroch Pytroch pytorch hypland Hypland hyprland ",
    );
    assert_eq!(
        buf,
        "PyTorch PyTorch PyTorch Hyprland Hyprland Hyprland "
    );

    // 4. CapsLock inversion ("cHAGNES") and sentence-start late-shift ("cHagnes") repair typos to "Changes"
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "<ENTER>cHagnes cHAGNES ");
    assert_eq!(buf, "\nChanges Changes ");
}

#[test]
fn test_untouched_unknowns_cleanup_on_manual_fix_backspace_and_demotion() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. Immediate backspace cancels tentative unfixed_unknown counter
    feed_keys(&mut session, &mut engine, &mut buf, "zellij <CBS>nvim ");
    assert!(
        !engine.store().state().untouched_unknowns.contains_key("zellij"),
        "immediate Ctrl+BackSpace must cancel tentative untouched_unknowns entry"
    );

    // 2. Typing a typo 2x untouched and then manually fixing it clears untouched_unknowns
    //    so a later untouched slip cannot falsely graduate at 3x
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "zyxwvut zyxwvut zyxwvut <CBS>zyxwvuts ",
    );
    assert!(
        !engine
            .store()
            .state()
            .untouched_unknowns
            .contains_key("en:zyxwvut"),
        "manual fix must purge staged untouched_unknowns for the typo"
    );
    feed_keys(&mut session, &mut engine, &mut buf, "zyxwvut <ENTER>");
    assert!(
        !engine.store().compiled().personal_dict.contains("zyxwvut"),
        "cleared untouched_unknowns must not graduate on a single subsequent slip"
    );

    // 3. Demoting a personal_dict word clears untouched_unknowns so 1x subsequent use does not re-promote it
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "quxkorp quxkorp quxkorp <ENTER>",
    );
    assert!(engine.store().compiled().personal_dict.contains("quxkorp"));
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "quxkorp <CBS>quxcore quxkorp <CBS>quxcore ",
    );
    assert!(!engine.store().compiled().personal_dict.contains("quxkorp"));

    // Even if user unblocks/clears correction or types it once, untouched_unknowns was reset to 0
    assert!(
        !engine
            .store()
            .state()
            .untouched_unknowns
            .contains_key("en:quxkorp"),
        "remove_personal_word must clear untouched_unknowns counter"
    );

    // 4. Vietnamese toneless dictionary words ("anh", "em", "hai", "ba") do not log as untouched_unknowns
    session.set_lang(LangId::vi());
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "anh em hai ba <ENTER>");
    assert!(!engine.store().state().untouched_unknowns.contains_key("anh"));
    assert!(!engine.store().state().untouched_unknowns.contains_key("vi:anh"));
}
