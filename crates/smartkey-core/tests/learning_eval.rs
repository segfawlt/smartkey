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

        // 4. Row 2: Undo + Keep Verbatim (`1x` shields in session ONLY, NOT permanent on disk yet)
        buf.clear();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "alot <BS> ");
        assert_eq!(buf, "alot ");
        assert!(evs.contains(&LearnEvent::ShieldedWordSession {
            word: "alot".to_string()
        }));
        assert!(
            engine.store().compiled().personal_dict.is_empty(),
            "1x undo+keep must NOT permanently write personal_dict yet"
        );
        // Shielded for rest of Session 1:
        feed_keys(&mut session, &mut engine, &mut buf, "alot ");
        assert_eq!(buf, "alot alot ");

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

        // 9. Active `lang = "vi"` scopes `3x` untouched slang (`khum`) to `[lang.vi]`, NOT English!
        session.set_lang(LangId("vi".to_string()));
        feed_keys(
            &mut session,
            &mut engine,
            &mut buf,
            "khum mot khum hai khum ba <ENTER>",
        );
        assert!(engine
            .store()
            .compiled()
            .lang_personal_dict
            .get("vi")
            .is_some_and(|s| s.contains("khum")));
        assert!(!engine.store().compiled().personal_dict.contains("khum"));
    }

    // --- Session 2 (Reload from disk: verifies `2x` Undo+Keep permanent graduation & comment preservation) ---
    {
        let store =
            RuleStore::load_or_init(rules_path.clone(), state_path.clone(), Preset::Balanced)
                .unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        // In Session 2, `alot` was only 1x Undo+Keep in Session 1, so it autocorrects once more:
        // Undoing + keeping a 2nd time graduates `alot` permanently to `blocklist` + `personal_dict`!
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "alot <BS> ");
        assert_eq!(buf, "alot ");
        assert!(evs.contains(&LearnEvent::GraduatedUndoKeep {
            typed: "alot".to_string(),
            rejected: "a lot".to_string(),
        }));
        assert!(engine.store().compiled().personal_dict.contains("alot"));
        assert!(engine.store().is_pair_blocked("alot", "a lot"));
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
fn test_slang_shielding_ofc() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // "ofc " must NOT become "of "
    feed_keys(&mut session, &mut engine, &mut buf, "ofc ");
    assert_eq!(buf, "ofc ");
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
