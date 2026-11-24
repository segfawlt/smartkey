use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use smartkey_core::{
    apply_key_to_buffer, EvalContext, EvalOutcome, KeyInput, LangId, MatchKind, Preset, RuleStore,
    SessionAction, SmartKeyEngine, SmartKeySession, UserRuleView,
};

fn make_vi_engine_and_session() -> (SmartKeyEngine, SmartKeySession) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "smartkey_vi_lotus_test_{}_{}",
        std::process::id(),
        id
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let rules_path = dir.join("rules.toml");
    let state_path = dir.join("learner_state.json");
    let store = RuleStore::load_or_init(rules_path, state_path, Preset::Balanced).unwrap();
    let engine = SmartKeyEngine::new_bundled(store).unwrap();
    let mut session = SmartKeySession::new();
    session.set_lang(LangId::vi());
    (engine, session)
}

fn type_sequence(
    session: &mut SmartKeySession,
    engine: &mut SmartKeyEngine,
    seq: &str,
) -> (String, Vec<SessionAction>) {
    let mut lotus_screen_buf = String::new();
    let mut actions = Vec::new();
    let mut now_ms = 1_000u64;

    // Simulate how real `fcitx5-lotus` updates the screen during word typing:
    // - When `sim.needs_uinput_restore_on_break` is true (`"nuowcss "` -> `"nươc"` on screen,
    //   `mustFallbackToEnglish()` triggers on break), real Lotus filters the boundary key (`PostInputMethod`
    //   never runs for the key event!) and instead commits the restored text via `CommitStringEvent`.
    // - Otherwise Lotus forwards the boundary key to `PostInputMethod`.
    let mut cur_word_keys = String::new();
    let opts = engine.store().lotus_options();
    for ch in seq.chars() {
        now_ms += 40;
        let is_boundary = matches!(ch, ' ' | ',' | '.' | '!' | '?' | ';' | ':');
        if !is_boundary {
            cur_word_keys.push(ch);
            let out = session.process_key(engine, KeyInput::Char(ch), now_ms);
            assert_eq!(
                out.action,
                SessionAction::PassThrough,
                "non-boundary char {ch:?} must never be filtered in PreInputMethod"
            );
            actions.push(out.action);
        } else if !cur_word_keys.is_empty() {
            let sim = smartkey_core::simulate_lotus_word_with_options(&cur_word_keys, opts);
            cur_word_keys.clear();
            if sim.needs_uinput_restore_on_break {
                lotus_screen_buf.push_str(&sim.committed_at_break);
                lotus_screen_buf.push(ch);
                // Real Lotus `compareAndSplitU32(oldPreBuffer_, commitStr)` commits only the suffix
                // after the shared prefix (`"nươc"` vs `"nuowcss "` -> commits `"uowcss "`):
                let common_prefix = sim
                    .preedit_before_break
                    .chars()
                    .zip(sim.committed_at_break.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a.len_utf8())
                    .sum::<usize>();
                let mut diff_added = sim.committed_at_break[common_prefix..].to_string();
                diff_added.push(ch);
                let out = session.on_lotus_boundary_commit(engine, ch, &diff_added, now_ms);
                if let SessionAction::Replace {
                    delete_chars,
                    insert,
                } = &out.action
                {
                    assert!(
                        *delete_chars <= lotus_screen_buf.chars().count(),
                        "delete_chars={delete_chars} > screen len in {lotus_screen_buf:?}"
                    );
                    for _ in 0..*delete_chars {
                        lotus_screen_buf.pop();
                    }
                    lotus_screen_buf.push_str(insert);
                }
                actions.push(out.action);
            } else {
                if sim.is_composed_vietnamese {
                    lotus_screen_buf.push_str(&sim.committed_at_break);
                } else {
                    lotus_screen_buf.push_str(&sim.preedit_before_break);
                }
                let key = KeyInput::Char(ch);
                let out = session.process_key(engine, key, now_ms);
                apply_key_to_buffer(&mut lotus_screen_buf, key, &out.action);
                actions.push(out.action);
            }
        } else {
            let key = KeyInput::Char(ch);
            let out = session.process_key(engine, key, now_ms);
            apply_key_to_buffer(&mut lotus_screen_buf, key, &out.action);
            actions.push(out.action);
        }
    }

    (lotus_screen_buf, actions)
}

#[test]
fn test_vi_lotus_broken_telex_rescue_and_exact_screen_chars() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. `ttieengs ` -> 8 screen chars -> `"tiếng "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(buf, "tiếng ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 8,
            insert: "tiếng ".to_string(),
        }
    );

    // 2. `tieeengs ` -> 7 screen chars (3rd `e` undid `ê -> ee`) -> `"tiếng "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "tieeengs ");
    assert_eq!(buf, "tiếng ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 7,
            insert: "tiếng ".to_string(),
        }
    );

    // 3. `tieengss ` -> 8 screen chars -> `"tiếng "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "tieengss ");
    assert_eq!(buf, "tiếng ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 8,
            insert: "tiếng ".to_string(),
        }
    );

    // 4. `hnuwngx ` -> 7 screen chars -> `"những "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "hnuwngx ");
    assert_eq!(buf, "những ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 7,
            insert: "những ".to_string(),
        }
    );

    // 5. `dddi ` -> 3 screen chars (3rd `d` undid `đ -> dd`, leaving `"ddi"`) -> `"đi "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "dddi ");
    assert_eq!(buf, "đi ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 3,
            insert: "đi ".to_string(),
        }
    );

    // 6. `khooong ` -> 6 screen chars (3rd `o` undid `ô -> oo`, leaving `"khoong"`) -> `"không "`
    let (buf, actions) = type_sequence(&mut session, &mut engine, "khooong ");
    assert_eq!(buf, "không ");
    assert_eq!(
        actions.last().unwrap(),
        &SessionAction::Replace {
            delete_chars: 6,
            insert: "không ".to_string(),
        }
    );

    // 7. `hoagnf ` (adjacent coda transposition) and `hoagfn ` (split coda transposition across `f`) -> `"hoàng "`
    for raw in ["hoagnf ", "hoagfn "] {
        let (buf, actions) = type_sequence(&mut session, &mut engine, raw);
        assert_eq!(buf, "hoàng ");
        assert_eq!(
            actions.last().unwrap(),
            &SessionAction::Replace {
                delete_chars: 6,
                insert: "hoàng ".to_string(),
            }
        );
    }
}

#[test]
fn test_vi_lotus_shields_valid_vietnamese_and_english_words() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // Composed Vietnamese words and toneless Vietnamese syllables (`nha`, `em`, `anh`, `con`, `cho`)
    // must never be corrupted into English words (`nah`, `me`, etc.):
    let (buf, _) = type_sequence(
        &mut session,
        &mut engine,
        "tieengs vieetj nha em anh con cho mai lan ",
    );
    assert_eq!(buf, "tiếng việt nha em anh con cho mai lan ");

    // Valid English words (including Floss-rule `-ss`/`-ff`, `-ee`/`-oo` words, and 3-key Telex escapes
    // `treee -> tree`, `seee -> see`, `tooo -> too`) must stay English:
    let (buf, _) = type_sequence(
        &mut session,
        &mut engine,
        "treee feel issue book class need inn seee tooo off boss less process ",
    );
    assert_eq!(
        buf,
        "tree feel issue book class need inn see too off boss less process "
    );

    // English typos in Lotus mode still autocorrect via Layer 1 English scorer:
    let (buf, _) = type_sequence(&mut session, &mut engine, "dont teh chagnes ");
    assert_eq!(buf, "don't the changes ");
}

#[test]
fn test_vi_lotus_backspace_undo_and_modular_plugin_toggle() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Stepwise BackSpace after `"ttieengs "` lets the user add forgotten `. ` -> `"tiếng. "` without reverting or shielding:
    let (mut dot_buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(dot_buf, "tiếng ");
    let bs_peel = session.process_key(&mut engine, KeyInput::BackSpace, 1_500);
    assert_eq!(bs_peel.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut dot_buf, KeyInput::BackSpace, &bs_peel.action);
    for (idx, ch) in ['.', ' '].into_iter().enumerate() {
        let out = session.process_key(&mut engine, KeyInput::Char(ch), 1_520 + (idx as u64) * 20);
        assert_eq!(out.action, SessionAction::PassThrough);
        apply_key_to_buffer(&mut dot_buf, KeyInput::Char(ch), &out.action);
    }
    assert_eq!(dot_buf, "tiếng. ");
    let _ = session.reset(&mut engine);

    // 2. Type `ttieengs ` -> `"tiếng "`, then `2x BackSpace` -> 1st peels `' '`, 2nd reverts to `"ttieengs"`!
    let (mut buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(buf, "tiếng ");

    let bs1 = session.process_key(&mut engine, KeyInput::BackSpace, 1_950);
    assert_eq!(bs1.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut buf, KeyInput::BackSpace, &bs1.action);
    assert_eq!(buf, "tiếng");

    let bs_out = session.process_key(&mut engine, KeyInput::BackSpace, 2_000);
    apply_key_to_buffer(&mut buf, KeyInput::BackSpace, &bs_out.action);
    assert_eq!(buf, "ttieengs");

    // Disable `vi-lotus` plugin -> `ttieengs` is no longer rescued as Vietnamese Telex:
    assert!(engine.store().is_plugin_enabled("vi-lotus").unwrap());
    engine
        .store_mut()
        .set_plugin_enabled("vi-lotus", false)
        .unwrap();
    assert!(!engine.store().is_plugin_enabled("vi-lotus").unwrap());

    let outcome = engine.evaluate_token("ttieengs", &EvalContext::default());
    assert!(!matches!(
        outcome,
        EvalOutcome::Autocorrect {
            kind: MatchKind::VietnameseTelexRescue,
            ..
        }
    ));

    // Re-enable `vi-lotus` plugin -> `ttieengs` rescues to `"tiếng"` again:
    engine
        .store_mut()
        .set_plugin_enabled("vi-lotus", true)
        .unwrap();
    let outcome = engine.evaluate_token("ttieengs", &EvalContext::default());
    assert_eq!(
        outcome,
        EvalOutcome::Autocorrect {
            replacement: "tiếng".to_string(),
            kind: MatchKind::VietnameseTelexRescue,
            score: 9.2,
            margin: 9.2,
        }
    );
}

#[test]
fn test_vi_lotus_shorthand_expansion_and_slang_shielding() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Vietnamese shorthand auto-expansions (`ko -> không`, `dc -> được`, `ntn -> như thế nào`, `mn -> mọi người`):
    let (buf, _) = type_sequence(&mut session, &mut engine, "ko dc ntn mn ");
    assert_eq!(buf, "không được như thế nào mọi người ");

    // 2. Stepwise BackSpace immediately after multi-word Vietnamese shorthand expansion (`ntn -> như thế nào`):
    //    1st BackSpace peels trailing `' '`; 2nd deletes the expanded phrase `"như thế nào"` and restores `"ntn"`, and Space blocks the pair!
    let (mut buf2, _) = type_sequence(&mut session, &mut engine, "ntn ");
    assert_eq!(buf2, "như thế nào ");
    let bs2_step = session.process_key(&mut engine, KeyInput::BackSpace, 4_950);
    assert_eq!(bs2_step.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut buf2, KeyInput::BackSpace, &bs2_step.action);
    assert_eq!(buf2, "như thế nào");
    let bs_out = session.process_key(&mut engine, KeyInput::BackSpace, 5_000);
    apply_key_to_buffer(&mut buf2, KeyInput::BackSpace, &bs_out.action);
    assert_eq!(buf2, "ntn");
    let sp_out = session.process_key(&mut engine, KeyInput::Char(' '), 5_050);
    apply_key_to_buffer(&mut buf2, KeyInput::Char(' '), &sp_out.action);
    assert_eq!(buf2, "ntn ");
    assert!(engine.store().is_pair_blocked("ntn", "như thế nào"));

    // Next time `ntn ` is typed, it stays `"ntn "`!
    let (buf3, _) = type_sequence(&mut session, &mut engine, "ntn ");
    assert_eq!(buf3, "ntn ");

    // 3. Vietnamese slang & clipped words (`ib`, `rep`, `onl`, `uk`, `uhm`, `bt`, `nc`, `khum`)
    //    are shielded 100% and never corrupted into English or other Vietnamese words:
    let (buf4, _) = type_sequence(
        &mut session,
        &mut engine,
        "ib rep onl uk uhm bt nc khum okela ",
    );
    assert_eq!(buf4, "ib rep onl uk uhm bt nc khum okela ");
}

#[test]
fn test_vi_lotus_host_contract_filtered_boundaries_macros_off_mode_and_options() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Filtered break-restore rescue (`nuowcss ` -> runic `"nươc"` -> Lotus filters Space and
    //    commits `"uowcss "` via CommitStringEvent -> SmartKey deletes 8 chars and commits `"nước "`)
    //    plus `ddFreeStyle` `đ`-word rescue (`ddwungs ` -> `"đwungs"` stays unfiltered -> `"đứng "`):
    let (mut buf, _) = type_sequence(&mut session, &mut engine, "nuowcss ruowujj ddwungs ");
    assert_eq!(buf, "nước rượu đứng ");

    // Trailing dot lookahead across filtered Lotus restore (`nuowcss. ` -> `"nước. "` while `nuowcss.com ` stays untouched):
    let (buf_dot, _) = type_sequence(&mut session, &mut engine, "nuowcss. nuowcss.com ");
    assert_eq!(buf_dot, "nước. nuowcss.com ");

    // 2. Lotus macro expansion (`ko` -> user macro `"Kính gửi"`): `CommitStringEvent` text is not
    //    a suffix of `"ko "`, so SmartKey leaves the macro output untouched instead of corrupting it!
    for ch in "ko".chars() {
        session.process_key(&mut engine, KeyInput::Char(ch), 3_000);
    }
    let macro_out = session.on_lotus_boundary_commit(&mut engine, ' ', "Kính gửi ", 3_020);
    assert_eq!(macro_out.action, SessionAction::PassThrough);

    // 3. Lotus `Off`/`Default` mode (`lotus_composing = false`): even when `"of"` is explicitly in
    //    `telex_passthrough` and `"issue"` has doubled Telex keys, SmartKey knows Lotus did NOT
    //    compose `"ò"` or `"isue"` on screen, preventing `"oof "` / `"iissue "` corruption!
    engine.store_mut().add_telex_passthrough("of");
    session.set_lotus_composing(false);
    let mut off_buf = String::new();
    for ch in "of issue teh ".chars() {
        let key = KeyInput::Char(ch);
        let out = session.process_key(&mut engine, key, 4_000);
        apply_key_to_buffer(&mut off_buf, key, &out.action);
    }
    assert_eq!(off_buf, "of issue the ");
    session.set_lotus_composing(true);

    // 4. English-Sentence Momentum Shield + stepwise BackSpace revert back to Vietnamese rune:
    let (mut mom_buf, _) = type_sequence(&mut session, &mut engine, "it is ");
    assert_eq!(mom_buf, "it is ");
    let bs_m1 = session.process_key(&mut engine, KeyInput::BackSpace, 4_025);
    assert_eq!(bs_m1.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut mom_buf, KeyInput::BackSpace, &bs_m1.action);
    assert_eq!(mom_buf, "it is");
    let bs = session.process_key(&mut engine, KeyInput::BackSpace, 4_050);
    apply_key_to_buffer(&mut mom_buf, KeyInput::BackSpace, &bs.action);
    assert_eq!(mom_buf, "it í");
    let _ = session.reset(&mut engine);

    // 5. `W2U=Everywhere` parsed from `lotus.conf`: `"wa "` composes valid Vietnamese `"ưa "`!
    let opts = smartkey_core::LotusOptions::from_fcitx5_conf_str(
        "Mode=\"Uinput (Super Smooth)\"\nW2U=Everywhere\nModernStyle=True\n",
    );
    assert_eq!(opts.w2u, smartkey_core::W2uMode::Everywhere);
    engine.store_mut().set_lotus_options(opts);
    let (w2u_buf, _) = type_sequence(&mut session, &mut engine, "wa ");
    assert_eq!(w2u_buf, "ưa ");

    // 6. Global English `personal_dict` does NOT block Vietnamese Telex rescue, whereas `[lang.vi]` does:
    engine
        .store_mut()
        .add_personal_word(&LangId::en(), "ttieengs");
    let (en_pd_buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(en_pd_buf, "tiếng ");
    engine
        .store_mut()
        .add_personal_word(&LangId::vi(), "ttieengs");
    let (vi_pd_buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(vi_pd_buf, "ttieengs ");

    // 7. `SurroundingText` mouse-click cursor reconciliation resets stale `en` composing buffer:
    session.set_lang(LangId::en());
    let _ = session.reset(&mut engine);
    for ch in "chag".chars() {
        session.process_key(&mut engine, KeyInput::Char(ch), 5_000);
    }
    // Within 80ms (key-echo lag): ignored
    session.reconcile_surrounding(&mut engine, "hello world", 5, 5_040);
    // After 80ms at an unrelated cursor location ("hello| world"): resets stale "chag" buffer!
    session.reconcile_surrounding(&mut engine, "hello world", 5, 5_200);
    let sp = session.process_key(&mut engine, KeyInput::Char(' '), 5_220);
    assert_eq!(sp.action, SessionAction::PassThrough);
    let _ = &mut buf;
}

#[test]
fn test_vi_lotus_uinput_restore_class_rescue_and_unknown_learning() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Single tone/mark key substitution across the filtered-boundary commit (`duongx ` ->
    //    runic `duõng` on screen -> uinput restore burst commits `ongx ` -> SmartKey deletes the
    //    6-char raw word + boundary and commits `dương `):
    let (buf, _) = type_sequence(&mut session, &mut engine, "duongx ");
    assert_eq!(buf, "dương ");

    // 2. Restore-class word with NO rescue candidate (`duonxg `): screen stays raw ASCII, but the
    //    word must no longer be silently dropped — it feeds `log_unfixed_unknown` and stages the
    //    untouched unknown (`3x -> [lang.vi].personal_dict`), flush confirmed by `reset`.
    let (buf2, _) = type_sequence(&mut session, &mut engine, "duonxg ");
    assert_eq!(buf2, "duonxg ");
    let _ = session.reset(&mut engine);
    assert!(
        engine
            .store()
            .state()
            .untouched_unknowns
            .contains_key("vi:duonxg"),
        "restore-class unknown must be staged: {:?}",
        engine.store().state().untouched_unknowns
    );

    // 3. Live-English-fallback words in the same screen/token mismatch class (`feeel ` renders
    //    `feel` on screen) keep `do_not_learn` — valid English words are never staged into the
    //    Vietnamese dictionary:
    let (buf3, _) = type_sequence(&mut session, &mut engine, "feeel ");
    assert_eq!(buf3, "feel ");
    let _ = session.reset(&mut engine);
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("vi:feeel"));
    assert!(!engine
        .store()
        .state()
        .untouched_unknowns
        .contains_key("feeel"));
}

#[test]
fn test_vi_lotus_sentence_start_english_passthrough_and_surrounding_undo_guard() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Safe sentence-start TitleCase passthrough (`If`, `Of`, `Or`), including stepwise BackSpace
    //    revert back to Lotus's Vietnamese rune (`Ò`):
    let (if_buf, _) = type_sequence(&mut session, &mut engine, "If ");
    assert_eq!(if_buf, "If ");

    let _ = session.reset(&mut engine);
    let (mut of_buf, _) = type_sequence(&mut session, &mut engine, "Of ");
    assert_eq!(of_buf, "Of ");
    let bs_step = session.process_key(&mut engine, KeyInput::BackSpace, 2_450);
    assert_eq!(bs_step.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut of_buf, KeyInput::BackSpace, &bs_step.action);
    assert_eq!(of_buf, "Of");
    let bs = session.process_key(&mut engine, KeyInput::BackSpace, 2_500);
    apply_key_to_buffer(&mut of_buf, KeyInput::BackSpace, &bs.action);
    assert_eq!(of_buf, "Ò");

    let _ = session.reset(&mut engine);
    let (or_buf, _) = type_sequence(&mut session, &mut engine, "Or ");
    assert_eq!(or_buf, "Or ");

    // 2. Sentence-initial real Vietnamese starters (`Á`, `Í`, `Há`) remain shielded Vietnamese:
    for (keys, expected) in [
        ("As haauj ", "Á hậu "),
        ("Is thooi ", "Í thôi "),
        ("Has mieengj ", "Há miệng "),
    ] {
        let _ = session.reset(&mut engine);
        let (vi_buf, _) = type_sequence(&mut session, &mut engine, keys);
        assert_eq!(vi_buf, expected);
    }

    // 3. Displaced-cursor `reconcile_surrounding` in `vi` mode (> 400 ms echo guard) resets
    //    `JustAutocorrected` so clicking away cancels the `1x BackSpace` undo:
    let _ = session.reset(&mut engine);
    let (rescued_buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(rescued_buf, "tiếng ");
    // Within 400ms echo guard -> ignored:
    session.reconcile_surrounding(&mut engine, "elsewhere", 4, 1_500);
    // Past 400ms echo guard at displaced cursor -> resets JustAutocorrected:
    session.reconcile_surrounding(&mut engine, "elsewhere", 4, 2_000);
    let bs_after_click = session.process_key(&mut engine, KeyInput::BackSpace, 2_050);
    assert_eq!(bs_after_click.action, SessionAction::PassThrough);
}

#[test]
fn test_vi_lotus_english_corrections_isolated_while_vi_corrections_and_snippets_work() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    fn type_direct(
        session: &mut SmartKeySession,
        engine: &mut SmartKeyEngine,
        text: &str,
    ) -> String {
        let mut buf = String::new();
        let mut now_ms = 10_000u64;
        for ch in text.chars() {
            now_ms += 30;
            let key = KeyInput::Char(ch);
            let out = session.process_key(engine, key, now_ms);
            apply_key_to_buffer(&mut buf, key, &out.action);
        }
        buf
    }

    // 1. Top-level English `[corrections]` (`r = "or"`) must NOT fire when `lang == "vi"`,
    //    preserving Vietnamese shorthand `"r"` (*"rồi"*) in both precomposed and Telex flows,
    //    while still firing normally when switched to `lang == "en"`.
    engine.store_mut().add_correction(&LangId::en(), "r", "or");

    session.set_lang(LangId::vi());
    let _ = session.reset(&mut engine);
    assert_eq!(
        type_direct(&mut session, &mut engine, "b cứ hỏi "),
        "b cứ hỏi "
    );
    let _ = session.reset(&mut engine);
    assert_eq!(
        type_direct(&mut session, &mut engine, "mấy tuần r "),
        "mấy tuần r "
    );
    let _ = session.reset(&mut engine);
    let (telex_ask, _) = type_sequence(&mut session, &mut engine, "b cuws hoir ");
    assert_eq!(telex_ask, "b cứ hỏi ");
    let _ = session.reset(&mut engine);
    let (telex_weeks, _) = type_sequence(&mut session, &mut engine, "maays tuaanf r ");
    assert_eq!(telex_weeks, "mấy tuần r ");

    session.set_lang(LangId::en());
    let _ = session.reset(&mut engine);
    assert_eq!(type_direct(&mut session, &mut engine, "r "), "or ");
    let _ = session.reset(&mut engine);
    assert_eq!(
        type_direct(&mut session, &mut engine, "this r that "),
        "this or that "
    );

    // 2. `[lang.vi.corrections]` fires when `lang == "vi"` and NOT when `lang == "en"`.
    engine
        .store_mut()
        .add_correction(&LangId::vi(), "vntypo", "Việt Nam");

    session.set_lang(LangId::vi());
    let _ = session.reset(&mut engine);
    let (vi_corr_buf, _) = type_sequence(&mut session, &mut engine, "vntypo ");
    assert_eq!(vi_corr_buf, "Việt Nam ");

    session.set_lang(LangId::en());
    let _ = session.reset(&mut engine);
    assert_eq!(type_direct(&mut session, &mut engine, "vntypo "), "vntypo ");

    // 3. `[snippets]` remains global across both `"en"` and `"vi"`.
    engine
        .store_mut()
        .set_snippet(";addr", "123 Lê Lợi")
        .unwrap();

    session.set_lang(LangId::vi());
    let _ = session.reset(&mut engine);
    let (vi_snip_buf, _) = type_sequence(&mut session, &mut engine, ";addr ");
    assert_eq!(vi_snip_buf, "123 Lê Lợi ");

    session.set_lang(LangId::en());
    let _ = session.reset(&mut engine);
    assert_eq!(
        type_direct(&mut session, &mut engine, ";addr "),
        "123 Lê Lợi "
    );
}

#[test]
fn test_vi_lotus_omitted_mark_and_dropped_ng_with_undo_and_corpus_shield() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Omitted `ie`/`ye` circumflex + unique 1-mark + dropped `-n` before `-g` end-to-end
    //    across filtered restore breaks (`tienf`, `vietj`, `buocs`), `ddFreeStyle` (`dduocj`),
    //    and plain-ASCII breaks (`nhuwg`, `nhuwxg`, `hoafg`), while ambiguous `nuocs` stays untouched:
    let (buf, _) = type_sequence(
        &mut session,
        &mut engine,
        "tienf vietj dduocj buocs nhuwg nhuwxg hoafg nuocs ",
    );
    assert_eq!(buf, "tiền việt được bước nhưng những hoàng nuocs ");

    // 2. Stepwise `2x BackSpace` undo + `Space` on `nhuwg -> nhưng` reverts and blocks pair:
    let _ = session.reset(&mut engine);
    let (mut undo_buf, _) = type_sequence(&mut session, &mut engine, "nhuwg ");
    assert_eq!(undo_buf, "nhưng ");
    let bs1 = session.process_key(&mut engine, KeyInput::BackSpace, 2_000);
    assert_eq!(bs1.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut undo_buf, KeyInput::BackSpace, &bs1.action);
    let bs2 = session.process_key(&mut engine, KeyInput::BackSpace, 2_050);
    apply_key_to_buffer(&mut undo_buf, KeyInput::BackSpace, &bs2.action);
    assert_eq!(undo_buf, "nhuwg");
    let sp = session.process_key(&mut engine, KeyInput::Char(' '), 2_100);
    apply_key_to_buffer(&mut undo_buf, KeyInput::Char(' '), &sp.action);
    assert_eq!(undo_buf, "nhuwg ");
    assert!(engine.store().is_pair_blocked("nhuwg", "nhưng"));

    // 3. Bare toneless Vietnamese names stay untouched:
    let _ = session.reset(&mut engine);
    let (names_buf, _) = type_sequence(&mut session, &mut engine, "Nguyen Duong ");
    assert_eq!(names_buf, "Nguyen Duong ");

    // 4. Every ASCII alphabetic entry in `tests/corpus/must_not_touch.txt` must never trigger
    //    `VietnameseTelexRescue` through `SmartKeyEngine::evaluate_token` in `vi` mode:
    let corpus = include_str!("../../../tests/corpus/must_not_touch.txt");
    for line in corpus.lines() {
        let word = line.trim();
        if word.is_empty()
            || word.starts_with('#')
            || !word.is_ascii()
            || !word.chars().all(|c| c.is_ascii_alphabetic())
        {
            continue;
        }
        let outcome = engine.evaluate_token(word, &EvalContext::default());
        assert!(
            !matches!(
                outcome,
                EvalOutcome::Autocorrect {
                    kind: MatchKind::VietnameseTelexRescue,
                    ..
                }
            ),
            "must_not_touch entry {word:?} falsely triggered VietnameseTelexRescue: {outcome:?}"
        );
    }
}

#[test]
fn test_vi_lotus_tier1_impossible_standalone_vowel_restore_and_guards() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. `"aws"` composes `"ắ"` (impossible toned open `ă` syllable) and is a known lexicon/dev word:
    //    auto-restores to `"aws"` at sentence start, mid-Vietnamese, and mid-English WITHOUT
    //    any `telex_passthrough` rule.
    assert!(!engine.is_telex_passthrough("aws"));
    let (start_buf, _) = type_sequence(&mut session, &mut engine, "aws ");
    assert_eq!(start_buf, "aws ");

    let _ = session.reset(&mut engine);
    let (vi_ctx_buf, _) = type_sequence(&mut session, &mut engine, "dufng aws ");
    assert_eq!(vi_ctx_buf, "dùng aws ");

    let _ = session.reset(&mut engine);
    let (en_ctx_buf, _) = type_sequence(&mut session, &mut engine, "use aws ");
    assert_eq!(en_ctx_buf, "use aws ");
    assert!(
        !engine
            .store()
            .state()
            .staged_telex_passthrough
            .contains_key("aws"),
        "Tier 1 auto-restored words need not be staged in staged_telex_passthrough"
    );

    // 2. Non-English `"awf"` stays `"ằ"`, and toneless `"aw"`, `"aa"`, `"dd"` stay Vietnamese:
    let _ = session.reset(&mut engine);
    let (non_en_buf, _) = type_sequence(&mut session, &mut engine, "chuw aw aa dd awf ");
    assert_eq!(non_en_buf, "chư ă â đ ằ ");

    // 3. Lotus Off mode (`lotus_composing = false`): `"aws "` stays `"aws "` via PassThrough.
    let _ = session.reset(&mut engine);
    session.set_lotus_composing(false);
    let mut off_buf = String::new();
    for (idx, ch) in "aws ".chars().enumerate() {
        let key = KeyInput::Char(ch);
        let out = session.process_key(&mut engine, key, 10_000 + (idx as u64) * 30);
        assert_eq!(out.action, SessionAction::PassThrough);
        apply_key_to_buffer(&mut off_buf, key, &out.action);
    }
    assert_eq!(off_buf, "aws ");
    session.set_lotus_composing(true);

    // 4. Stepwise BackSpace undo on Tier 1 `"aws "` (`2x BS` -> `"ắ"`, then Space) blocks `("ắ", "aws")`
    //    so subsequent `"aws "` stays `"ắ "`.
    let _ = session.reset(&mut engine);
    let (mut undo_buf, _) = type_sequence(&mut session, &mut engine, "aws ");
    assert_eq!(undo_buf, "aws ");
    let bs1 = session.process_key(&mut engine, KeyInput::BackSpace, 1_500);
    assert_eq!(bs1.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut undo_buf, KeyInput::BackSpace, &bs1.action);
    let bs2 = session.process_key(&mut engine, KeyInput::BackSpace, 1_540);
    apply_key_to_buffer(&mut undo_buf, KeyInput::BackSpace, &bs2.action);
    assert_eq!(undo_buf, "ắ");
    let sp = session.process_key(&mut engine, KeyInput::Char(' '), 1_580);
    apply_key_to_buffer(&mut undo_buf, KeyInput::Char(' '), &sp.action);
    assert_eq!(undo_buf, "ắ ");
    assert!(engine.store().is_pair_blocked("ắ", "aws"));

    let _ = session.reset(&mut engine);
    let (after_block, _) = type_sequence(&mut session, &mut engine, "aws ");
    assert_eq!(after_block, "ắ ");
}

#[test]
fn test_vi_lotus_tier2_passive_escapes_stage_only_without_hijacking_vietnamese() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // Typing `"Didd "` 3x in `vi-lotus` stages `"did"` for TUI review without silently graduating
    // `"did"` into `telex_passthrough`:
    for _ in 0..3 {
        let _ = session.reset(&mut engine);
        let (buf, _) = type_sequence(&mut session, &mut engine, "Didd ");
        assert_eq!(buf, "Did ");
    }
    assert!(!engine.is_telex_passthrough("did"));
    assert_eq!(
        engine
            .store()
            .state()
            .staged_telex_passthrough
            .get("did")
            .copied(),
        Some(3)
    );

    // Vietnamese `"anh đi"` and sentence-start `"Đi"` remain protected, while Momentum Shield
    // still restores `"what did "`:
    let _ = session.reset(&mut engine);
    let (vi_buf, _) = type_sequence(&mut session, &mut engine, "anh did ");
    assert_eq!(vi_buf, "anh đi ");

    let _ = session.reset(&mut engine);
    let (start_buf, _) = type_sequence(&mut session, &mut engine, "Did ");
    assert_eq!(start_buf, "Đi ");

    let _ = session.reset(&mut engine);
    let (mom_buf, _) = type_sequence(&mut session, &mut engine, "what did ");
    assert_eq!(mom_buf, "what did ");

    // Same for `"themee "` 3x: stages `"theme"` for review without hijacking `"thêm"`:
    for _ in 0..3 {
        let _ = session.reset(&mut engine);
        let (buf, _) = type_sequence(&mut session, &mut engine, "themee ");
        assert_eq!(buf, "theme ");
    }
    assert!(!engine.is_telex_passthrough("theme"));
    assert_eq!(
        engine
            .store()
            .state()
            .staged_telex_passthrough
            .get("theme")
            .copied(),
        Some(3)
    );

    let _ = session.reset(&mut engine);
    let (them_buf, _) = type_sequence(&mut session, &mut engine, "chọn theem ");
    assert_eq!(them_buf, "chọn thêm ");
    let _ = session.reset(&mut engine);
    let (theme_vi_buf, _) = type_sequence(&mut session, &mut engine, "chọn theme ");
    assert_eq!(theme_vi_buf, "chọn thêm ");
}

#[test]
fn test_vi_lotus_tier3_never_stages_pure_vietnamese_telex_repairs_or_escapes() {
    let (mut engine, mut session) = make_vi_engine_and_session();

    // 1. Backspacing `"thì "` into EditingPrevious and retyping `"thif "` (`"thì"`) in Lotus mode
    //    must NOT stage `"thif"` in `staged_telex_passthrough`:
    let (mut buf, _) = type_sequence(&mut session, &mut engine, "thif ");
    assert_eq!(buf, "thì ");
    let mut ts = 2_000u64;
    for _ in 0..4 {
        ts += 30;
        let out = session.process_key(&mut engine, KeyInput::BackSpace, ts);
        apply_key_to_buffer(&mut buf, KeyInput::BackSpace, &out.action);
    }
    assert_eq!(buf, "");
    for ch in "thif ".chars() {
        ts += 30;
        if ch != ' ' {
            let out = session.process_key(&mut engine, KeyInput::Char(ch), ts);
            assert_eq!(out.action, SessionAction::PassThrough);
        } else {
            buf.push_str("thì");
            let out = session.process_key(&mut engine, KeyInput::Char(' '), ts);
            apply_key_to_buffer(&mut buf, KeyInput::Char(' '), &out.action);
        }
    }
    assert_eq!(buf, "thì ");
    assert!(
        !engine
            .store()
            .state()
            .staged_telex_passthrough
            .contains_key("thif"),
        "pure Vietnamese Telex retype must never stage in staged_telex_passthrough"
    );

    // 2. Accidental double-key escape on non-English Telex (`"thiff "`) must NOT stage `"thif"`:
    let _ = session.reset(&mut engine);
    let (esc_buf, _) = type_sequence(&mut session, &mut engine, "thiff ");
    assert_eq!(esc_buf, "thif ");
    assert!(
        !engine
            .store()
            .state()
            .staged_telex_passthrough
            .contains_key("thif"),
        "non-English Telex double-key escape must never stage in staged_telex_passthrough"
    );
}

#[test]
fn test_vi_lotus_undo_demotes_machine_learned_but_preserves_manual_telex_passthrough() {
    // 1. Machine-learned `telex_passthrough` (`RuleOrigin::ManualFix2x`):
    //    `2x BackSpace + Space` reverts `"did "` -> `"đi "`, deletes `"did"` from
    //    `telex_passthrough`, clears any staged count, and permanently blocks `("đi", "did")`.
    let (mut engine, mut session) = make_vi_engine_and_session();
    engine.store_mut().add_telex_passthrough("did");
    engine
        .store_mut()
        .state_mut()
        .staged_telex_passthrough
        .insert("did".to_string(), 2);
    assert!(engine.is_telex_passthrough("did"));

    let (mut buf, _) = type_sequence(&mut session, &mut engine, "anh did ");
    assert_eq!(buf, "anh did ");
    let bs1 = session.process_key(&mut engine, KeyInput::BackSpace, 3_000);
    assert_eq!(bs1.action, SessionAction::PassThrough);
    apply_key_to_buffer(&mut buf, KeyInput::BackSpace, &bs1.action);
    let bs2 = session.process_key(&mut engine, KeyInput::BackSpace, 3_040);
    apply_key_to_buffer(&mut buf, KeyInput::BackSpace, &bs2.action);
    assert_eq!(buf, "anh đi");
    let sp = session.process_key(&mut engine, KeyInput::Char(' '), 3_080);
    apply_key_to_buffer(&mut buf, KeyInput::Char(' '), &sp.action);
    assert_eq!(buf, "anh đi ");

    assert!(
        !engine.is_telex_passthrough("did"),
        "undoing machine-learned telex_passthrough must demote it"
    );
    assert!(
        !engine
            .store()
            .state()
            .staged_telex_passthrough
            .contains_key("did"),
        "undoing telex_passthrough must clear staged_telex_passthrough count"
    );
    assert!(engine.store().is_pair_blocked("đi", "did"));
    let _ = session.reset(&mut engine);
    let (next_buf, _) = type_sequence(&mut session, &mut engine, "anh did ");
    assert_eq!(next_buf, "anh đi ");

    // 2. Manual `telex_passthrough` (`RuleOrigin::Manual`):
    //    undo keeps `"did"` in `telex_passthrough`, but permanent block `("đi", "did")` stops it firing.
    let (mut engine2, mut session2) = make_vi_engine_and_session();
    engine2.store_mut().add_user_telex_passthrough("did").unwrap();
    assert!(engine2.is_telex_passthrough("did"));

    let (mut buf2, _) = type_sequence(&mut session2, &mut engine2, "anh did ");
    assert_eq!(buf2, "anh did ");
    let bs1 = session2.process_key(&mut engine2, KeyInput::BackSpace, 4_000);
    apply_key_to_buffer(&mut buf2, KeyInput::BackSpace, &bs1.action);
    let bs2 = session2.process_key(&mut engine2, KeyInput::BackSpace, 4_040);
    apply_key_to_buffer(&mut buf2, KeyInput::BackSpace, &bs2.action);
    assert_eq!(buf2, "anh đi");
    let sp = session2.process_key(&mut engine2, KeyInput::Char(' '), 4_080);
    apply_key_to_buffer(&mut buf2, KeyInput::Char(' '), &sp.action);
    assert_eq!(buf2, "anh đi ");

    assert!(
        engine2.is_telex_passthrough("did"),
        "manual telex_passthrough must stay preserved on undo"
    );
    assert!(engine2.store().is_pair_blocked("đi", "did"));
    let _ = session2.reset(&mut engine2);
    let (next_buf2, _) = type_sequence(&mut session2, &mut engine2, "anh did ");
    assert_eq!(next_buf2, "anh đi ");
}
