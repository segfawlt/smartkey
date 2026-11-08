use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use smartkey_core::{
    apply_key_to_buffer, EvalContext, EvalOutcome, KeyInput, LangId, MatchKind, Preset, RuleStore,
    SessionAction, SmartKeyEngine, SmartKeySession,
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

    // Simulate how Lotus updates the screen during word typing + how SmartKey's PostInputMethod
    // replacement acts at the boundary:
    let mut cur_word_keys = String::new();
    for ch in seq.chars() {
        now_ms += 40;
        let is_boundary = matches!(ch, ' ' | ',' | '.' | '!' | '?');
        if !is_boundary {
            cur_word_keys.push(ch);
            let out = session.process_key(engine, KeyInput::Char(ch), now_ms);
            assert_eq!(
                out.action,
                SessionAction::PassThrough,
                "non-boundary char {ch:?} must never be filtered in PreInputMethod"
            );
            actions.push(out.action);
        } else {
            // Right before the boundary key, Lotus's `oldPreBuffer_` has put `preedit_before_break`
            // on screen (or `committed_at_break` if Lotus composed a valid Vietnamese word):
            if !cur_word_keys.is_empty() {
                let sim = smartkey_core::simulate_lotus_word(&cur_word_keys);
                if sim.is_composed_vietnamese {
                    lotus_screen_buf.push_str(&sim.committed_at_break);
                } else {
                    lotus_screen_buf.push_str(&sim.preedit_before_break);
                }
                cur_word_keys.clear();
            }
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
