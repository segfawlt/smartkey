use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use proptest::prelude::*;
use serde::Deserialize;
use smartkey_core::{
    apply_key_to_buffer, Candidate, DiagnosticEvent, EvalContext, EvalOutcome, KeyInput, LangId,
    LearnEvent, MatchKind, Preset, RuleStore, SessionAction, SkipReason, SmartKeyEngine,
    SmartKeySession, SpaceHabitKind, UndoResolution, UserRuleView,
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
    #[serde(default)]
    lang: Option<String>,
    steps: Vec<TraceStep>,
}

#[derive(Debug, Deserialize)]
struct TraceStep {
    keys: String,
    expect_buf: String,
}

fn feed_lotus_keys(
    session: &mut SmartKeySession,
    engine: &mut SmartKeyEngine,
    client_buf: &mut String,
    cur_word_keys: &mut String,
    spec: &str,
) {
    let opts = engine.store().lotus_options();
    for key in parse_key_stream(spec) {
        match key {
            KeyInput::Char(ch) if !matches!(ch, ' ' | ',' | '.' | '!' | '?' | ';' | ':') => {
                cur_word_keys.push(ch);
                let out = session.process_key(engine, KeyInput::Char(ch), 100);
                assert_eq!(out.action, SessionAction::PassThrough);
            }
            KeyInput::Char(ch) => {
                if !cur_word_keys.is_empty() {
                    let sim = smartkey_core::simulate_lotus_word_with_options(cur_word_keys, opts);
                    cur_word_keys.clear();
                    if sim.needs_uinput_restore_on_break {
                        // Real Lotus filters the boundary key, backspaces runic preedit, and commits
                        // `committed_at_break + ch` via `CommitStringEvent`:
                        client_buf.push_str(&sim.committed_at_break);
                        client_buf.push(ch);
                        let mut committed = sim.committed_at_break.clone();
                        committed.push(ch);
                        let out = session.on_lotus_boundary_commit(engine, ch, &committed, 120);
                        if let SessionAction::Replace {
                            delete_chars,
                            insert,
                        } = &out.action
                        {
                            for _ in 0..*delete_chars {
                                client_buf.pop();
                            }
                            client_buf.push_str(insert);
                        }
                        continue;
                    }
                    if sim.is_composed_vietnamese {
                        client_buf.push_str(&sim.committed_at_break);
                    } else {
                        client_buf.push_str(&sim.preedit_before_break);
                    }
                }
                let out = session.process_key(engine, KeyInput::Char(ch), 120);
                apply_key_to_buffer(client_buf, KeyInput::Char(ch), &out.action);
            }
            other => {
                cur_word_keys.clear();
                let out = session.process_key(engine, other, 120);
                apply_key_to_buffer(client_buf, other, &out.action);
            }
        }
    }
}

#[test]
fn golden_learning_traces_pass() {
    let raw = fs::read_to_string(workspace_root().join("tests/corpus/learning_traces.toml"))
        .expect("read learning_traces.toml");
    let parsed: TraceFile = toml_edit::de::from_str(&raw).expect("parse learning_traces.toml");

    for case in &parsed.trace {
        let mut engine = SmartKeyEngine::in_memory_default().expect("bundled engine");
        let mut session = SmartKeySession::new();
        let is_vi = case.lang.as_deref() == Some("vi");
        if is_vi {
            session.set_lang(LangId::vi());
        }
        let mut buf = String::new();
        let mut cur_word_keys = String::new();

        for (idx, step) in case.steps.iter().enumerate() {
            if is_vi {
                feed_lotus_keys(
                    &mut session,
                    &mut engine,
                    &mut buf,
                    &mut cur_word_keys,
                    &step.keys,
                );
            } else {
                feed_keys(&mut session, &mut engine, &mut buf, &step.keys);
            }
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

        // 1. Snippet undo reverts WITHOUT blocklisting `addr`
        feed_keys(&mut session, &mut engine, &mut buf, "addr <BS><BS> ");
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
            "chagnes <BS><BS><BS><BS><BS><BS>nges ",
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
            "chagnes <BS><BS><BS><BS><BS><BS><BS><BS><BS>hello ",
        );
        assert_eq!(buf, "hello ");
        assert!(
            !engine.store().is_pair_blocked("chagnes", "changes"),
            "clearing input to write a different word must not poison blocklist"
        );

        // 4. Row 2: Undo + Keep Verbatim (`1x` immediately blocks the pair on disk so the user
        //    never has to backspace a 2nd time, while `personal_dict` waits for a 2nd confirmation)
        buf.clear();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "alot <BS><BS> ");
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

    // 3. Snippet undo 1x, 2x, 3x (`;addr -> 123 Le Loi` -> `<BS><BS>` -> kept verbatim)
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        ";addr <BS><BS> ;addr <BS><BS> ;addr <BS><BS> ",
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
        "chagnes <BS><BS><BS><BS><BS><BS>nges ",
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
        "teh <BS><BS><BS><BS><BS><BS>",
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

    // 2x BackSpace undo under default step mode -> 1st deletes space, 2nd snaps back to "english"
    feed_keys(&mut session, &mut engine, &mut buf, "<BS><BS>");
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

    // Shorthand triggers (`ofc`, `rn`) auto-expand, and step undo reverts the entire phrase
    // and blocks that pair permanently so the next `rn ` stays `rn `:
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "ofc rn <BS><BS> rn ");
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
    assert!(
        !stale_log.exists(),
        "stale events-2020-01-01.jsonl should be pruned"
    );
    assert!(
        unrelated_file.exists(),
        "non-event files must not be deleted"
    );

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
    assert!(lines
        .iter()
        .any(|v| v["event"] == "word_unchanged" && v["word"] == "sent" && v["app"] == "kitty"));
    assert!(lines.iter().any(|v| v["event"] == "real_word_declined"
        && v["prev"] == "sent"
        && v["word"] == "form"
        && v["candidate"] == "from"
        && v["candidate_score"].as_f64().unwrap() > v["typed_score"].as_f64().unwrap()));
    assert!(lines.iter().any(|v| v["event"] == "autocorrect_applied"
        && v["typed"] == "teh"
        && v["applied"] == "the"));

    // Verify learner_state.json recent_events only contains the 4 core variants (not bloated by WordUnchanged/RealWordDeclined)
    assert!(engine
        .store()
        .state()
        .recent_events
        .iter()
        .all(|ev| !matches!(
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
        !engine
            .store()
            .compiled()
            .personal_dict
            .contains("promptcord"),
        "1x manual fix must not promote unknown target to personal_dict yet"
    );
    assert!(
        !engine
            .store()
            .compiled()
            .corrections
            .contains_key("protmpcord"),
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
    assert!(engine
        .store()
        .compiled()
        .personal_dict
        .contains("promptcord"));
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
    let evs_bounce = feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "zyxqwert <CBS>zyxqwertt ",
    );
    assert!(evs_bounce.contains(&LearnEvent::Ignored(SkipReason::TargetNotValidWord)));
    assert!(!engine
        .store()
        .compiled()
        .personal_dict
        .contains("zyxqwertt"));

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
    assert!(engine
        .store()
        .compiled()
        .personal_dict
        .contains("promptcord"));

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
        !engine
            .store()
            .compiled()
            .personal_dict
            .contains("promptcord"),
        "2x manual fix away from a personal_dict word must un-dictionary it"
    );
    assert!(engine
        .store()
        .compiled()
        .personal_dict
        .contains("promptcard"));
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
        engine
            .store()
            .compiled()
            .corrections
            .get("fomr")
            .map(String::as_str),
        Some("forn")
    );

    // Next time `fomr ` autocorrects to `forn `, user steps back and edits to `form `:
    // `add_permanent_block("fomr", "forn")` immediately deletes the bad `"fomr" = "forn"` rule!
    feed_keys(&mut session, &mut engine, &mut buf, "fomr <BS><BS><BS><BS>rm ");
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
    assert_eq!(buf, "PyTorch PyTorch PyTorch Hyprland Hyprland Hyprland ");

    // 4. CapsLock inversion ("cHAGNES") and sentence-start late-shift ("cHagnes") repair typos to "Changes"
    buf.clear();
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "<ENTER>cHagnes cHAGNES ",
    );
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
        !engine
            .store()
            .state()
            .untouched_unknowns
            .contains_key("zellij"),
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
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("anh"));
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("vi:anh"));
}

// --- 2x evidence for blocking curated unconditional contractions ---

fn contraction_tmp_paths(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let dir = std::env::temp_dir().join(format!(
        "smartkey_contraction_{tag}_{}_{}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    (
        dir.clone(),
        dir.join("rules.toml"),
        dir.join("learner_state.json"),
    )
}

fn contraction_engine(rules: &Path, state: &Path) -> SmartKeyEngine {
    let store = RuleStore::load_or_init(rules.to_path_buf(), state.to_path_buf(), Preset::Balanced)
        .unwrap();
    SmartKeyEngine::new_bundled(store).unwrap()
}

fn reject_count(engine: &SmartKeyEngine) -> u32 {
    engine
        .store()
        .state()
        .staged_contraction_rejects
        .get("en:dont->don't")
        .copied()
        .unwrap_or(0)
}

#[test]
fn contraction_predicate_matches_only_curated_pairs() {
    use smartkey_core::scorer::is_unconditional_contraction_pair as p;
    assert!(p("dont", "don't"));
    assert!(p("im", "I'm"));
    assert!(p("im", "i'm"));
    assert!(!p("alot", "a lot"));
    assert!(!p("dont", "doesn't"));
}

#[test]
fn contraction_undo_keep_needs_2x_across_sessions_to_block() {
    let (dir, rules, state) = contraction_tmp_paths("undo_keep");

    // Session 1: 1x undo-keep stages only.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "dont <BS><BS> ");
        assert_eq!(buf, "dont ");
        assert!(evs.contains(&LearnEvent::ShieldedWordSession {
            word: "dont".to_string()
        }));
        assert_eq!(reject_count(&engine), 1);
        assert!(
            engine.store().state().staged_undo_keep.is_empty(),
            "1x must not persist an undo-keep key (it acts as a cross-session shield)"
        );
        assert!(!engine
            .store()
            .compiled()
            .blocklist
            .contains(&("dont".to_string(), "don't".to_string())));
    }

    // Session 2: rule still fires; 2nd undo-keep blocks and graduates in lockstep.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, "dont ");
        assert_eq!(
            buf, "don't ",
            "1x accident must not silence the curated rule"
        );

        buf.clear();
        let mut session = SmartKeySession::new();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "dont <BS><BS> ");
        assert_eq!(buf, "dont ");
        assert_eq!(reject_count(&engine), 2);
        assert!(engine
            .store()
            .compiled()
            .blocklist
            .contains(&("dont".to_string(), "don't".to_string())));
        assert!(
            !evs.iter()
                .any(|e| matches!(e, LearnEvent::GraduatedUndoKeep { .. })),
            "contraction rejection must not promote the raw form into personal_dict: {evs:?}"
        );
        assert!(!engine.store().compiled().personal_dict.contains("dont"));
    }

    // Session 3: blocked.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, "dont ");
        assert_eq!(buf, "dont ");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn contraction_edit_to_third_word_blocks_only_at_2x() {
    let (dir, rules, state) = contraction_tmp_paths("third");
    for round in 1..=2u32 {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "dont <BS><BS><BS><BS>nut ");
        let blocked_ev = evs.iter().any(
            |e| matches!(e, LearnEvent::BlockedPairPermanent { typed, .. } if typed == "dont"),
        );
        assert_eq!(reject_count(&engine), round, "{evs:?}");
        assert_eq!(
            engine
                .store()
                .compiled()
                .blocklist
                .contains(&("dont".to_string(), "don't".to_string())),
            round == 2
        );
        assert_eq!(blocked_ev, round == 2, "{evs:?}");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn contraction_manual_fix_graduation_clears_stale_block() {
    let (dir, rules, state) = contraction_tmp_paths("deadlock");
    {
        let mut engine = contraction_engine(&rules, &state);
        engine.store_mut().add_permanent_block("dont", "don't");
        assert!(engine
            .store()
            .compiled()
            .blocklist
            .contains(&("dont".to_string(), "don't".to_string())));
    }
    for _ in 0..2 {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "dont <CBS>don't ");
        assert_eq!(buf, "don't ", "{evs:?}");
    }
    let mut engine = contraction_engine(&rules, &state);
    assert!(!engine
        .store()
        .compiled()
        .blocklist
        .contains(&("dont".to_string(), "don't".to_string())));
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    feed_keys(&mut session, &mut engine, &mut buf, "dont ");
    assert_eq!(buf, "don't ");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn non_contraction_undo_keep_still_blocks_at_1x() {
    let (dir, rules, state) = contraction_tmp_paths("alot");
    let mut engine = contraction_engine(&rules, &state);
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    feed_keys(&mut session, &mut engine, &mut buf, "alot <BS><BS> ");
    assert!(engine.store().is_pair_blocked("alot", "a lot"));
    assert!(engine.store().state().staged_contraction_rejects.is_empty());
    let _ = fs::remove_dir_all(&dir);
}

fn contraction_blocked(engine: &SmartKeyEngine) -> bool {
    engine
        .store()
        .compiled()
        .blocklist
        .contains(&("dont".to_string(), "don't".to_string()))
}

/// Runs one session that types `keys` against a freshly loaded store.
fn contraction_session(rules: &Path, state: &Path, keys: &str) -> (SmartKeyEngine, String) {
    let mut engine = contraction_engine(rules, state);
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    feed_keys(&mut session, &mut engine, &mut buf, keys);
    (engine, buf)
}

#[test]
fn contraction_rejection_goes_stale_after_many_accepted_corrections() {
    let (dir, rules, state) = contraction_tmp_paths("decay");

    // Rejection 1, then 12 corrections accepted, then rejection 2: the old rejection is stale.
    let (engine, _) = contraction_session(&rules, &state, "dont <BS><BS> ");
    assert_eq!(reject_count(&engine), 1);
    let (_, buf) = contraction_session(&rules, &state, &"dont ".repeat(12));
    assert_eq!(buf, "don't ".repeat(12));
    let (engine, _) = contraction_session(&rules, &state, "dont <BS><BS> ");
    assert_eq!(reject_count(&engine), 1, "stale evidence must restart at 1");
    assert!(!contraction_blocked(&engine));

    // A rejection soon after the previous one still accumulates to a block.
    let (engine, _) = contraction_session(&rules, &state, "dont <BS><BS> ");
    assert_eq!(reject_count(&engine), 2);
    assert!(contraction_blocked(&engine));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn contraction_rejections_close_together_still_block() {
    let (dir, rules, state) = contraction_tmp_paths("close");
    contraction_session(&rules, &state, "dont <BS><BS> ");
    // 3 accepted corrections is below the staleness threshold.
    contraction_session(&rules, &state, &"dont ".repeat(3));
    let (engine, _) = contraction_session(&rules, &state, "dont <BS><BS> ");
    assert_eq!(reject_count(&engine), 2);
    assert!(contraction_blocked(&engine));
    let _ = fs::remove_dir_all(&dir);
}

// --- Case-only (proper noun) corrections: 2x rejection evidence, pin override ---

fn case_pair_blocked(engine: &SmartKeyEngine) -> bool {
    engine
        .store()
        .compiled()
        .blocklist
        .contains(&("english".to_string(), "english".to_string()))
}

#[test]
fn case_only_undo_keep_needs_2x_across_sessions_to_block() {
    let (dir, rules, state) = contraction_tmp_paths("case_only");

    // Session 1: 1x deliberate keep-verbatim stages only; it is not mistaken for an accident.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        let evs = feed_keys(&mut session, &mut engine, &mut buf, "english <BS><BS> ");
        assert_eq!(buf, "english ");
        assert!(
            !evs.contains(&LearnEvent::SelfConfirmCancelled),
            "case-only keep-verbatim must not be read as an accidental BackSpace: {evs:?}"
        );
        assert!(!case_pair_blocked(&engine));
    }

    // Session 2: the curated rule still fires after one rejection; the 2nd rejection blocks.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, "english ");
        assert_eq!(
            buf, "English ",
            "1x rejection must not disable the curated rule"
        );

        buf.clear();
        let mut session = SmartKeySession::new();
        feed_keys(&mut session, &mut engine, &mut buf, "english <BS><BS> ");
        assert_eq!(buf, "english ");
        assert!(case_pair_blocked(&engine));
    }

    // Session 3: blocked.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, "english ");
        assert_eq!(buf, "english ");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn pinned_word_ignores_rejections_and_existing_blocks() {
    let (dir, rules, state) = contraction_tmp_paths("pinned");
    let mut engine = contraction_engine(&rules, &state);
    engine.store_mut().add_user_pinned_word("English").unwrap();
    engine
        .store_mut()
        .add_user_blocklist_pair("english", "english")
        .unwrap();
    assert!(case_pair_blocked(&engine));

    for _ in 0..3 {
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, "english <BS><BS> ");
        assert_eq!(buf, "english ");
        let mut session = SmartKeySession::new();
        buf.clear();
        feed_keys(&mut session, &mut engine, &mut buf, "english ");
        assert_eq!(buf, "English ", "pinned word must always capitalize");
    }

    // Survives a reload from disk, and unpinning restores the block.
    let mut engine = contraction_engine(&rules, &state);
    assert!(engine.store().is_pinned("english"));
    assert!(engine
        .store_mut()
        .remove_user_pinned_word("english")
        .unwrap());
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    feed_keys(&mut session, &mut engine, &mut buf, "english ");
    assert_eq!(buf, "english ");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn polish_homograph_is_left_alone() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    feed_keys(&mut session, &mut engine, &mut buf, "polish the code ");
    assert_eq!(buf, "polish the code ");
    feed_keys(&mut session, &mut engine, &mut buf, "Polish ");
    assert_eq!(buf, "polish the code Polish ");
}

#[test]
fn f1_novel_oov_1x_session_shield_and_2x_fast_track_preserves_canonical_case() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1x manual fix to novel OOV `"Domixi"`: staged (not in personal_dict yet),
    // but shields `"domixi"` in-session and credits +2 toward untouched promotion.
    let evs1 = feed_keys(&mut session, &mut engine, &mut buf, "dmoxii <CBS>Domixi ");
    assert!(evs1.iter().any(|e| matches!(
        e,
        LearnEvent::StagedCorrection {
            orig,
            fixed,
            count: 1,
            ..
        } if orig == "dmoxii" && fixed == "Domixi"
    )));
    assert!(
        !engine.store().compiled().personal_dict.contains("domixi"),
        "1x fix must NOT graduate to personal_dict immediately"
    );

    // One untouched use in the same session reaches 3/3 and preserves canonical `"Domixi"`.
    feed_keys(&mut session, &mut engine, &mut buf, "domixi ok ");
    assert!(
        engine.store().compiled().personal_dict.contains("domixi"),
        "1x fix + 1x untouched must fast-track graduate to personal_dict"
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("domixi")
            .map(String::as_str),
        Some("Domixi")
    );

    let mut next_session = SmartKeySession::new();
    let mut next_buf = String::new();
    feed_keys(
        &mut next_session,
        &mut engine,
        &mut next_buf,
        "hello domixi ",
    );
    assert_eq!(next_buf, "hello Domixi ");
}

#[test]
fn f1_case_only_manual_edit_learns_tech_and_oov_but_ignores_ordinary_words() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

    // Ordinary prose words (`hello -> Hello`, `polish -> Polish`) must be ignored (`SameWord`).
    for _ in 0..3 {
        let mut s = SmartKeySession::new();
        let mut b = String::new();
        feed_keys(&mut s, &mut engine, &mut b, "hello <CBS>Hello ");
        feed_keys(&mut s, &mut engine, &mut b, "polish <CBS>Polish ");
    }
    assert!(!engine.store().compiled().personal_dict.contains("hello"));
    assert!(!engine.store().compiled().personal_dict.contains("polish"));
    {
        let mut s = SmartKeySession::new();
        let mut b = String::new();
        feed_keys(
            &mut s,
            &mut engine,
            &mut b,
            "we hello world and polish the code ",
        );
        assert_eq!(b, "we hello world and polish the code ");
    }

    // Intentional TitleCase novel word (`hyprland -> Hyprland`) stages at 1x and learns at 2x.
    let mut s1 = SmartKeySession::new();
    let mut b1 = String::new();
    let ev1 = feed_keys(&mut s1, &mut engine, &mut b1, "hyprland <CBS>Hyprland ");
    assert!(ev1.iter().any(|e| matches!(
        e,
        LearnEvent::StagedCorrection {
            fixed,
            count: 1,
            ..
        } if fixed == "Hyprland"
    )));
    assert!(!engine.store().compiled().personal_dict.contains("hyprland"));

    let mut s2 = SmartKeySession::new();
    let mut b2 = String::new();
    let ev2 = feed_keys(&mut s2, &mut engine, &mut b2, "hyprland <CBS>Hyprland ");
    assert!(ev2.iter().any(|e| matches!(
        e,
        LearnEvent::GraduatedPersonalWord { word, .. } if word == "Hyprland"
    )));
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("hyprland")
            .map(String::as_str),
        Some("Hyprland")
    );
    let mut s3 = SmartKeySession::new();
    let mut b3 = String::new();
    feed_keys(&mut s3, &mut engine, &mut b3, "using hyprland ");
    assert_eq!(b3, "using Hyprland ");

    // Lowercase-first personal_dict (`"domixv"` x3) upgrades to `"Domixv"` at 2x intentional mid-sentence cased typing.
    let mut s4 = SmartKeySession::new();
    let mut b4 = String::new();
    feed_keys(
        &mut s4,
        &mut engine,
        &mut b4,
        "hi domixv ok domixv ok domixv ok ",
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("domixv")
            .map(String::as_str),
        Some("domixv")
    );
    let mut s5 = SmartKeySession::new();
    let mut b5 = String::new();
    feed_keys(&mut s5, &mut engine, &mut b5, "hi Domixv ok hi Domixv ok ");
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("domixv")
            .map(String::as_str),
        Some("Domixv")
    );
    let mut s6 = SmartKeySession::new();
    let mut b6 = String::new();
    feed_keys(&mut s6, &mut engine, &mut b6, "meet domixv ");
    assert_eq!(b6, "meet Domixv ");
}

#[test]
fn f3_personal_word_usage_boosts_typo_rescue_tiebreak_after_reset() {
    static NEXT_F3_ID: AtomicU64 = AtomicU64::new(900);
    let dir = std::env::temp_dir().join(format!(
        "smartkey_f3_test_{}_{}",
        std::process::id(),
        NEXT_F3_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");

    let mut store = RuleStore::load_or_init(rules_path, state_path, Preset::Balanced).unwrap();
    store
        .add_user_personal_word(&LangId::en(), "zarqv")
        .unwrap();
    store
        .add_user_personal_word(&LangId::en(), "zarqb")
        .unwrap();
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    assert_eq!(engine.store().personal_word_freq("zarqv"), 175);
    assert_eq!(engine.store().personal_word_freq("zarqb"), 175);

    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    for _ in 0..15 {
        feed_keys(&mut session, &mut engine, &mut buf, "zarqb ");
    }
    session.reset(&mut engine);

    let freq_b = engine.store().personal_word_freq("zarqb");
    let freq_v = engine.store().personal_word_freq("zarqv");
    assert!(
        freq_b > freq_v && freq_b <= 225,
        "expected flushed usage boost for zarqb (got b={freq_b}, v={freq_v})"
    );

    let mut test_session = SmartKeySession::new();
    let mut test_buf = String::new();
    feed_keys(&mut test_session, &mut engine, &mut test_buf, "zarqn ");
    assert_eq!(
        test_buf, "zarqb ",
        "more frequently used personal_dict word must win edit-distance tie"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn reconcile_surrounding_guards_displaced_cursor_undo() {
    let mut engine = SmartKeyEngine::in_memory_default().expect("bundled engine");

    // 1. Staying at the corrected word boundary keeps stepwise BackSpace undo alive:
    let mut session = SmartKeySession::new();
    let mut buf = String::new();
    for (i, ch) in "teh ".chars().enumerate() {
        let out = session.process_key(&mut engine, KeyInput::Char(ch), 1_000 + (i as u64) * 20);
        apply_key_to_buffer(&mut buf, KeyInput::Char(ch), &out.action);
    }
    assert_eq!(buf, "the ");
    session.reconcile_surrounding(&mut engine, "hello the ", 10, 1_300);
    let bs_step = session.process_key(&mut engine, KeyInput::BackSpace, 1_320);
    assert_eq!(bs_step.action, SessionAction::PassThrough);
    session.reconcile_surrounding(&mut engine, "hello the", 9, 1_500);
    let bs = session.process_key(&mut engine, KeyInput::BackSpace, 1_520);
    assert!(
        matches!(bs.action, SessionAction::Replace { ref insert, .. } if insert == "teh"),
        "expected undo Replace when cursor stayed after 'the', got {:?}",
        bs.action
    );

    // 2. Clicking elsewhere before pressing BackSpace resets JustAutocorrected so BackSpace
    //    passes through normally instead of deleting/inserting 'teh' at the displaced cursor:
    let mut session2 = SmartKeySession::new();
    let mut buf2 = String::new();
    for (i, ch) in "teh ".chars().enumerate() {
        let out = session2.process_key(&mut engine, KeyInput::Char(ch), 2_000 + (i as u64) * 20);
        apply_key_to_buffer(&mut buf2, KeyInput::Char(ch), &out.action);
    }
    assert_eq!(buf2, "the ");
    session2.reconcile_surrounding(&mut engine, "other text", 5, 2_300);
    let bs2 = session2.process_key(&mut engine, KeyInput::BackSpace, 2_320);
    assert_eq!(bs2.action, SessionAction::PassThrough);

    // 3. Moving cursor to index 0 also clears JustAutocorrected:
    let mut session3 = SmartKeySession::new();
    let mut buf3 = String::new();
    for (i, ch) in "teh ".chars().enumerate() {
        let out = session3.process_key(&mut engine, KeyInput::Char(ch), 3_000 + (i as u64) * 20);
        apply_key_to_buffer(&mut buf3, KeyInput::Char(ch), &out.action);
    }
    assert_eq!(buf3, "the ");
    session3.reconcile_surrounding(&mut engine, "the ", 0, 3_300);
    let bs3 = session3.process_key(&mut engine, KeyInput::BackSpace, 3_320);
    assert_eq!(bs3.action, SessionAction::PassThrough);
}

#[test]
fn sentence_start_typo_fix_never_promotes_ordinary_word_to_title_case_personal_dict() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

    for _ in 0..2 {
        let mut s = SmartKeySession::new();
        let mut b = String::new();
        feed_keys(&mut s, &mut engine, &mut b, "t <CBS>It ");
        feed_keys(&mut s, &mut engine, &mut b, "itq <CBS>It ");
    }

    assert!(
        !engine.store().compiled().personal_dict.contains("it"),
        "ordinary lexicon word 'it' must never promote to personal_dict from sentence-start fix"
    );
    assert!(
        !engine.store().compiled().corrections.contains_key("t"),
        "1-char trigger 't' must not graduate to [corrections]"
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .corrections
            .get("itq")
            .map(String::as_str),
        Some("it"),
        "sentence-start fix 'itq -> It' must store lowercase 'it'"
    );

    let mut check_s = SmartKeySession::new();
    let mut check_b = String::new();
    feed_keys(
        &mut check_s,
        &mut engine,
        &mut check_b,
        "we itq saw it and it works ",
    );
    assert_eq!(check_b, "we it saw it and it works ");

    // Even if an ordinary non-tech lexicon word like "It" were seeded directly into personal_dict,
    // Step 3 must not force-capitalize mid-sentence "it" unless pinned.
    engine
        .store_mut()
        .add_user_personal_word(&LangId::en(), "It")
        .unwrap();
    let mut seeded_s = SmartKeySession::new();
    let mut seeded_b = String::new();
    feed_keys(&mut seeded_s, &mut engine, &mut seeded_b, "we saw it ");
    assert_eq!(seeded_b, "we saw it ");
}

#[test]
fn uppercase_to_lowercase_manual_edit_logs_shields_and_downgrades_personal_case() {
    let (dir, rules, state) = contraction_tmp_paths("uncase_manual");
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut s = SmartKeySession::new();
        let mut b = String::new();
        feed_keys(
            &mut s,
            &mut engine,
            &mut b,
            "Domixi, Domixi, Domixi.<ENTER>",
        );
        assert_eq!(
            engine
                .store()
                .compiled()
                .personal_dict_canonical
                .get("domixi")
                .map(String::as_str),
            Some("Domixi")
        );

        let mut check_s = SmartKeySession::new();
        let mut check_b = String::new();
        feed_keys(&mut check_s, &mut engine, &mut check_b, "hi domixi ");
        assert_eq!(check_b, "hi Domixi ");

        // 1x manual fix Domixi -> domixi: logs, stages, and shields in-session immediately.
        let mut fix1_s = SmartKeySession::new();
        let mut fix1_b = String::new();
        let evs1 = feed_keys(&mut fix1_s, &mut engine, &mut fix1_b, "Domixi <CBS>domixi ");
        assert!(evs1.iter().any(|e| matches!(
            e,
            LearnEvent::StagedCorrection {
                orig,
                fixed,
                count: 1,
                ..
            } if orig == "Domixi" && fixed == "domixi"
        )));
        assert!(
            engine.store().state().recent_events.iter().any(|ev| {
                matches!(
                    ev,
                    DiagnosticEvent::ManualBackspaceFix { orig, fixed, outcome, .. }
                    if orig == "Domixi" && fixed == "domixi" && outcome == "staged (1x)"
                )
            }),
            "1x uppercase->lowercase manual fix must be logged"
        );

        let mut same_s = SmartKeySession::new();
        let mut same_b = String::new();
        feed_keys(&mut same_s, &mut engine, &mut same_b, "hi domixi ");
        assert_eq!(same_b, "hi domixi ");
    }

    // 2x manual fix in a new session permanently blocks ("domixi", "domixi") and downgrades canonical case.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut fix2_s = SmartKeySession::new();
        let mut fix2_b = String::new();
        let evs2 = feed_keys(&mut fix2_s, &mut engine, &mut fix2_b, "Domixi <CBS>domixi ");
        assert!(evs2.iter().any(|e| matches!(
            e,
            LearnEvent::BlockedPairPermanent { typed, rejected }
            if typed == "domixi" && rejected == "domixi"
        )));
        assert_eq!(
            engine
                .store()
                .compiled()
                .personal_dict_canonical
                .get("domixi")
                .map(String::as_str),
            Some("domixi"),
            "downgraded personal_dict entry must remain in lowercase"
        );
        assert!(engine.store().is_pair_blocked("domixi", "domixi"));
    }

    // Fresh session reloaded from disk leaves "domixi" lowercase, and Hello->hello stays ignored.
    {
        let mut engine = contraction_engine(&rules, &state);
        let mut fresh_s = SmartKeySession::new();
        let mut fresh_b = String::new();
        feed_keys(&mut fresh_s, &mut engine, &mut fresh_b, "hi domixi ");
        assert_eq!(fresh_b, "hi domixi ");

        for _ in 0..2 {
            let mut h_s = SmartKeySession::new();
            let mut h_b = String::new();
            let evs = feed_keys(&mut h_s, &mut engine, &mut h_b, "Hello <CBS>hello ");
            assert!(
                evs.contains(&LearnEvent::Ignored(SkipReason::SameWord)),
                "ordinary Hello->hello must stay ignored: {evs:?}"
            );
        }
        assert!(!engine.store().is_pair_blocked("hello", "hello"));
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn personal_dict_case_undo_keep_blocks_at_1x_without_decay_and_ctrl_bs_tracks_undo() {
    let (dir, rules, state) = contraction_tmp_paths("personal_undo_1x");
    {
        let mut engine = contraction_engine(&rules, &state);
        engine
            .store_mut()
            .add_user_personal_word(&LangId::en(), "Domixi")
            .unwrap();
        engine
            .store_mut()
            .add_user_personal_word(&LangId::en(), "Zarqv")
            .unwrap();

        let mut s = SmartKeySession::new();
        let mut b = String::new();
        feed_keys(&mut s, &mut engine, &mut b, "domixi <BS><BS> ");
        assert_eq!(b, "domixi ");
        assert!(
            engine.store().is_pair_blocked("domixi", "domixi"),
            "1x undo-keep on personal_dict capitalization must block immediately without 2x staging"
        );
        assert!(!engine
            .store()
            .state()
            .staged_contraction_rejects
            .contains_key("en:domixi->domixi"));

        // Ctrl+BackSpace right after an autocorrect triggers undo tracking and EditingRestored.
        let mut s2 = SmartKeySession::new();
        let mut b2 = String::new();
        feed_keys(&mut s2, &mut engine, &mut b2, "zarqv <CBS>zarqv ");
        assert_eq!(b2, "zarqv ");
        assert!(engine.store().is_pair_blocked("zarqv", "zarqv"));
        assert!(engine.store().state().recent_events.iter().any(|ev| {
            matches!(
                ev,
                DiagnosticEvent::BackspaceUndo {
                    typed,
                    rejected,
                    resolution: UndoResolution::KeptVerbatim,
                    ..
                } if typed == "zarqv" && rejected == "Zarqv"
            )
        }));

        let mut s3 = SmartKeySession::new();
        let mut b3 = String::new();
        feed_keys(&mut s3, &mut engine, &mut b3, "meet zarqv ");
        assert_eq!(b3, "meet zarqv ");
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn sentence_start_untouched_oov_followed_by_lowercase_graduates_lowercase_and_skips_known_typos() {
    let mut store = RuleStore::in_memory(Preset::Balanced);
    let lex = smartkey_core::PackLexicon::from_bundled().unwrap();
    for _ in 0..3 {
        let ev = smartkey_core::learner::record_untouched_unknown(
            &mut store,
            Some(&lex),
            &LangId::en(),
            "beleive",
        );
        assert_eq!(ev, LearnEvent::None);
    }
    assert!(!store.compiled().personal_dict.contains("beleive"));

    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut s = SmartKeySession::new();
    let mut b = String::new();
    feed_keys(
        &mut s,
        &mut engine,
        &mut b,
        "Zyxkorp. we use zyxkorp and zyxkorp.<ENTER>",
    );
    assert_eq!(
        engine
            .store()
            .compiled()
            .personal_dict_canonical
            .get("zyxkorp")
            .map(String::as_str),
        Some("zyxkorp"),
        "sentence-start OOV followed by lowercase uses must graduate as lowercase"
    );
}

#[test]
fn english_mode_skips_untouched_raw_telex_words_but_keeps_3char_brands() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut s = SmartKeySession::new();
    let mut b = String::new();

    feed_keys(
        &mut s,
        &mut engine,
        &mut b,
        "chuwa and chuwa and chuwa.<ENTER>chawcs and chawcs and chawcs.<ENTER>",
    );
    assert!(!engine.store().is_personal_word("chuwa"));
    assert!(!engine.store().is_personal_word("chawcs"));
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("en:chuwa"));
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("en:chawcs"));

    feed_keys(&mut s, &mut engine, &mut b, "dux and dux and dux.<ENTER>");
    assert!(
        engine.store().is_personal_word("dux"),
        "3-char brand tokens must remain eligible for English personal_dict graduation"
    );
}

#[test]
fn dev_terms_textbox_shielded_and_accidental_undo_block_heals_on_1x_manual_fix() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

    // 1. `"textbox"` is now in bundled `dev_terms.txt` (`ValidWord`), so fixing it without
    //    preceding context hits Guard 2 (`ValidWordNoContext`) and never learns a global rule.
    assert!(matches!(
        engine.evaluate_token("textbox", &EvalContext::default()),
        EvalOutcome::Unchanged {
            reason: MatchKind::ValidWord,
            ..
        }
    ));
    let mut s = SmartKeySession::new();
    let mut b = String::new();
    feed_keys(
        &mut s,
        &mut engine,
        &mut b,
        "textbox <CBS>textbook <ENTER>textbox <CBS>textbook <ENTER>",
    );
    assert!(
        engine.store().lookup_correction("textbox").is_none(),
        "valid dev compound `textbox` must never graduate to [corrections] without context"
    );

    // 2. Accidental `1x BackSpace` + Space block on a known typo (`"somewher" -> "somewhere"`)
    //    heals immediately upon `1x` manual repair (`"somewher" -> "somewhere"`).
    let mut s2 = SmartKeySession::new();
    let mut b2 = String::new();
    feed_keys(&mut s2, &mut engine, &mut b2, "somewher <BS><BS> ");
    assert_eq!(b2, "somewher ");
    assert!(engine.store().is_pair_blocked("somewher", "somewhere"));

    feed_keys(&mut s2, &mut engine, &mut b2, "somewher <CBS>somewhere ");
    assert!(
        !engine.store().is_pair_blocked("somewher", "somewhere"),
        "1x manual repair of a dictionary typo must heal an accidental undo blocklist entry"
    );

    let mut b3 = String::new();
    feed_keys(&mut s2, &mut engine, &mut b3, "somewher ");
    assert_eq!(b3, "somewhere ");

    // 3. `reconcile_surrounding` ignores client echoes for 180ms while in `EditingRestored`.
    let mut s3 = SmartKeySession::new();
    for ch in "machien ".chars() {
        s3.process_key(&mut engine, KeyInput::Char(ch), 1_000);
    }
    let bs_step1 = s3.process_key(&mut engine, KeyInput::BackSpace, 1_100);
    assert_eq!(bs_step1.action, SessionAction::PassThrough);
    let bs_out = s3.process_key(&mut engine, KeyInput::BackSpace, 1_120);
    assert!(matches!(
        bs_out.action,
        SessionAction::Replace { ref insert, .. } if insert == "machien"
    ));
    s3.reconcile_surrounding(&mut engine, "machine ", 8, 1_220);
    assert!(
        !engine.store().is_pair_blocked("machien", "machine"),
        "Wayland surrounding echo at +120ms during EditingRestored must not block (machien -> machine)"
    );

    // 4. Explicit `remove_blocklist_pair` purges both the blocklist entry and `staged_undo_keep`,
    //    lifting any cross-session ghost undo shield.
    let mut s4 = SmartKeySession::new();
    let mut b4 = String::new();
    feed_keys(&mut s4, &mut engine, &mut b4, "alot <BS><BS> ");
    assert_eq!(b4, "alot ");
    assert!(engine.store().is_pair_blocked("alot", "a lot"));
    assert!(engine
        .store()
        .state()
        .staged_undo_keep
        .contains_key("en:alot->a lot"));
    assert!(engine
        .store_mut()
        .remove_blocklist_pair("alot", "a lot")
        .unwrap());
    assert!(!engine.store().is_pair_blocked("alot", "a lot"));
    assert!(!engine
        .store()
        .state()
        .staged_undo_keep
        .contains_key("en:alot->a lot"));
    assert!(!engine.store().is_undo_shielded("alot"));
    let mut b5 = String::new();
    feed_keys(&mut s4, &mut engine, &mut b5, "alot ");
    assert_eq!(b5, "a lot ");
}

#[test]
fn reconcile_surrounding_disarms_undo_on_dropped_delete_duplication() {
    let mut engine = SmartKeyEngine::in_memory_default().expect("bundled engine");
    let mut session = SmartKeySession::new();
    let mut now_ms = 1_000u64;

    let mut last_action = SessionAction::PassThrough;
    for ch in "dont ".chars() {
        now_ms += 20;
        last_action = session.process_key(&mut engine, KeyInput::Char(ch), now_ms).action;
    }
    assert_eq!(
        last_action,
        SessionAction::Replace {
            delete_chars: 4,
            insert: "don't ".to_string(),
        }
    );

    // Legitimate repeated word ("dont don't ") must keep JustAutocorrected armed.
    now_ms += 100;
    session.reconcile_surrounding(&mut engine, "dont don't ", 11, now_ms);
    assert_eq!(session.debug_snapshot().mode, "JustAutocorrected");

    // Dropped-delete duplication ("dontdon't ") must disarm JustAutocorrected to Idle
    // so subsequent cleanup BackSpaces do not trigger undo/blocklist poisoning.
    now_ms += 100;
    session.reconcile_surrounding(&mut engine, "dontdon't ", 10, now_ms);
    assert_eq!(session.debug_snapshot().mode, "Idle");

    for _ in 0..6 {
        now_ms += 20;
        let out = session.process_key(&mut engine, KeyInput::BackSpace, now_ms);
        assert_eq!(out.action, SessionAction::PassThrough);
    }
    assert!(
        !engine.store().is_pair_blocked("dont", "don't"),
        "cleanup BackSpaces after dropped-delete disarm must never blocklist (dont, don't)"
    );

    let mut recheck = String::new();
    for ch in "dont ".chars() {
        now_ms += 20;
        let out = session.process_key(&mut engine, KeyInput::Char(ch), now_ms);
        apply_key_to_buffer(&mut recheck, KeyInput::Char(ch), &out.action);
    }
    assert_eq!(recheck, "don't ");
}

#[test]
fn undo_mode_step_supports_post_autocorrect_punctuation_and_suffix_edits() {
    let mut store = RuleStore::in_memory(Preset::Balanced);
    store
        .set_setting("auto_capitalize_sentence", "true")
        .unwrap();
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. Backspace once to delete trailing space, add forgotten `. `, and verify next word
    //    capitalizes (sentence start) with ZERO BackspaceUndo events or blocklist entries:
    feed_keys(&mut session, &mut engine, &mut buf, "the reosurce <BS>. next ");
    assert_eq!(buf, "The resource. Next ");
    assert!(engine.store().state().undo_counts.is_empty());
    assert!(!engine.store().is_pair_blocked("reosurce", "resource"));

    // 2. Comma, exclamation (triggers sentence cap), and semicolon:
    buf.clear();
    let _ = session.reset(&mut engine);
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "the reosurce <BS>, reosurce <BS>! next reosurce <BS>; ",
    );
    assert_eq!(buf, "The resource, resource! Next resource; ");
    assert!(engine.store().state().undo_counts.is_empty());

    // 3. Suffix extension (`reosurce <BS>s `) and dot-filename merge (`reosurce <BS>.json `):
    buf.clear();
    let _ = session.reset(&mut engine);
    feed_keys(
        &mut session,
        &mut engine,
        &mut buf,
        "the reosurce <BS>s reosurce <BS>.json ",
    );
    assert_eq!(buf, "The resources resource.json ");
    assert!(engine.store().state().undo_counts.is_empty());

    // 4. Deleting the newly added dot + extra backspace snaps back cleanly to raw input:
    buf.clear();
    let _ = session.reset(&mut engine);
    feed_keys(&mut session, &mut engine, &mut buf, "ssytem <BS>.<BS><BS>");
    assert_eq!(buf, "ssytem");

    // 5. Same after `. `: 3 backspaces peel `' '`, then `'.'`, then snap back to `"reosurce"`:
    buf.clear();
    let _ = session.reset(&mut engine);
    feed_keys(&mut session, &mut engine, &mut buf, "reosurce <BS>. <BS><BS><BS> ");
    assert_eq!(buf, "reosurce ");
    assert!(engine.store().is_pair_blocked("reosurce", "resource"));
}

#[test]
fn undo_mode_immediate_and_off_behave_per_setting() {
    // 1. Immediate mode: legacy 1x BackSpace snaps `"resource "` -> `"reosurce"` and `"changes, "` -> `"chagnes"`.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        store.set_setting("settings.undo_mode", "immediate").unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        feed_keys(&mut session, &mut engine, &mut buf, "reosurce <BS>");
        assert_eq!(buf, "reosurce");

        buf.clear();
        let _ = session.reset(&mut engine);
        feed_keys(&mut session, &mut engine, &mut buf, "chagnes, <BS>");
        assert_eq!(buf, "chagnes");
    }

    // 2. Off mode: BackSpace never snaps back to `"reosurce"`, and Ctrl+BackSpace does not log undo.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        store.set_setting("settings.undo_mode", "off").unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        feed_keys(&mut session, &mut engine, &mut buf, "reosurce <BS><BS>");
        assert_eq!(buf, "resourc");
        feed_keys(&mut session, &mut engine, &mut buf, " ");
        assert!(!engine.store().is_pair_blocked("reosurce", "resource"));
        assert!(engine.store().state().undo_counts.is_empty());

        buf.clear();
        let _ = session.reset(&mut engine);
        feed_keys(&mut session, &mut engine, &mut buf, "chagnes <CBS>");
        assert_eq!(buf, "");
        assert!(engine.store().state().undo_counts.is_empty());
    }
}

#[test]
fn prefix_truncation_is_rejected_and_pending_split_learns_fused_pairs() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // 1. `wantto<BS><BS> to `: 1x stages `en:wantto->want to`, NEVER `en:wantto->want`.
    let evs1 = feed_keys(&mut session, &mut engine, &mut buf, "wantto<BS><BS> to ");
    assert_eq!(buf, "want to ");
    assert!(
        !engine
            .store()
            .state()
            .staged_corrections
            .contains_key("en:wantto->want"),
        "must never stage prefix truncation en:wantto->want"
    );
    assert!(
        evs1.iter().any(|e| matches!(
            e,
            LearnEvent::StagedCorrection { orig, fixed, count: 1, .. }
                if orig == "wantto" && fixed == "want to"
        )),
        "expected StagedCorrection wantto->want to, got {evs1:?}"
    );
    assert_eq!(engine.store().state().space_habits.missed_space, 1);

    // 2. 2x graduates `wantto = "want to"`, and 3rd `wantto ` auto-fixes cleanly.
    buf.clear();
    let evs2 = feed_keys(&mut session, &mut engine, &mut buf, "wantto<BS><BS> to ");
    assert!(
        evs2.iter().any(|e| matches!(
            e,
            LearnEvent::GraduatedCorrection { orig, fixed, .. }
                if orig == "wantto" && fixed == "want to"
        )),
        "expected GraduatedCorrection wantto->want to, got {evs2:?}"
    );
    assert_eq!(
        engine.store().compiled().corrections.get("wantto").map(String::as_str),
        Some("want to")
    );
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "wantto ");
    assert_eq!(buf, "want to ");

    // 3. Partial word-2 prefix (`thati<BS> is `, `partyri<BS><BS> rims `) cancels truncation,
    //    increments `missed_space`, and never stages prefix truncation.
    let mut s2 = SmartKeySession::new();
    let mut b2 = String::new();
    feed_keys(&mut s2, &mut engine, &mut b2, "thati<BS> is ");
    assert_eq!(b2, "that is ");
    feed_keys(&mut s2, &mut engine, &mut b2, "partyri<BS><BS> rims ");
    assert_eq!(b2, "that is party rims ");
    assert!(
        !engine
            .store()
            .state()
            .staged_corrections
            .contains_key("en:thati->that")
            && !engine
                .store()
                .state()
                .staged_corrections
                .contains_key("en:partyri->party")
    );
    assert_eq!(engine.store().state().space_habits.missed_space, 4);

    // 4. Incomplete-w2 split (`movieevr<BS><BS><BS> ever `) matches via `dl <= 1` and graduates at 2x.
    let mut s3 = SmartKeySession::new();
    let mut b3 = String::new();
    feed_keys(&mut s3, &mut engine, &mut b3, "movieevr<BS><BS><BS> ever ");
    assert_eq!(b3, "movie ever ");
    let evs4 = feed_keys(&mut s3, &mut engine, &mut b3, "movieevr<BS><BS><BS> ever ");
    assert!(
        evs4.iter().any(|e| matches!(
            e,
            LearnEvent::GraduatedCorrection { orig, fixed, .. }
                if orig == "movieevr" && fixed == "movie ever"
        )),
        "expected movieevr->movie ever graduation, got {evs4:?}"
    );

    // 5. Direct manual truncation `wantto -> want` followed by unrelated word never graduates or stages.
    let mut s4 = SmartKeySession::new();
    let mut b4 = String::new();
    feed_keys(&mut s4, &mut engine, &mut b4, "haveto<BS><BS> sleep ");
    assert!(
        !engine
            .store()
            .state()
            .staged_corrections
            .contains_key("en:haveto->have")
    );
}

#[test]
fn editing_restored_split_blocks_wrong_autocorrect_and_graduates_split() {
    let mut store = RuleStore::in_memory(Preset::Balanced);
    // Force `herson` to hit single-word fuzzy `herons` first to exercise `EditingRestored` split.
    store.set_setting("enable_missed_space_split", "false").unwrap();
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    let mut buf = String::new();

    // Verify `herson ` autocorrected to `herons `, then user undoes (`Step`: <BS> space, <BS> word -> `herson`),
    // deletes `son` -> `her`, types ` son `.
    feed_keys(&mut session, &mut engine, &mut buf, "herson ");
    assert_eq!(buf, "herons ");
    let evs1 = feed_keys(&mut session, &mut engine, &mut buf, "<BS><BS><BS><BS><BS> son ");
    assert_eq!(buf, "her son ");
    assert!(
        engine.store().is_pair_blocked("herson", "herons"),
        "expected wrong single-word autocorrect (herson, herons) to be blocked on restored split"
    );
    assert!(
        evs1.iter().any(|e| matches!(
            e,
            LearnEvent::StagedCorrection { orig, fixed, count: 1, .. }
                if orig == "herson" && fixed == "her son"
        )),
        "expected StagedCorrection herson->her son, got {evs1:?}"
    );

    // Next `herson ` no longer becomes `herons `; at 2x `herson -> her son` graduates!
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "herson ");
    assert_eq!(buf, "herson ");
    let evs2 = feed_keys(&mut session, &mut engine, &mut buf, "<BS><BS><BS><BS> son ");
    assert!(
        evs2.iter().any(|e| matches!(
            e,
            LearnEvent::GraduatedCorrection { orig, fixed, .. }
                if orig == "herson" && fixed == "her son"
        )),
        "expected GraduatedCorrection herson->her son, got {evs2:?}"
    );
    buf.clear();
    feed_keys(&mut session, &mut engine, &mut buf, "herson ");
    assert_eq!(buf, "her son ");
}

#[test]
fn layer1_broader_fused_split_and_cross_space_shift_work() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

    for (input, expected) in [
        ("herson ", "her son "),
        ("canadd ", "can add "),
        ("thelearning ", "the learning "),
        ("addi n ", "add in "),
        ("theo ther ", "the other "),
        ("fo rme ", "for me "),
        ("th e ", "the "),
        ("ha ve ", "have "),
        ("Theo ther ", "The other "),
        ("cany ou ", "can you "),
    ] {
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, input);
        assert_eq!(buf, expected, "input={input:?}");
    }
}

#[test]
fn layer1_space_shift_zero_false_positives_and_boundary_guards() {
    let store = RuleStore::in_memory(Preset::Balanced);
    let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

    for (input, expected) in [
        ("an other ", "an other "),
        ("in to ", "in to "),
        ("a lot ", "a lot "),
        ("the end ", "the end "),
        ("git hub ", "git hub "),
        ("hypr land ", "hypr land "),
        ("I n ", "I n "),
        ("addi <NAV>n ", "addi n "),
        ("addi, n ", "addi, n "),
        ("addi  n ", "addi  n "),
    ] {
        let mut session = SmartKeySession::new();
        let mut buf = String::new();
        feed_keys(&mut session, &mut engine, &mut buf, input);
        assert_eq!(buf, expected, "must remain untouched for {input:?}");
    }

    // `vi` (`fcitx5-lotus`) mode stays excluded from cross-space shifts.
    let mut vi_session = SmartKeySession::new();
    vi_session.set_lang(LangId::vi());
    let mut vi_buf = String::new();
    feed_keys(&mut vi_session, &mut engine, &mut vi_buf, "addi n th e ");
    assert_eq!(vi_buf, "addi n th e ");
}

#[test]
fn space_shift_undo_blocks_pair_and_adjusts_habit_brake() {
    // 1. Step undo on `addi n -> add in` restores `addi n`, blocks pair, and increments `space_fix_undos`.
    {
        let store = RuleStore::in_memory(Preset::Balanced);
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        feed_keys(&mut session, &mut engine, &mut buf, "addi n ");
        assert_eq!(buf, "add in ");
        feed_keys(&mut session, &mut engine, &mut buf, "<BS><BS> ");
        assert_eq!(buf, "addi n ");
        assert!(engine.store().is_pair_blocked("addi n", "add in"));
        assert_eq!(engine.store().state().space_habits.space_fix_undos, 1);

        buf.clear();
        feed_keys(&mut session, &mut engine, &mut buf, "addi n ");
        assert_eq!(buf, "addi n ");
    }

    // 2. Immediate undo on `addi n -> add in` works identically.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        store.set_setting("settings.undo_mode", "immediate").unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        feed_keys(&mut session, &mut engine, &mut buf, "addi n ");
        assert_eq!(buf, "add in ");
        feed_keys(&mut session, &mut engine, &mut buf, "<BS> ");
        assert_eq!(buf, "addi n ");
        assert!(engine.store().is_pair_blocked("addi n", "add in"));
        assert_eq!(engine.store().state().space_habits.space_fix_undos, 1);
    }

    // 3. Self-confirming undo (`th e` -> `the` -> BSx6 -> `the `) must NOT increment `space_fix_undos`.
    {
        let store = RuleStore::in_memory(Preset::Balanced);
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();
        let mut session = SmartKeySession::new();
        let mut buf = String::new();

        let evs = feed_keys(&mut session, &mut engine, &mut buf, "th e <BS><BS><BS><BS><BS><BS>the ");
        assert_eq!(buf, "the ");
        assert!(evs.contains(&LearnEvent::SelfConfirmCancelled));
        assert_eq!(engine.store().state().space_habits.space_fix_undos, 0);
    }

    // 4. Manual cross-space shift & extra-space repairs record `late_space`, `early_space`, and `extra_space`.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        store.set_setting("enable_missed_space_split", "false").unwrap();
        let mut engine = SmartKeyEngine::new_bundled(store).unwrap();

        let mut s1 = SmartKeySession::new();
        let mut b1 = String::new();
        let ev1 = feed_keys(&mut s1, &mut engine, &mut b1, "addi n <BS><BS><BS><BS> in ");
        assert_eq!(b1, "add in ");
        assert!(ev1.iter().any(|e| matches!(
            e,
            LearnEvent::RecordedSpaceHabit {
                habit: SpaceHabitKind::LateSpace,
                count: 1,
            }
        )));

        let mut s2 = SmartKeySession::new();
        let mut b2 = String::new();
        let ev2 = feed_keys(&mut s2, &mut engine, &mut b2, "fo rme <BS><BS><BS><BS><BS>r me ");
        assert_eq!(b2, "for me ");
        assert!(ev2.iter().any(|e| matches!(
            e,
            LearnEvent::RecordedSpaceHabit {
                habit: SpaceHabitKind::EarlySpace,
                count: 1,
            }
        )));

        let mut s3 = SmartKeySession::new();
        let mut b3 = String::new();
        let ev3 = feed_keys(&mut s3, &mut engine, &mut b3, "th e <BS><BS><BS>e ");
        assert_eq!(b3, "the ");
        assert!(ev3.iter().any(|e| matches!(
            e,
            LearnEvent::RecordedSpaceHabit {
                habit: SpaceHabitKind::ExtraSpace,
                count: 1,
            }
        )));

        assert!(engine.store().space_habit_boost(SpaceHabitKind::LateSpace) >= 0.0);
        engine.store_mut().record_space_habit(SpaceHabitKind::LateSpace);
        assert!(engine.store().space_habit_boost(SpaceHabitKind::LateSpace) > 0.0);
        engine.store_mut().record_space_fix_undo();
        assert!(engine.store().space_habit_boost(SpaceHabitKind::LateSpace) < 0.0);
    }
}

#[test]
fn vi_syllable_validation_untouched_guards_trailing_quote_and_ai_shift_slip() {
    use smartkey_core::learner::{record_manual_edit, record_untouched_unknown};
    use smartkey_core::PackLexicon;

    // 1. VI manual fixes (<4 chars) validate against syllable_dict instead of failing TargetNotValidWord,
    //    and untouched VI syllables/slang never graduate into personal_dict.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        let vi_lex = PackLexicon::from_bundled_vi().expect("bundled vi lexicon");
        let vi = LangId::vi();

        let ev_di = record_manual_edit(&mut store, Some(&vi_lex), &vi, None, "đ", "đi");
        assert!(
            matches!(ev_di, LearnEvent::Ignored(SkipReason::ValidWordNoContext)),
            "valid VI syllable `đ -> đi` without context must hit ValidWordNoContext, not TargetNotValidWord: {ev_di:?}"
        );

        let ev_truu = record_manual_edit(&mut store, Some(&vi_lex), &vi, None, "trừu", "trừ");
        assert!(
            matches!(ev_truu, LearnEvent::Ignored(SkipReason::ValidWordNoContext)),
            "valid VI syllable `trừu -> trừ` without context must hit ValidWordNoContext: {ev_truu:?}"
        );

        let ev_tru = record_manual_edit(&mut store, Some(&vi_lex), &vi, None, "trùu", "trừ");
        assert!(
            matches!(ev_tru, LearnEvent::StagedCorrection { count: 1, .. }),
            "typo `trùu` -> valid VI syllable `trừ` must stage at 1x: {ev_tru:?}"
        );

        assert_eq!(
            record_untouched_unknown(&mut store, Some(&vi_lex), &vi, "đi"),
            LearnEvent::None
        );
        assert_eq!(
            record_untouched_unknown(&mut store, Some(&vi_lex), &vi, "ae"),
            LearnEvent::None
        );
    }

    // 2. EN untouched_unknowns blocks built-in contractions (`didnt`) and unconfirmed 2-char noise
    //    (`od`, `sp`, `aj`) while keeping 3-char OOV brands (`dux`) eligible.
    {
        let mut store = RuleStore::in_memory(Preset::Balanced);
        let en_lex = PackLexicon::from_bundled().expect("bundled en lexicon");
        let en = LangId::en();

        for blocked_tok in ["didnt", "od", "sp", "aj"] {
            for _ in 0..3 {
                assert_eq!(
                    record_untouched_unknown(&mut store, Some(&en_lex), &en, blocked_tok),
                    LearnEvent::None,
                    "{blocked_tok:?} must not stage or graduate via untouched_3x"
                );
            }
            assert!(!store.is_personal_word(blocked_tok));
        }

        for _ in 0..3 {
            record_untouched_unknown(&mut store, Some(&en_lex), &en, "dux");
        }
        assert!(
            store.is_personal_word("dux"),
            "3-char brand `dux` must remain eligible for untouched_3x graduation"
        );
    }

    // 3 & 4. Lone trailing apostrophe on contractions (`thats'` -> `that's`, preserving `'thats'` -> `'that's'`),
    //        and 2-char shift-slip exclusions (`aI` stays unchanged, `iS` -> `Is`).
    {
        let store = RuleStore::in_memory(Preset::Balanced);
        let engine = SmartKeyEngine::new_bundled(store).unwrap();
        let ctx = EvalContext::default();

        assert!(matches!(
            engine.evaluate_token("thats'", &ctx),
            EvalOutcome::Autocorrect {
                ref replacement,
                kind: MatchKind::Contraction,
                ..
            } if replacement == "that's"
        ));
        assert!(matches!(
            engine.evaluate_token("'thats'", &ctx),
            EvalOutcome::Autocorrect {
                ref replacement,
                kind: MatchKind::Contraction,
                ..
            } if replacement == "'that's'"
        ));
        assert!(matches!(
            engine.evaluate_token("aI", &ctx),
            EvalOutcome::Unchanged { .. }
        ));
        assert!(matches!(
            engine.evaluate_token("iS", &ctx),
            EvalOutcome::Autocorrect {
                ref replacement,
                kind: MatchKind::ShiftSlip,
                ..
            } if replacement == "Is"
        ));
    }
}
