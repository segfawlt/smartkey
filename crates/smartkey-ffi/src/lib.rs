#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::slice;

use smartkey_core::{
    AppProfile, EnglishLexicon, EnglishScorer, KeyInput, LangId, LearnEvent, Preset, RuleStore,
    SessionAction, SmartKeyEngine, SmartKeySession, UserRuleView,
};

pub const SMARTKEY_PRESET_BALANCED: u8 = 0;
pub const SMARTKEY_PRESET_CHAT_CASUAL: u8 = 1;
pub const SMARTKEY_PRESET_DEVELOPER_STRICT: u8 = 2;

pub const SMARTKEY_PROFILE_BYPASS: u8 = 0;
pub const SMARTKEY_PROFILE_BROWSER: u8 = 1;
pub const SMARTKEY_PROFILE_CHAT: u8 = 2;
pub const SMARTKEY_PROFILE_PROSE: u8 = 3;

pub const SMARTKEY_KEY_CHAR: u8 = 0;
pub const SMARTKEY_KEY_BACKSPACE: u8 = 1;
pub const SMARTKEY_KEY_CTRL_BACKSPACE: u8 = 2;
pub const SMARTKEY_KEY_ENTER: u8 = 3;
pub const SMARTKEY_KEY_TAB: u8 = 4;
pub const SMARTKEY_KEY_NAV_OR_CLICK: u8 = 5;
pub const SMARTKEY_KEY_FOCUS_LOST: u8 = 6;

pub const SMARTKEY_ACTION_PASS_THROUGH: u8 = 0;
pub const SMARTKEY_ACTION_REPLACE: u8 = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SmartKeyAction {
    pub kind: u8,
    pub delete_chars: u32,
    pub insert_utf8: *const c_char,
    pub insert_byte_len: u32,
}

impl Default for SmartKeyAction {
    fn default() -> Self {
        Self {
            kind: SMARTKEY_ACTION_PASS_THROUGH,
            delete_chars: 0,
            insert_utf8: ptr::null(),
            insert_byte_len: 0,
        }
    }
}

/// Single-threaded engine handle owned by the C/C++ host event loop.
/// Not `Send` or `Sync` (enforced via `_not_send_sync`); poisoned if an internal panic is caught.
pub struct SmartKeyEngineHandle {
    inner: SmartKeyEngine,
    poisoned: bool,
    _not_send_sync: std::marker::PhantomData<*mut ()>,
}

/// Single-threaded per-context session handle owned by the C/C++ host event loop.
pub struct SmartKeySessionHandle {
    inner: SmartKeySession,
    /// NUL-terminated scratch buffer keeping `SmartKeyAction::insert_utf8` valid with zero leaks.
    scratch_insert: Vec<u8>,
    poisoned: bool,
    _not_send_sync: std::marker::PhantomData<*mut ()>,
}

fn decode_preset(preset: u8) -> Preset {
    match preset {
        SMARTKEY_PRESET_CHAT_CASUAL => Preset::ChatCasual,
        SMARTKEY_PRESET_DEVELOPER_STRICT => Preset::DeveloperStrict,
        _ => Preset::Balanced,
    }
}

fn decode_profile(profile: u8) -> AppProfile {
    match profile {
        SMARTKEY_PROFILE_BYPASS => AppProfile::Bypass,
        SMARTKEY_PROFILE_BROWSER => AppProfile::Browser,
        SMARTKEY_PROFILE_CHAT => AppProfile::Chat,
        _ => AppProfile::Prose,
    }
}

unsafe fn opt_cstr<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    CStr::from_ptr(ptr).to_str().ok().filter(|s| !s.is_empty())
}

/// Distinguishes an intentional `NULL` pointer (`Ok(None)` -> in-memory store) from a non-NULL
/// empty/whitespace or non-UTF-8 path (`Err(())` -> fail `smartkey_engine_new` instead of silently
/// dropping persistence).
unsafe fn parse_optional_path<'a>(ptr: *const c_char) -> Result<Option<&'a str>, ()> {
    if ptr.is_null() {
        return Ok(None);
    }
    let s = CStr::from_ptr(ptr).to_str().map_err(|_| ())?;
    if s.trim().is_empty() {
        return Err(());
    }
    Ok(Some(s))
}

unsafe fn with_engine_ref<R>(
    engine: *const SmartKeyEngineHandle,
    default: R,
    f: impl FnOnce(&SmartKeyEngine) -> R,
) -> R {
    if engine.is_null() || (*engine).poisoned {
        return default;
    }
    catch_unwind(AssertUnwindSafe(|| f(&(*engine).inner))).unwrap_or(default)
}

unsafe fn with_engine_mut<R>(
    engine: *mut SmartKeyEngineHandle,
    default: R,
    f: impl FnOnce(&mut SmartKeyEngine) -> R,
) -> R {
    if engine.is_null() || (*engine).poisoned {
        return default;
    }
    match catch_unwind(AssertUnwindSafe(|| f(&mut (*engine).inner))) {
        Ok(v) => v,
        Err(_) => {
            (*engine).poisoned = true;
            default
        }
    }
}

unsafe fn with_session_mut<R>(
    session: *mut SmartKeySessionHandle,
    default: R,
    f: impl FnOnce(&mut SmartKeySession) -> R,
) -> R {
    if session.is_null() || (*session).poisoned {
        return default;
    }
    match catch_unwind(AssertUnwindSafe(|| f(&mut (*session).inner))) {
        Ok(v) => v,
        Err(_) => {
            (*session).poisoned = true;
            default
        }
    }
}

unsafe fn with_session_and_engine_mut<R>(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    default: R,
    f: impl FnOnce(&mut SmartKeySessionHandle, &mut SmartKeyEngine) -> R,
) -> R {
    if session.is_null() || engine.is_null() || (*session).poisoned || (*engine).poisoned {
        return default;
    }
    match catch_unwind(AssertUnwindSafe(|| f(&mut *session, &mut (*engine).inner))) {
        Ok(v) => v,
        Err(_) => {
            (*session).poisoned = true;
            (*engine).poisoned = true;
            default
        }
    }
}

fn build_store(
    rules_path_utf8: *const c_char,
    state_path_utf8: *const c_char,
    preset: u8,
) -> Result<RuleStore, ()> {
    let p = decode_preset(preset);
    let rules_opt = unsafe { parse_optional_path(rules_path_utf8)? };
    let state_opt = unsafe { parse_optional_path(state_path_utf8)? };

    match rules_opt {
        None => Ok(RuleStore::in_memory(p)),
        Some(rp) => {
            let r_path = PathBuf::from(rp);
            let s_path = match state_opt {
                Some(sp) => PathBuf::from(sp),
                None => r_path.with_file_name("learner_state.json"),
            };
            RuleStore::load_or_init(r_path, s_path, p).map_err(|_| ())
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_new(
    rules_path_utf8: *const c_char,
    state_path_utf8: *const c_char,
    preset: u8,
) -> *mut SmartKeyEngineHandle {
    catch_unwind(|| {
        let Ok(store) = build_store(rules_path_utf8, state_path_utf8, preset) else {
            return ptr::null_mut();
        };

        Box::into_raw(Box::new(SmartKeyEngineHandle {
            inner: SmartKeyEngine::new_auto(store),
            poisoned: false,
            _not_send_sync: std::marker::PhantomData,
        }))
    })
    .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_reload_dicts(engine: *mut SmartKeyEngineHandle) -> bool {
    with_engine_mut(engine, false, |eng| eng.reload_dicts())
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_new_with_lexicon_bytes(
    skdict_bytes: *const u8,
    skdict_len: usize,
    rules_path_utf8: *const c_char,
    state_path_utf8: *const c_char,
    preset: u8,
) -> *mut SmartKeyEngineHandle {
    catch_unwind(|| {
        let Ok(store) = build_store(rules_path_utf8, state_path_utf8, preset) else {
            return ptr::null_mut();
        };
        let scorer = if !skdict_bytes.is_null() && skdict_len > 0 {
            let bytes = slice::from_raw_parts(skdict_bytes, skdict_len).to_vec();
            let Ok(lex) = EnglishLexicon::from_owned_bytes(bytes) else {
                return ptr::null_mut();
            };
            Some(EnglishScorer::new(lex))
        } else {
            None
        };
        Box::into_raw(Box::new(SmartKeyEngineHandle {
            inner: SmartKeyEngine::new(scorer, store),
            poisoned: false,
            _not_send_sync: std::marker::PhantomData,
        }))
    })
    .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_free(engine: *mut SmartKeyEngineHandle) {
    if !engine.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(Box::from_raw(engine));
        }));
    }
}

#[no_mangle]
pub extern "C" fn smartkey_session_new() -> *mut SmartKeySessionHandle {
    catch_unwind(|| {
        Box::into_raw(Box::new(SmartKeySessionHandle {
            inner: SmartKeySession::new(),
            scratch_insert: vec![0],
            poisoned: false,
            _not_send_sync: std::marker::PhantomData,
        }))
    })
    .unwrap_or(ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_free(session: *mut SmartKeySessionHandle) {
    if !session.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(Box::from_raw(session));
        }));
    }
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_reset(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
) {
    with_session_and_engine_mut(session, engine, (), |sess, eng| {
        let _ = sess.inner.reset(eng);
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_set_profile(
    session: *mut SmartKeySessionHandle,
    profile: u8,
) {
    with_session_mut(session, (), |sess| {
        sess.set_profile(decode_profile(profile));
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_set_lang(
    session: *mut SmartKeySessionHandle,
    lang_utf8: *const c_char,
) {
    with_session_mut(session, (), |sess| {
        if let Some(lang) = opt_cstr(lang_utf8) {
            sess.set_lang(LangId(lang.to_string()));
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_set_app(
    session: *mut SmartKeySessionHandle,
    app_utf8: *const c_char,
) {
    with_session_mut(session, (), |sess| {
        sess.set_app(opt_cstr(app_utf8));
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_set_log_dir(
    engine: *mut SmartKeyEngineHandle,
    log_dir_utf8: *const c_char,
) -> bool {
    with_engine_mut(engine, false, |eng| {
        let dir = opt_cstr(log_dir_utf8).map(PathBuf::from);
        eng.store_mut().set_log_dir(dir);
        true
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_log_app_event(
    engine: *mut SmartKeyEngineHandle,
    app_utf8: *const c_char,
    profile: u8,
    reason_utf8: *const c_char,
) {
    with_engine_mut(engine, (), |eng| {
        let app = opt_cstr(app_utf8).unwrap_or("");
        let reason = opt_cstr(reason_utf8).unwrap_or("unknown");
        let prof = decode_profile(profile);
        eng.store_mut().log_app_bypass(app, prof, reason);
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_process_key(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    key_kind: u8,
    codepoint: u32,
    now_ms: u64,
    out_action: *mut SmartKeyAction,
) -> bool {
    if out_action.is_null() {
        return false;
    }
    *out_action = SmartKeyAction::default();

    with_session_and_engine_mut(session, engine, false, |sess, eng| {
        let key = match key_kind {
            SMARTKEY_KEY_CHAR => {
                let Some(ch) = char::from_u32(codepoint) else {
                    return false;
                };
                KeyInput::Char(ch)
            }
            SMARTKEY_KEY_BACKSPACE => KeyInput::BackSpace,
            SMARTKEY_KEY_CTRL_BACKSPACE => KeyInput::CtrlBackSpace,
            SMARTKEY_KEY_ENTER => KeyInput::Enter,
            SMARTKEY_KEY_TAB => KeyInput::Tab,
            SMARTKEY_KEY_NAV_OR_CLICK => KeyInput::NavOrClick,
            SMARTKEY_KEY_FOCUS_LOST => KeyInput::FocusLost,
            _ => return false,
        };

        let out = sess.inner.process_key(eng, key, now_ms);

        match out.action {
            SessionAction::PassThrough => {
                *out_action = SmartKeyAction::default();
            }
            SessionAction::Replace {
                delete_chars,
                insert,
            } => {
                sess.scratch_insert.clear();
                sess.scratch_insert.extend_from_slice(insert.as_bytes());
                sess.scratch_insert.push(0);
                *out_action = SmartKeyAction {
                    kind: SMARTKEY_ACTION_REPLACE,
                    delete_chars: delete_chars as u32,
                    insert_utf8: sess.scratch_insert.as_ptr() as *const c_char,
                    insert_byte_len: insert.len() as u32,
                };
            }
        }
        true
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_vi_lotus_enabled(
    engine: *const SmartKeyEngineHandle,
) -> bool {
    with_engine_ref(engine, false, |eng| eng.store().compiled().vi_lotus_enabled)
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_english_word(
    engine: *const SmartKeyEngineHandle,
    word_utf8: *const c_char,
    out_freq: *mut u8,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        let Some(word) = opt_cstr(word_utf8) else {
            return false;
        };
        let lower = word.to_lowercase();
        if eng.store().is_personal_word(&lower) {
            if !out_freq.is_null() {
                *out_freq = 200;
            }
            return true;
        }
        if let Some(lex) = eng.lexicon() {
            let dialect = eng.store().scorer_config().dialect;
            if let Some(meta) = lex.lookup_word(&lower) {
                if meta.is_valid_for_dialect(dialect) {
                    if !out_freq.is_null() {
                        *out_freq = meta.effective_freq(dialect);
                    }
                    return true;
                }
            }
        }
        false
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_telex_passthrough(
    engine: *const SmartKeyEngineHandle,
    raw_ascii_utf8: *const c_char,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        let Some(w) = opt_cstr(raw_ascii_utf8) else {
            return false;
        };
        eng.is_telex_passthrough(w)
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_record_telex_passthrough_repair(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    raw_ascii_utf8: *const c_char,
) -> bool {
    with_session_and_engine_mut(session, engine, false, |sess, eng| {
        let Some(w) = opt_cstr(raw_ascii_utf8) else {
            return false;
        };
        matches!(
            sess.inner.on_lotus_passthrough_repair(eng, w),
            LearnEvent::GraduatedTelexPassthrough { .. }
        )
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_pair_blocked(
    engine: *const SmartKeyEngineHandle,
    typed_utf8: *const c_char,
    candidate_utf8: *const c_char,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        let (Some(t), Some(c)) = (opt_cstr(typed_utf8), opt_cstr(candidate_utf8)) else {
            return false;
        };
        eng.store()
            .is_pair_blocked(&t.to_lowercase(), &c.to_lowercase())
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_personal_word(
    engine: *const SmartKeyEngineHandle,
    word_utf8: *const c_char,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        let Some(w) = opt_cstr(word_utf8) else {
            return false;
        };
        eng.store().is_personal_word(&w.to_lowercase())
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_mobile_on_auto_commit(
    session: *mut SmartKeySessionHandle,
    raw_typed_utf8: *const c_char,
    applied_utf8: *const c_char,
) {
    with_session_mut(session, (), |sess| {
        if let (Some(raw), Some(app)) = (opt_cstr(raw_typed_utf8), opt_cstr(applied_utf8)) {
            sess.on_mobile_auto_commit(raw, app);
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_mobile_on_undo_keep(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    raw_typed_utf8: *const c_char,
    rejected_utf8: *const c_char,
) {
    with_session_and_engine_mut(session, engine, (), |sess, eng| {
        if let (Some(raw), Some(rej)) = (opt_cstr(raw_typed_utf8), opt_cstr(rejected_utf8)) {
            let _ = sess.inner.on_mobile_undo_keep(eng, raw, rej);
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_mobile_on_manual_fix(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    orig_utf8: *const c_char,
    fixed_utf8: *const c_char,
) {
    with_session_and_engine_mut(session, engine, (), |sess, eng| {
        if let (Some(orig), Some(fixed)) = (opt_cstr(orig_utf8), opt_cstr(fixed_utf8)) {
            let _ = sess.inner.on_mobile_manual_fix(eng, orig, fixed);
        }
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_mobile_on_untouched_commit(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    word_utf8: *const c_char,
) {
    with_session_and_engine_mut(session, engine, (), |sess, eng| {
        if let Some(word) = opt_cstr(word_utf8) {
            let _ = sess.inner.on_mobile_untouched_commit(eng, word);
        }
    });
}
