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

    // Type `ttieengs ` -> `"tiếng "`, then `1x BackSpace` -> reverts to `"ttieengs"`!
    let (mut buf, _) = type_sequence(&mut session, &mut engine, "ttieengs ");
    assert_eq!(buf, "tiếng ");

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

    // 2. `1x BackSpace` immediately after multi-word Vietnamese shorthand expansion (`ntn -> như thế nào`)
    //    deletes the entire expanded phrase `"như thế nào "` and restores `"ntn"`, and Space blocks the pair!
    let (mut buf2, _) = type_sequence(&mut session, &mut engine, "ntn ");
    assert_eq!(buf2, "như thế nào ");
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

    // 4. English-Sentence Momentum Shield + `1x BackSpace` revert back to Vietnamese rune:
    let (mut mom_buf, _) = type_sequence(&mut session, &mut engine, "it is ");
    assert_eq!(mom_buf, "it is ");
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
