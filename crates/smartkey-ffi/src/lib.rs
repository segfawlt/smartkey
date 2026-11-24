#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::ptr;
use std::slice;

use smartkey_core::{
    AppProfile, KeyInput, LangId, LatinScorer, LearnEvent, PackLexicon, Preset, RuleStore,
    SessionAction, SmartKeyEngine, SmartKeySession, UserRuleView,
};

pub const SMARTKEY_PRESET_BALANCED: u8 = 0;
pub const SMARTKEY_PRESET_CHAT_CASUAL: u8 = 1;
pub const SMARTKEY_PRESET_DEVELOPER_STRICT: u8 = 2;

pub const SMARTKEY_PROFILE_BYPASS: u8 = 0;
pub const SMARTKEY_PROFILE_BROWSER: u8 = 1;
pub const SMARTKEY_PROFILE_CHAT: u8 = 2;
pub const SMARTKEY_PROFILE_PROSE: u8 = 3;
pub const SMARTKEY_PROFILE_NONE: u8 = 255;

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

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SmartKeySessionSnapshot {
    pub mode_utf8: *const c_char,
    pub buf_utf8: *const c_char,
    pub buf_byte_len: u32,
    pub aux_utf8: *const c_char,
    pub aux_byte_len: u32,
    pub prev_utf8: *const c_char,
    pub prev_byte_len: u32,
    pub trailing_count: u32,
    pub lotus_composing: bool,
}

impl Default for SmartKeySessionSnapshot {
    fn default() -> Self {
        Self {
            mode_utf8: c"Idle".as_ptr(),
            buf_utf8: c"".as_ptr(),
            buf_byte_len: 0,
            aux_utf8: c"".as_ptr(),
            aux_byte_len: 0,
            prev_utf8: c"".as_ptr(),
            prev_byte_len: 0,
            trailing_count: 0,
            lotus_composing: true,
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

fn encode_profile(profile: AppProfile) -> u8 {
    match profile {
        AppProfile::Bypass => SMARTKEY_PROFILE_BYPASS,
        AppProfile::Browser => SMARTKEY_PROFILE_BROWSER,
        AppProfile::Chat => SMARTKEY_PROFILE_CHAT,
        AppProfile::Prose => SMARTKEY_PROFILE_PROSE,
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

unsafe fn with_session_ref<R>(
    session: *const SmartKeySessionHandle,
    default: R,
    f: impl FnOnce(&SmartKeySession) -> R,
) -> R {
    if session.is_null() || (*session).poisoned {
        return default;
    }
    catch_unwind(AssertUnwindSafe(|| f(&(*session).inner))).unwrap_or(default)
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

fn build_store_readonly(
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
            RuleStore::load_readonly_or_default(r_path, s_path, p)
                .map(|(store, _)| store)
                .map_err(|_| ())
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
pub unsafe extern "C" fn smartkey_engine_new_readonly(
    rules_path_utf8: *const c_char,
    state_path_utf8: *const c_char,
    preset: u8,
) -> *mut SmartKeyEngineHandle {
    catch_unwind(|| {
        let Ok(store) = build_store_readonly(rules_path_utf8, state_path_utf8, preset) else {
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
pub unsafe extern "C" fn smartkey_engine_is_enabled(engine: *const SmartKeyEngineHandle) -> bool {
    with_engine_ref(engine, false, |eng| eng.store().compiled().enabled)
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_set_enabled(
    engine: *mut SmartKeyEngineHandle,
    enabled: bool,
) -> bool {
    with_engine_mut(engine, false, |eng| eng.store_mut().set_enabled(enabled).is_ok())
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_toggle_enabled(
    engine: *mut SmartKeyEngineHandle,
    out_enabled: *mut bool,
) -> bool {
    with_engine_mut(engine, false, |eng| {
        eng.store_mut().reload_if_modified();
        let target = !eng.store().compiled().enabled;
        match eng.store_mut().set_enabled(target) {
            Ok(()) => {
                if !out_enabled.is_null() {
                    *out_enabled = target;
                }
                true
            }
            Err(_) => {
                if !out_enabled.is_null() {
                    *out_enabled = eng.store().compiled().enabled;
                }
                false
            }
        }
    })
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
            let Ok(lex) = PackLexicon::from_owned_bytes(bytes) else {
                return ptr::null_mut();
            };
            Some(LatinScorer::new(lex))
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

fn static_mode_cstr(mode: &str) -> *const c_char {
    match mode {
        "Idle" => c"Idle".as_ptr(),
        "Composing" => c"Composing".as_ptr(),
        "PendingDot" => c"PendingDot".as_ptr(),
        "PendingSemi" => c"PendingSemi".as_ptr(),
        "AfterWord" => c"AfterWord".as_ptr(),
        "JustAutocorrected" => c"JustAutocorrected".as_ptr(),
        "EditingRestored" => c"EditingRestored".as_ptr(),
        "EditingPrevious" => c"EditingPrevious".as_ptr(),
        _ => c"Unknown".as_ptr(),
    }
}

fn borrowed_str_parts(s: &str) -> (*const c_char, u32) {
    if s.is_empty() {
        (c"".as_ptr(), 0)
    } else {
        (
            s.as_ptr() as *const c_char,
            s.len().min(u32::MAX as usize) as u32,
        )
    }
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_is_trace_enabled(
    engine: *const SmartKeyEngineHandle,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        eng.store().compiled().diagnostics.trace_log
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_debug_snapshot(
    session: *const SmartKeySessionHandle,
    out_snapshot: *mut SmartKeySessionSnapshot,
) -> bool {
    if out_snapshot.is_null() {
        return false;
    }
    *out_snapshot = SmartKeySessionSnapshot::default();
    with_session_ref(session, false, |sess| {
        let snap = sess.debug_snapshot();
        let (buf_utf8, buf_byte_len) = borrowed_str_parts(snap.active_buf);
        let (aux_utf8, aux_byte_len) = borrowed_str_parts(snap.aux_text);
        let (prev_utf8, prev_byte_len) = borrowed_str_parts(snap.prev_word);
        *out_snapshot = SmartKeySessionSnapshot {
            mode_utf8: static_mode_cstr(snap.mode),
            buf_utf8,
            buf_byte_len,
            aux_utf8,
            aux_byte_len,
            prev_utf8,
            prev_byte_len,
            trailing_count: snap.trailing_count.min(u32::MAX as usize) as u32,
            lotus_composing: snap.lotus_composing,
        };
        true
    })
}

fn static_reason_cstr(reason: &str) -> *const c_char {
    match reason {
        "user-app-override" => c"user-app-override".as_ptr(),
        "unknown-app-policy" => c"unknown-app-policy".as_ptr(),
        "program-list" => c"program-list".as_ptr(),
        "browser" => c"browser".as_ptr(),
        "chat" => c"chat".as_ptr(),
        _ => c"prose".as_ptr(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_classify_app_override(
    engine: *const SmartKeyEngineHandle,
    app_utf8: *const c_char,
) -> u8 {
    with_engine_ref(engine, SMARTKEY_PROFILE_NONE, |eng| {
        let app = if app_utf8.is_null() {
            ""
        } else {
            CStr::from_ptr(app_utf8).to_str().unwrap_or("")
        };
        eng.store()
            .compiled()
            .app_profiles
            .classify_override(app)
            .map(encode_profile)
            .unwrap_or(SMARTKEY_PROFILE_NONE)
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_engine_classify_app(
    engine: *const SmartKeyEngineHandle,
    app_utf8: *const c_char,
    out_reason_utf8: *mut *const c_char,
) -> u8 {
    if !out_reason_utf8.is_null() {
        *out_reason_utf8 = c"prose".as_ptr();
    }
    let app = if app_utf8.is_null() {
        ""
    } else {
        CStr::from_ptr(app_utf8).to_str().unwrap_or("")
    };
    with_engine_ref(engine, SMARTKEY_PROFILE_PROSE, |eng| {
        let (prof, reason) = eng.store().compiled().app_profiles.classify_app(app);
        if !out_reason_utf8.is_null() {
            *out_reason_utf8 = static_reason_cstr(reason);
        }
        encode_profile(prof)
    })
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_reconcile_surrounding(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    surrounding_utf8: *const c_char,
    cursor_chars: u32,
    now_ms: u64,
) {
    with_session_and_engine_mut(session, engine, (), |sess, eng| {
        if surrounding_utf8.is_null() {
            return;
        }
        let Ok(text) = CStr::from_ptr(surrounding_utf8).to_str() else {
            return;
        };
        sess.inner
            .reconcile_surrounding(eng, text, cursor_chars as usize, now_ms);
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_set_lotus_composing(
    session: *mut SmartKeySessionHandle,
    composing: bool,
) {
    with_session_mut(session, (), |sess| {
        sess.set_lotus_composing(composing);
    });
}

#[no_mangle]
pub unsafe extern "C" fn smartkey_session_on_lotus_commit(
    session: *mut SmartKeySessionHandle,
    engine: *mut SmartKeyEngineHandle,
    boundary_codepoint: u32,
    committed_utf8: *const c_char,
    now_ms: u64,
    out_action: *mut SmartKeyAction,
) -> bool {
    if out_action.is_null() {
        return false;
    }
    *out_action = SmartKeyAction::default();

    with_session_and_engine_mut(session, engine, false, |sess, eng| {
        let Some(boundary_ch) = char::from_u32(boundary_codepoint) else {
            return false;
        };
        let Some(committed) = opt_cstr(committed_utf8) else {
            return false;
        };
        let out = sess
            .inner
            .on_lotus_boundary_commit(eng, boundary_ch, committed, now_ms);
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
pub unsafe extern "C" fn smartkey_engine_is_vi_dict_word(
    engine: *const SmartKeyEngineHandle,
    word_utf8: *const c_char,
) -> bool {
    with_engine_ref(engine, false, |eng| {
        let Some(word) = opt_cstr(word_utf8) else {
            return false;
        };
        let lower = word.to_lowercase();
        eng.is_vi_dict_word(&lower) || eng.is_vi_slang_word(&lower)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    #[test]
    fn ffi_classify_app_respects_builtins_overrides_and_unknown_policy() {
        let dir = std::env::temp_dir().join(format!("smartkey_ffi_app_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.toml");
        std::fs::write(
            &rules,
            "[app_profiles]\nunknown_app_policy = \"bypass\"\nprose = [\"obsidian\"]\n",
        )
        .unwrap();

        let state = dir.join("state.json");
        let c_rules = CString::new(rules.to_str().unwrap()).unwrap();
        let c_state = CString::new(state.to_str().unwrap()).unwrap();
        let eng = unsafe {
            smartkey_engine_new(c_rules.as_ptr(), c_state.as_ptr(), SMARTKEY_PRESET_BALANCED)
        };
        assert!(!eng.is_null());

        let check = |app: &str, expected_prof: u8, expected_reason: &str| {
            let c_app = CString::new(app).unwrap();
            let mut reason_ptr: *const c_char = std::ptr::null();
            let prof =
                unsafe { smartkey_engine_classify_app(eng, c_app.as_ptr(), &mut reason_ptr) };
            assert_eq!(prof, expected_prof, "unexpected profile for {app:?}");
            assert!(!reason_ptr.is_null());
            let reason = unsafe { CStr::from_ptr(reason_ptr) }.to_str().unwrap();
            assert_eq!(reason, expected_reason, "unexpected reason for {app:?}");
        };

        check("zcode", SMARTKEY_PROFILE_BYPASS, "program-list");
        check("firefox", SMARTKEY_PROFILE_BROWSER, "browser");
        check("vesktop", SMARTKEY_PROFILE_CHAT, "chat");
        check("obsidian", SMARTKEY_PROFILE_PROSE, "user-app-override");
        check(
            "my-unlisted-app",
            SMARTKEY_PROFILE_BYPASS,
            "unknown-app-policy",
        );
        check("", SMARTKEY_PROFILE_BYPASS, "unknown-app-policy");

        unsafe { smartkey_engine_free(eng) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffi_trace_enabled_and_debug_snapshot_across_session_modes() {
        let dir = std::env::temp_dir().join(format!(
            "smartkey_ffi_trace_{}_{}",
            std::process::id(),
            smartkey_core::event_log::current_epoch_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.toml");
        let state = dir.join("state.json");
        std::fs::write(&rules, "[diagnostics]\ntrace_log = false\n").unwrap();

        let c_rules = CString::new(rules.to_str().unwrap()).unwrap();
        let c_state = CString::new(state.to_str().unwrap()).unwrap();
        let eng = unsafe {
            smartkey_engine_new(c_rules.as_ptr(), c_state.as_ptr(), SMARTKEY_PRESET_BALANCED)
        };
        assert!(!eng.is_null());
        let sess = smartkey_session_new();
        assert!(!sess.is_null());

        assert!(!unsafe { smartkey_engine_is_trace_enabled(eng) });
        std::fs::write(&rules, "[diagnostics]\ntrace_log = true\n").unwrap();
        assert!(unsafe { smartkey_engine_reload_dicts(eng) });
        assert!(unsafe { smartkey_engine_is_trace_enabled(eng) });

        let read_snap = || -> (String, String, String, String, u32, bool) {
            let mut snap = SmartKeySessionSnapshot::default();
            assert!(unsafe { smartkey_session_debug_snapshot(sess, &mut snap) });
            let slice = |ptr: *const c_char, len: u32| -> String {
                if ptr.is_null() || len == 0 {
                    String::new()
                } else {
                    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
                    std::str::from_utf8(bytes).unwrap().to_string()
                }
            };
            let mode = unsafe { CStr::from_ptr(snap.mode_utf8) }
                .to_str()
                .unwrap()
                .to_string();
            (
                mode,
                slice(snap.buf_utf8, snap.buf_byte_len),
                slice(snap.aux_utf8, snap.aux_byte_len),
                slice(snap.prev_utf8, snap.prev_byte_len),
                snap.trailing_count,
                snap.lotus_composing,
            )
        };

        assert_eq!(
            read_snap(),
            ("Idle".to_string(), "".to_string(), "".to_string(), "".to_string(), 0, true)
        );

        let mut act = SmartKeyAction::default();
        for (idx, cp) in ['t' as u32, 'e' as u32, 'h' as u32].into_iter().enumerate() {
            assert!(unsafe {
                smartkey_session_process_key(
                    sess,
                    eng,
                    SMARTKEY_KEY_CHAR,
                    cp,
                    100 + idx as u64 * 10,
                    &mut act,
                )
            });
            assert_eq!(act.kind, SMARTKEY_ACTION_PASS_THROUGH);
        }
        assert_eq!(
            read_snap(),
            ("Composing".to_string(), "teh".to_string(), "".to_string(), "".to_string(), 0, true)
        );

        assert!(unsafe {
            smartkey_session_process_key(sess, eng, SMARTKEY_KEY_CHAR, ' ' as u32, 150, &mut act)
        });
        assert_eq!(act.kind, SMARTKEY_ACTION_REPLACE);
        assert_eq!(
            read_snap(),
            (
                "JustAutocorrected".to_string(),
                "the".to_string(),
                "teh".to_string(),
                "the".to_string(),
                1,
                true
            )
        );

        assert!(unsafe {
            smartkey_session_process_key(sess, eng, SMARTKEY_KEY_BACKSPACE, 0, 180, &mut act)
        });
        assert_eq!(act.kind, SMARTKEY_ACTION_PASS_THROUGH);
        assert_eq!(
            read_snap(),
            (
                "JustAutocorrected".to_string(),
                "the".to_string(),
                "teh".to_string(),
                "the".to_string(),
                0,
                true
            )
        );

        assert!(unsafe {
            smartkey_session_process_key(sess, eng, SMARTKEY_KEY_BACKSPACE, 0, 200, &mut act)
        });
        assert_eq!(act.kind, SMARTKEY_ACTION_REPLACE);
        assert_eq!(
            read_snap(),
            (
                "EditingRestored".to_string(),
                "teh".to_string(),
                "teh".to_string(),
                "".to_string(),
                0,
                true
            )
        );

        unsafe {
            smartkey_session_free(sess);
            smartkey_engine_free(eng);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffi_engine_enabled_set_toggle_syncs_external_edits_and_respects_parse_lock() {
        let dir = std::env::temp_dir().join(format!(
            "smartkey_ffi_toggle_{}_{}",
            std::process::id(),
            smartkey_core::event_log::current_epoch_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let rules = dir.join("rules.toml");
        let state = dir.join("state.json");
        let c_rules = CString::new(rules.to_str().unwrap()).unwrap();
        let c_state = CString::new(state.to_str().unwrap()).unwrap();
        let eng = unsafe {
            smartkey_engine_new(c_rules.as_ptr(), c_state.as_ptr(), SMARTKEY_PRESET_BALANCED)
        };
        assert!(!eng.is_null());

        assert!(unsafe { smartkey_engine_is_enabled(eng) });
        assert!(unsafe { smartkey_engine_set_enabled(eng, false) });
        assert!(!unsafe { smartkey_engine_is_enabled(eng) });
        assert!(std::fs::read_to_string(&rules).unwrap().contains("enabled = false"));

        // External edit (e.g. `smartkey enable`) flips enabled = true; toggle should reload first
        // and flip true -> false instead of oscillating on stale in-memory state.
        let updated = std::fs::read_to_string(&rules)
            .unwrap()
            .replace("enabled = false", "enabled = true");
        std::fs::write(&rules, updated).unwrap();
        let mut new_enabled = true;
        assert!(unsafe { smartkey_engine_toggle_enabled(eng, &mut new_enabled) });
        assert!(!new_enabled);
        assert!(!unsafe { smartkey_engine_is_enabled(eng) });

        // Corrupt rules.toml write-locks the store; toggle returns false cleanly without panic.
        std::fs::write(&rules, "[settings\nbroken = [1, 2\n").unwrap();
        let mut lock_state = true;
        assert!(!unsafe { smartkey_engine_toggle_enabled(eng, &mut lock_state) });
        assert!(!unsafe { smartkey_engine_set_enabled(eng, true) });
        assert!(!lock_state);

        unsafe { smartkey_engine_free(eng) };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ffi_engine_readonly_and_vi_dict_lookup() {
        let dir = std::env::temp_dir().join(format!(
            "smartkey_ffi_readonly_{}_{}",
            std::process::id(),
            smartkey_core::event_log::current_epoch_ms()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let rules = dir.join("rules.toml");
        let state = dir.join("state.json");
        let c_rules = CString::new(rules.to_str().unwrap()).unwrap();
        let c_state = CString::new(state.to_str().unwrap()).unwrap();

        let eng = unsafe {
            smartkey_engine_new_readonly(c_rules.as_ptr(), c_state.as_ptr(), SMARTKEY_PRESET_BALANCED)
        };
        assert!(!eng.is_null());
        assert!(
            !dir.exists(),
            "smartkey_engine_new_readonly must not create missing config directory"
        );

        let anh = CString::new("anh").unwrap();
        let thanh = CString::new("Thanh").unwrap();
        let fomr = CString::new("fomr").unwrap();
        assert!(unsafe { smartkey_engine_is_vi_dict_word(eng, anh.as_ptr()) });
        assert!(unsafe { smartkey_engine_is_vi_dict_word(eng, thanh.as_ptr()) });
        assert!(!unsafe { smartkey_engine_is_vi_dict_word(eng, fomr.as_ptr()) });

        unsafe { smartkey_engine_free(eng) };
    }
}
