use std::ffi::CString;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use smartkey_core::{default_config_dir, default_state_log_dir, normalize_program_id};
use smartkey_ffi::{
    smartkey_engine_free, smartkey_engine_is_enabled, smartkey_engine_is_trace_enabled,
    smartkey_engine_log_app_event, smartkey_engine_new, smartkey_engine_new_readonly,
    smartkey_engine_set_log_dir, smartkey_session_free, smartkey_session_new,
    smartkey_session_reconcile_surrounding, smartkey_session_reset, smartkey_session_set_app,
    smartkey_session_set_lang, smartkey_session_set_profile, SmartKeyEngineHandle,
    SmartKeySessionHandle, SMARTKEY_KEY_FOCUS_LOST, SMARTKEY_PRESET_BALANCED,
    SMARTKEY_PROFILE_BYPASS, SMARTKEY_PROFILE_PROSE,
};
use windows::core::{implement, BOOL, IUnknownImpl, Interface, Ref, GUID};
use windows::Win32::Foundation::{E_FAIL, LPARAM, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::{VT_I4, VT_UNKNOWN};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, GetKeyboardLayout, GetKeyboardState, ToUnicodeEx, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfDocumentMgr, ITfEditRecord, ITfEditSession, ITfEditSession_Impl,
    ITfInputScope, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfRange,
    ITfSource, ITfTextEditSink, ITfTextEditSink_Impl, ITfTextInputProcessorEx,
    ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl, ITfThreadMgr,
    ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, ITfThreadMgrEx, GUID_PROP_INPUTSCOPE,
    TF_ANCHOR_END, TF_DEFAULT_SELECTION, TF_ES_READ, TF_ES_READWRITE, TF_ES_SYNC, TF_HALTCOND,
    TF_SELECTION, TF_TMF_IMMERSIVEMODE, TF_TMF_SECUREMODE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetMessageExtraInfo, GetWindowThreadProcessId,
};

use crate::register::{decrement_dll_ref, increment_dll_ref};
use crate::state::{
    classify_app_via_ffi, classify_tsf_context, verify_pre_cursor_utf16, FfiSessionBridge,
    KeyEventInput, PendingReplace, SessionBridge, TsfKeyTracker, VK_LWIN, VK_PACKET, VK_RWIN,
};
use crate::trace::{format_snapshot_compact, TraceLogger};

struct ServiceInner {
    thread_mgr: Option<ITfThreadMgr>,
    tid: u32,
    thread_mgr_cookie: u32,
    key_sink_advised: bool,
    edit_sink_cookie: u32,
    edit_sink_context: Option<ITfContext>,
    engine: *mut SmartKeyEngineHandle,
    session: *mut SmartKeySessionHandle,
    tracker: TsfKeyTracker,
    trace: Option<TraceLogger>,
    start: Instant,
    current_app: String,
    current_profile: u8,
    last_logged_profile: Option<(u8, &'static str)>,
    pending_high_surrogate: Option<u16>,
    readonly_mode: bool,
    ctx_counter: u64,
}

unsafe impl Send for ServiceInner {}

impl Default for ServiceInner {
    fn default() -> Self {
        Self {
            thread_mgr: None,
            tid: 0,
            thread_mgr_cookie: 0,
            key_sink_advised: false,
            edit_sink_cookie: 0,
            edit_sink_context: None,
            engine: std::ptr::null_mut(),
            session: std::ptr::null_mut(),
            tracker: TsfKeyTracker::new(),
            trace: None,
            start: Instant::now(),
            current_app: String::new(),
            current_profile: SMARTKEY_PROFILE_PROSE,
            last_logged_profile: None,
            pending_high_surrogate: None,
            readonly_mode: false,
            ctx_counter: 1,
        }
    }
}

impl Drop for ServiceInner {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe { smartkey_session_free(self.session) };
            self.session = std::ptr::null_mut();
        }
        if !self.engine.is_null() {
            unsafe { smartkey_engine_free(self.engine) };
            self.engine = std::ptr::null_mut();
        }
    }
}

#[implement(
    ITfTextInputProcessorEx,
    ITfThreadMgrEventSink,
    ITfTextEditSink,
    ITfKeyEventSink
)]
pub struct SmartKeyTextService {
    inner: Mutex<ServiceInner>,
    self_committing: Arc<AtomicBool>,
    tid_fast: AtomicU32,
}

impl SmartKeyTextService {
    pub fn new() -> Self {
        increment_dll_ref();
        Self {
            inner: Mutex::new(ServiceInner::default()),
            self_committing: Arc::new(AtomicBool::new(false)),
            tid_fast: AtomicU32::new(0),
        }
    }
}

impl Drop for SmartKeyTextService {
    fn drop(&mut self) {
        decrement_dll_ref();
    }
}

struct ScopedSelfCommit(Arc<AtomicBool>);

impl ScopedSelfCommit {
    fn enter(flag: &Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(Arc::clone(flag))
    }
}

impl Drop for ScopedSelfCommit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn foreground_process_basename() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return String::new();
    }
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return String::new();
    }
    let Ok(proc_handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };
    let mut buf = vec![0u16; 1024];
    let mut size = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            proc_handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(proc_handle);
    }
    if ok.is_err() || size == 0 {
        return String::new();
    }
    let full = String::from_utf16_lossy(&buf[..size as usize]);
    normalize_program_id(&full)
}

fn is_immersive_or_secure(tm: &ITfThreadMgr) -> (bool, bool) {
    let Ok(tm_ex) = tm.cast::<ITfThreadMgrEx>() else {
        return (false, false);
    };
    let Ok(flags) = (unsafe { tm_ex.GetActiveFlags() }) else {
        return (false, false);
    };
    (
        (flags & TF_TMF_IMMERSIVEMODE) != 0,
        (flags & TF_TMF_SECUREMODE) != 0,
    )
}

fn init_engine_for_process(immersive: bool) -> (*mut SmartKeyEngineHandle, bool, Option<PathBuf>) {
    let cfg_dir = default_config_dir();
    let rules_path = cfg_dir.join("rules.toml");
    let state_path = cfg_dir.join("learner_state.json");
    let log_dir = default_state_log_dir();

    let c_rules = CString::new(rules_path.to_string_lossy().as_bytes()).ok();
    let c_state = CString::new(state_path.to_string_lossy().as_bytes()).ok();

    if !immersive {
        if let (Some(cr), Some(cs)) = (&c_rules, &c_state) {
            let eng = unsafe {
                smartkey_engine_new(cr.as_ptr(), cs.as_ptr(), SMARTKEY_PRESET_BALANCED)
            };
            if !eng.is_null() {
                if let Ok(cl) = CString::new(log_dir.to_string_lossy().as_bytes()) {
                    unsafe { smartkey_engine_set_log_dir(eng, cl.as_ptr()) };
                }
                return (eng, false, Some(log_dir));
            }
        }
    }

    if let (Some(cr), Some(cs)) = (&c_rules, &c_state) {
        let eng = unsafe {
            smartkey_engine_new_readonly(cr.as_ptr(), cs.as_ptr(), SMARTKEY_PRESET_BALANCED)
        };
        if !eng.is_null() {
            return (eng, true, None);
        }
    }

    let eng = unsafe {
        smartkey_engine_new(
            std::ptr::null(),
            std::ptr::null(),
            SMARTKEY_PRESET_BALANCED,
        )
    };
    (eng, true, None)
}

fn read_utf16_before_caret(ec: u32, range: &ITfRange, max_units: usize) -> Option<Vec<u16>> {
    let clone = unsafe { range.Clone().ok()? };
    unsafe { clone.Collapse(ec, TF_ANCHOR_END).ok()? };
    let mut shifted: i32 = 0;
    let halt = TF_HALTCOND::default();
    unsafe {
        clone
            .ShiftStart(ec, -(max_units as i32), &mut shifted, &halt)
            .ok()?;
    }
    if shifted == 0 {
        return Some(Vec::new());
    }
    let want = (-shifted).clamp(0, max_units as i32) as usize;
    let mut buf = vec![0u16; want + 4];
    let mut got: u32 = 0;
    unsafe {
        clone.GetText(ec, 0, &mut buf, &mut got).ok()?;
    }
    buf.truncate(got as usize);
    Some(buf)
}

fn read_utf16_after_caret(ec: u32, range: &ITfRange, max_units: usize) -> Option<Vec<u16>> {
    let clone = unsafe { range.Clone().ok()? };
    unsafe { clone.Collapse(ec, TF_ANCHOR_END).ok()? };
    let mut shifted: i32 = 0;
    let halt = TF_HALTCOND::default();
    unsafe {
        clone
            .ShiftEnd(ec, max_units as i32, &mut shifted, &halt)
            .ok()?;
    }
    if shifted <= 0 {
        return Some(Vec::new());
    }
    let want = (shifted as usize).min(max_units);
    let mut buf = vec![0u16; want + 4];
    let mut got: u32 = 0;
    unsafe {
        clone.GetText(ec, 0, &mut buf, &mut got).ok()?;
    }
    buf.truncate(got as usize);
    Some(buf)
}

#[implement(ITfEditSession)]
struct ReadInputScopeEditSession {
    context: ITfContext,
    out_scopes: Arc<Mutex<Vec<i32>>>,
}

impl ITfEditSession_Impl for ReadInputScopeEditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> windows_core::Result<()> {
        let Ok(prop) = (unsafe { self.context.GetAppProperty(&GUID_PROP_INPUTSCOPE) }) else {
            return Ok(());
        };
        let mut sel = [TF_SELECTION::default()];
        let mut fetched: u32 = 0;
        if unsafe {
            self.context
                .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)
        }
        .is_err()
            || fetched == 0
        {
            return Ok(());
        }
        let range_opt = unsafe {
            core::mem::ManuallyDrop::take(&mut sel[0].range)
        };
        let Some(range) = range_opt else {
            return Ok(());
        };
        let Ok(var) = (unsafe { prop.GetValue(ec, &range) }) else {
            return Ok(());
        };

        let mut collected = Vec::new();
        let vt = unsafe { var.Anonymous.Anonymous.vt };
        if vt == VT_I4 {
            let scope_val = unsafe { var.Anonymous.Anonymous.Anonymous.lVal };
            collected.push(scope_val);
        } else if vt == VT_UNKNOWN {
            if let Some(unk) = unsafe { &*var.Anonymous.Anonymous.Anonymous.punkVal } {
                if let Ok(input_scope) = unk.cast::<ITfInputScope>() {
                    let mut ptr_scopes: *mut windows::Win32::UI::TextServices::InputScope =
                        std::ptr::null_mut();
                    let mut count: u32 = 0;
                    if unsafe { input_scope.GetInputScopes(&mut ptr_scopes, &mut count) }.is_ok()
                        && !ptr_scopes.is_null()
                    {
                        let slice =
                            unsafe { std::slice::from_raw_parts(ptr_scopes, count as usize) };
                        for s in slice {
                            collected.push(s.0);
                        }
                        unsafe { CoTaskMemFree(Some(ptr_scopes as *const core::ffi::c_void)) };
                    }
                }
            }
        }
        if let Ok(mut guard) = self.out_scopes.lock() {
            *guard = collected;
        }
        Ok(())
    }
}

#[implement(ITfEditSession)]
struct ReplaceEditSession {
    context: ITfContext,
    pending: PendingReplace,
    applied: Arc<AtomicBool>,
}

impl ITfEditSession_Impl for ReplaceEditSession_Impl {
    fn DoEditSession(&self, ec: u32) -> windows_core::Result<()> {
        let mut sel = [TF_SELECTION::default()];
        let mut fetched: u32 = 0;
        unsafe {
            self.context
                .GetSelection(ec, TF_DEFAULT_SELECTION, &mut sel, &mut fetched)?;
        }
        if fetched == 0 {
            return Err(E_FAIL.into());
        }
        let range_opt = unsafe { core::mem::ManuallyDrop::take(&mut sel[0].range) };
        let Some(range) = range_opt else {
            return Err(E_FAIL.into());
        };
        let is_empty = unsafe { range.IsEmpty(ec)? };
        if !is_empty.as_bool() {
            return Err(E_FAIL.into());
        }

        let utf16_units = if self.pending.delete_chars > 0 {
            let max_scan = (self.pending.delete_chars as usize).saturating_mul(2).saturating_add(8);
            let pre_utf16 = read_utf16_before_caret(ec, &range, max_scan).ok_or(E_FAIL)?;
            let Some(units) = verify_pre_cursor_utf16(
                &pre_utf16,
                self.pending.delete_chars,
                self.pending.expected_suffix.as_deref(),
            ) else {
                return Err(E_FAIL.into());
            };
            units
        } else {
            0
        };

        if utf16_units > 0 {
            let mut shifted: i32 = 0;
            let halt = TF_HALTCOND::default();
            unsafe {
                range.ShiftStart(ec, -(utf16_units as i32), &mut shifted, &halt)?;
            }
            if (-shifted) as usize != utf16_units {
                return Err(E_FAIL.into());
            }
        }

        let insert_w: Vec<u16> = self.pending.insert.encode_utf16().collect();
        unsafe {
            range.SetText(ec, 0, &insert_w)?;
            range.Collapse(ec, TF_ANCHOR_END)?;
            let new_sel = [TF_SELECTION {
                range: core::mem::ManuallyDrop::new(Some(range)),
                style: sel[0].style,
            }];
            let res = self.context.SetSelection(ec, &new_sel);
            let _ = core::mem::ManuallyDrop::into_inner(
                std::ptr::read(&new_sel[0].range),
            );
            res?;
        }
        self.applied.store(true, Ordering::SeqCst);
        Ok(())
    }
}

impl SmartKeyTextService {
    fn sync_context_classification(&self, context_opt: Option<&ITfContext>) {
        let tid = self.tid_fast.load(Ordering::SeqCst);
        let tm_opt = self
            .inner
            .lock()
            .ok()
            .and_then(|g| g.thread_mgr.clone());

        let (immersive, secure_mode) = tm_opt
            .as_ref()
            .map(is_immersive_or_secure)
            .unwrap_or((false, false));
        let _ = immersive;

        let mut scopes = Vec::new();
        if let Some(ctx) = context_opt {
            if tid != 0 {
                let out_scopes = Arc::new(Mutex::new(Vec::new()));
                let sess: ITfEditSession = ReadInputScopeEditSession {
                    context: ctx.clone(),
                    out_scopes: Arc::clone(&out_scopes),
                }
                .into();
                let mut hr_session = windows::core::HRESULT(0);
                if unsafe {
                    ctx.RequestEditSession(tid, &sess, TF_ES_READ | TF_ES_SYNC)
                }
                .map(|hr| hr_session = hr)
                .is_ok()
                    && hr_session.is_ok()
                {
                    if let Ok(guard) = out_scopes.lock() {
                        scopes = guard.clone();
                    }
                }
            }
        }

        let app_name = foreground_process_basename();
        if let Ok(mut g) = self.inner.lock() {
            g.current_app = app_name.clone();
            if !g.session.is_null() {
                if let Ok(c_app) = CString::new(app_name.as_str()) {
                    unsafe { smartkey_session_set_app(g.session, c_app.as_ptr()) };
                }
            }
            let engine_ptr = g.engine;
            let (profile, reason) = classify_tsf_context(secure_mode, &scopes, || unsafe {
                classify_app_via_ffi(engine_ptr, &app_name)
            });
            g.current_profile = profile;
            if !g.session.is_null() {
                unsafe { smartkey_session_set_profile(g.session, profile) };
            }
            if g.last_logged_profile != Some((profile, reason)) {
                g.last_logged_profile = Some((profile, reason));
                if !g.engine.is_null() {
                    if let (Ok(c_app), Ok(c_reason)) =
                        (CString::new(app_name.as_str()), CString::new(reason))
                    {
                        unsafe {
                            smartkey_engine_log_app_event(
                                g.engine,
                                c_app.as_ptr(),
                                profile,
                                c_reason.as_ptr(),
                            );
                        }
                    }
                }
            }
        }
    }

    fn unadvise_edit_sink(&self) {
        let (old_ctx, old_cookie) = if let Ok(mut g) = self.inner.lock() {
            (g.edit_sink_context.take(), std::mem::take(&mut g.edit_sink_cookie))
        } else {
            return;
        };
        if old_cookie != 0 {
            if let Some(ctx) = old_ctx {
                if let Ok(src) = ctx.cast::<ITfSource>() {
                    let _ = unsafe { src.UnadviseSink(old_cookie) };
                }
            }
        }
    }

    fn handle_focus_doc(&self, impl_this: &SmartKeyTextService_Impl, doc_opt: Option<&ITfDocumentMgr>) {
        self.unadvise_edit_sink();
        let top_ctx = doc_opt.and_then(|d| unsafe { d.GetTop().ok() });
        if let Ok(mut g) = self.inner.lock() {
            g.tracker.reset();
            g.pending_high_surrogate = None;
            g.ctx_counter = g.ctx_counter.saturating_add(1);
            if !g.session.is_null() && !g.engine.is_null() {
                unsafe { smartkey_session_reset(g.session, g.engine) };
            }
        }

        if let Some(ctx) = top_ctx.as_ref() {
            if let Ok(src) = ctx.cast::<ITfSource>() {
                let sink: ITfTextEditSink = impl_this.to_interface();
                if let Ok(cookie) =
                    unsafe { src.AdviseSink(&ITfTextEditSink::IID, &sink) }
                {
                    if let Ok(mut g) = self.inner.lock() {
                        g.edit_sink_cookie = cookie;
                        g.edit_sink_context = Some(ctx.clone());
                    }
                }
            }
        }

        self.sync_context_classification(top_ctx.as_ref());
    }

    fn decode_wparam_lparam(
        g: &mut ServiceInner,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> KeyEventInput {
        let vkey = (wparam.0 & 0xFFFF) as u16;
        let scan_word = ((lparam.0 >> 16) & 0xFFFF) as u16;
        let scan_code = scan_word & 0xFF;
        let ctrl = unsafe { (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 };
        let shift = unsafe { (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 };
        let alt = unsafe { (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 };
        let win = unsafe {
            (GetKeyState(VK_LWIN as i32) as u16 & 0x8000) != 0
                || (GetKeyState(VK_RWIN as i32) as u16 & 0x8000) != 0
        };
        let extra = unsafe { GetMessageExtraInfo().0 };
        let is_injected = extra != 0 || vkey == VK_PACKET;

        let unicode_char = if vkey == VK_PACKET {
            let unit = scan_word;
            if (0xD800..=0xDBFF).contains(&unit) {
                g.pending_high_surrogate = Some(unit);
                None
            } else if (0xDC00..=0xDFFF).contains(&unit) {
                g.pending_high_surrogate.take().and_then(|hi| {
                    let combined = 0x10000 + (((hi as u32 - 0xD800) << 10) | (unit as u32 - 0xDC00));
                    char::from_u32(combined)
                })
            } else {
                g.pending_high_surrogate = None;
                char::from_u32(unit as u32)
            }
        } else {
            g.pending_high_surrogate = None;
            let mut ks = [0u8; 256];
            if unsafe { GetKeyboardState(&mut ks) }.is_ok() {
                let mut utf16 = [0u16; 8];
                let hkl = unsafe { GetKeyboardLayout(0) };
                // Flag 0x4 (Win10 1607+) avoids mutating dead-key state during OnTestKeyDown.
                let rc = unsafe {
                    ToUnicodeEx(
                        vkey as u32,
                        scan_code as u32,
                        &ks,
                        &mut utf16,
                        0x4,
                        Some(hkl),
                    )
                };
                if rc == 1 {
                    char::from_u32(utf16[0] as u32)
                } else if rc == 2 {
                    char::decode_utf16(utf16[..2].iter().copied())
                        .next()
                        .and_then(Result::ok)
                } else {
                    None
                }
            } else {
                None
            }
        };

        KeyEventInput {
            vkey,
            scan_code,
            unicode_char,
            ctrl,
            alt,
            shift,
            win,
            is_injected,
            now_ms: g.start.elapsed().as_millis() as u64,
        }
    }
}

impl ITfTextInputProcessor_Impl for SmartKeyTextService_Impl {
    fn Activate(&self, ptim: Ref<'_, ITfThreadMgr>, tid: u32) -> windows_core::Result<()> {
        self.ActivateEx(ptim, tid, 0)
    }

    fn Deactivate(&self) -> windows_core::Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.unadvise_edit_sink();
            let (tm_opt, tm_cookie, tid, key_advised) = if let Ok(mut g) = self.inner.lock() {
                g.tracker.reset();
                let tm = g.thread_mgr.take();
                let tc = std::mem::take(&mut g.thread_mgr_cookie);
                let tid = std::mem::take(&mut g.tid);
                let ka = std::mem::take(&mut g.key_sink_advised);
                if !g.session.is_null() {
                    unsafe { smartkey_session_free(g.session) };
                    g.session = std::ptr::null_mut();
                }
                if !g.engine.is_null() {
                    unsafe { smartkey_engine_free(g.engine) };
                    g.engine = std::ptr::null_mut();
                }
                g.trace = None;
                (tm, tc, tid, ka)
            } else {
                return;
            };
            self.tid_fast.store(0, Ordering::SeqCst);
            if let Some(tm) = tm_opt {
                if key_advised {
                    if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
                        let _ = unsafe { km.UnadviseKeyEventSink(tid) };
                    }
                }
                if tm_cookie != 0 {
                    if let Ok(src) = tm.cast::<ITfSource>() {
                        let _ = unsafe { src.UnadviseSink(tm_cookie) };
                    }
                }
            }
        }));
        Ok(())
    }
}

impl ITfTextInputProcessorEx_Impl for SmartKeyTextService_Impl {
    fn ActivateEx(
        &self,
        ptim: Ref<'_, ITfThreadMgr>,
        tid: u32,
        _dwflags: u32,
    ) -> windows_core::Result<()> {
        let tm = ptim.ok()?.clone();
        let (immersive, _secure) = is_immersive_or_secure(&tm);
        let (eng, readonly_mode, log_dir_opt) = init_engine_for_process(immersive);
        let sess = smartkey_session_new();
        if !sess.is_null() {
            if let Ok(lang) = CString::new("en") {
                unsafe { smartkey_session_set_lang(sess, lang.as_ptr()) };
            }
        }

        if let Ok(mut g) = self.inner.lock() {
            g.thread_mgr = Some(tm.clone());
            g.tid = tid;
            g.engine = eng;
            g.session = sess;
            g.readonly_mode = readonly_mode;
            g.trace = log_dir_opt.map(TraceLogger::new);
        }
        self.tid_fast.store(tid, Ordering::SeqCst);

        if let Ok(src) = tm.cast::<ITfSource>() {
            let sink: ITfThreadMgrEventSink = self.to_interface();
            if let Ok(cookie) = unsafe { src.AdviseSink(&ITfThreadMgrEventSink::IID, &sink) } {
                if let Ok(mut g) = self.inner.lock() {
                    g.thread_mgr_cookie = cookie;
                }
            }
        }

        if let Ok(km) = tm.cast::<ITfKeystrokeMgr>() {
            let key_sink: ITfKeyEventSink = self.to_interface();
            if unsafe { km.AdviseKeyEventSink(tid, &key_sink, true) }.is_ok() {
                if let Ok(mut g) = self.inner.lock() {
                    g.key_sink_advised = true;
                }
            }
        }

        let focus_doc = unsafe { tm.GetFocus().ok() };
        self.handle_focus_doc(self, focus_doc.as_ref());
        Ok(())
    }
}

impl ITfThreadMgrEventSink_Impl for SmartKeyTextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<'_, ITfDocumentMgr>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _pdim: Ref<'_, ITfDocumentMgr>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnSetFocus(
        &self,
        pdimfocus: Ref<'_, ITfDocumentMgr>,
        _pdimprevfocus: Ref<'_, ITfDocumentMgr>,
    ) -> windows_core::Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            self.handle_focus_doc(self, pdimfocus.as_ref());
        }));
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<'_, ITfContext>) -> windows_core::Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<'_, ITfContext>) -> windows_core::Result<()> {
        Ok(())
    }
}

impl ITfTextEditSink_Impl for SmartKeyTextService_Impl {
    fn OnEndEdit(
        &self,
        pic: Ref<'_, ITfContext>,
        ecreadonly: u32,
        _peditrecord: Ref<'_, ITfEditRecord>,
    ) -> windows_core::Result<()> {
        if self.self_committing.load(Ordering::SeqCst) {
            return Ok(());
        }
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let Ok(ctx) = pic.ok() else {
                return;
            };
            let mut sel = [TF_SELECTION::default()];
            let mut fetched: u32 = 0;
            if unsafe { ctx.GetSelection(ecreadonly, TF_DEFAULT_SELECTION, &mut sel, &mut fetched) }
                .is_err()
                || fetched == 0
            {
                return;
            }
            let range_opt = unsafe { core::mem::ManuallyDrop::take(&mut sel[0].range) };
            let Some(range) = range_opt else {
                return;
            };
            let pre_u16 = read_utf16_before_caret(ecreadonly, &range, 64).unwrap_or_default();
            let post_u16 = read_utf16_after_caret(ecreadonly, &range, 64).unwrap_or_default();
            let pre_str = String::from_utf16_lossy(&pre_u16);
            let post_str = String::from_utf16_lossy(&post_u16);
            let cursor_chars = pre_str.chars().count() as u32;
            let combined = format!("{pre_str}{post_str}");
            let Ok(c_surr) = CString::new(combined.as_bytes()) else {
                return;
            };

            if let Ok(g) = self.inner.lock() {
                if !g.session.is_null() && !g.engine.is_null() && g.current_profile != SMARTKEY_PROFILE_BYPASS {
                    let now_ms = g.start.elapsed().as_millis() as u64;
                    unsafe {
                        smartkey_session_reconcile_surrounding(
                            g.session,
                            g.engine,
                            c_surr.as_ptr(),
                            cursor_chars,
                            now_ms,
                        );
                    }
                }
            }
        }));
        Ok(())
    }
}

impl ITfKeyEventSink_Impl for SmartKeyTextService_Impl {
    fn OnSetFocus(&self, fforeground: BOOL) -> windows_core::Result<()> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            if !fforeground.as_bool() {
                if let Ok(mut g) = self.inner.lock() {
                    g.tracker.reset();
                    if !g.session.is_null() && !g.engine.is_null() {
                        let now_ms = g.start.elapsed().as_millis() as u64;
                        let mut bridge = FfiSessionBridge {
                            session: g.session,
                            engine: g.engine,
                        };
                        let _ = bridge.process_key(SMARTKEY_KEY_FOCUS_LOST, 0, now_ms);
                    }
                }
            } else {
                let doc = self
                    .inner
                    .lock()
                    .ok()
                    .and_then(|g| g.thread_mgr.clone())
                    .and_then(|tm| unsafe { tm.GetFocus().ok() });
                let top = doc.and_then(|d| unsafe { d.GetTop().ok() });
                self.sync_context_classification(top.as_ref());
            }
        }));
        Ok(())
    }

    fn OnTestKeyDown(
        &self,
        _pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows_core::Result<BOOL> {
        let eaten = catch_unwind(AssertUnwindSafe(|| -> bool {
            let Ok(mut g) = self.inner.lock() else {
                return false;
            };
            if g.engine.is_null()
                || g.session.is_null()
                || g.current_profile == SMARTKEY_PROFILE_BYPASS
                || !unsafe { smartkey_engine_is_enabled(g.engine) }
            {
                return false;
            }
            let ev = SmartKeyTextService::decode_wparam_lparam(&mut g, wparam, lparam);
            let session_ptr = g.session;
            let engine_ptr = g.engine;
            let mut bridge = FfiSessionBridge {
                session: session_ptr,
                engine: engine_ptr,
            };
            let result = g.tracker.on_test_key_down(&mut bridge, ev);
            if unsafe { smartkey_engine_is_trace_enabled(engine_ptr) } {
                let snap = bridge.snapshot();
                let s1 = format_snapshot_compact(
                    &snap.mode,
                    &snap.buf,
                    &snap.aux,
                    &snap.prev,
                    snap.trailing_count,
                );
                let ch = ev.unicode_char.map(|c| c.to_string());
                let ctx_id = g.ctx_counter;
                let app = g.current_app.clone();
                if let Some(tr) = g.trace.as_mut() {
                    tr.record_event(
                        ctx_id,
                        &app,
                        "test_key",
                        if result { "eat" } else { "pass" },
                        Some(ev.vkey),
                        ch.as_deref(),
                        None,
                        None,
                        &s1,
                        None,
                    );
                }
            }
            result
        }))
        .unwrap_or(false);
        Ok(BOOL::from(eaten))
    }

    fn OnTestKeyUp(
        &self,
        _pic: Ref<'_, ITfContext>,
        _wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows_core::Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnKeyDown(
        &self,
        pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> windows_core::Result<BOOL> {
        let eaten = catch_unwind(AssertUnwindSafe(|| -> bool {
            let Ok(ctx) = pic.ok() else {
                return false;
            };
            let ctx = ctx.clone();

            let pending_opt = {
                let Ok(mut g) = self.inner.lock() else {
                    return false;
                };
                if g.engine.is_null()
                    || g.session.is_null()
                    || g.current_profile == SMARTKEY_PROFILE_BYPASS
                    || !unsafe { smartkey_engine_is_enabled(g.engine) }
                {
                    return false;
                }
                let ev = SmartKeyTextService::decode_wparam_lparam(&mut g, wparam, lparam);
                let session_ptr = g.session;
                let engine_ptr = g.engine;
                let mut bridge = FfiSessionBridge {
                    session: session_ptr,
                    engine: engine_ptr,
                };
                g.tracker.on_key_down(&mut bridge, ev)
            };

            let Some(pending) = pending_opt else {
                return false;
            };

            let tid = self.tid_fast.load(Ordering::SeqCst);
            if tid == 0 {
                return false;
            }

            let applied = Arc::new(AtomicBool::new(false));
            let session_obj: ITfEditSession = ReplaceEditSession {
                context: ctx.clone(),
                pending: pending.clone(),
                applied: Arc::clone(&applied),
            }
            .into();

            let req_ok = {
                let _self_commit = ScopedSelfCommit::enter(&self.self_committing);
                unsafe { ctx.RequestEditSession(tid, &session_obj, TF_ES_SYNC | TF_ES_READWRITE) }
            };

            let did_apply = req_ok.map(|hr| hr.is_ok()).unwrap_or(false)
                && applied.load(Ordering::SeqCst);

            if !did_apply {
                if let Ok(mut g) = self.inner.lock() {
                    g.tracker.reset();
                    if !g.session.is_null() && !g.engine.is_null() {
                        let now_ms = g.start.elapsed().as_millis() as u64;
                        let mut bridge = FfiSessionBridge {
                            session: g.session,
                            engine: g.engine,
                        };
                        let _ = bridge.process_key(
                            smartkey_ffi::SMARTKEY_KEY_NAV_OR_CLICK,
                            0,
                            now_ms,
                        );
                    }
                    if !g.engine.is_null() && unsafe { smartkey_engine_is_trace_enabled(g.engine) }
                    {
                        let ctx_id = g.ctx_counter;
                        let app = g.current_app.clone();
                        if let Some(tr) = g.trace.as_mut() {
                            tr.record_event(
                                ctx_id,
                                &app,
                                "replace",
                                "aborted",
                                Some(pending.vkey),
                                None,
                                Some(pending.delete_chars),
                                Some(&pending.insert),
                                "aborted",
                                None,
                            );
                        }
                    }
                }
                return false;
            }

            if let Ok(mut g) = self.inner.lock() {
                if !g.engine.is_null() && unsafe { smartkey_engine_is_trace_enabled(g.engine) } {
                    let ctx_id = g.ctx_counter;
                    let app = g.current_app.clone();
                    if let Some(tr) = g.trace.as_mut() {
                        tr.record_event(
                            ctx_id,
                            &app,
                            "replace",
                            "set_text",
                            Some(pending.vkey),
                            None,
                            Some(pending.delete_chars),
                            Some(&pending.insert),
                            "replaced",
                            None,
                        );
                    }
                }
            }
            true
        }))
        .unwrap_or(false);

        Ok(BOOL::from(eaten))
    }

    fn OnKeyUp(
        &self,
        _pic: Ref<'_, ITfContext>,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> windows_core::Result<BOOL> {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let vkey = (wparam.0 & 0xFFFF) as u16;
            let ctrl = unsafe { (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0 };
            let shift = unsafe { (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 };
            let alt = unsafe { (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0 };
            let win = unsafe {
                (GetKeyState(VK_LWIN as i32) as u16 & 0x8000) != 0
                    || (GetKeyState(VK_RWIN as i32) as u16 & 0x8000) != 0
            };
            if let Ok(mut g) = self.inner.lock() {
                g.tracker.on_key_up(vkey, ctrl, shift, alt, win);
            }
        }));
        Ok(BOOL(0))
    }

    fn OnPreservedKey(
        &self,
        _pic: Ref<'_, ITfContext>,
        _rguid: *const GUID,
    ) -> windows_core::Result<BOOL> {
        Ok(BOOL(0))
    }
}
