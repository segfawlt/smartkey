use smartkey_ffi::{
    smartkey_engine_classify_app, smartkey_engine_is_english_word, smartkey_engine_is_vi_dict_word,
    smartkey_session_debug_snapshot, smartkey_session_process_key, SmartKeyAction,
    SmartKeyEngineHandle, SmartKeySessionHandle, SmartKeySessionSnapshot, SMARTKEY_ACTION_REPLACE,
    SMARTKEY_KEY_BACKSPACE, SMARTKEY_KEY_CHAR, SMARTKEY_KEY_CTRL_BACKSPACE, SMARTKEY_KEY_ENTER,
    SMARTKEY_KEY_NAV_OR_CLICK, SMARTKEY_KEY_TAB, SMARTKEY_PROFILE_BYPASS, SMARTKEY_PROFILE_PROSE,
};
use std::ffi::{CStr, CString};

// Win32 virtual key constants (mirroring Win32_UI_Input_KeyboardAndMouse so tests run on Linux).
pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_CAPITAL: u16 = 0x14;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_NEXT: u16 = 0x22;
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_APPS: u16 = 0x5D;
pub const VK_NUMLOCK: u16 = 0x90;
pub const VK_SCROLL: u16 = 0x91;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RSHIFT: u16 = 0xA1;
pub const VK_LCONTROL: u16 = 0xA2;
pub const VK_RCONTROL: u16 = 0xA3;
pub const VK_LMENU: u16 = 0xA4;
pub const VK_RMENU: u16 = 0xA5;
pub const VK_PACKET: u16 = 0xE7;

// InputScope integer discriminants (matching Win32::UI::TextServices::InputScope).
pub const IS_URL: i32 = 1;
pub const IS_EMAIL_USERNAME: i32 = 4;
pub const IS_EMAIL_SMTPEMAILADDRESS: i32 = 5;
pub const IS_CURRENCY_AMOUNTANDSYMBOL: i32 = 20;
pub const IS_CURRENCY_AMOUNT: i32 = 21;
pub const IS_DIGITS: i32 = 28;
pub const IS_NUMBER: i32 = 29;
pub const IS_PASSWORD: i32 = 31;
pub const IS_TELEPHONE_FULLTELEPHONENUMBER: i32 = 32;
pub const IS_TELEPHONE_COUNTRYCODE: i32 = 33;
pub const IS_TELEPHONE_AREACODE: i32 = 34;
pub const IS_TELEPHONE_LOCALNUMBER: i32 = 35;
pub const IS_LOGINNAME: i32 = 60;
pub const IS_PRIVATE: i32 = 61;
pub const IS_NUMERIC_PASSWORD: i32 = 63;
pub const IS_NUMERIC_PIN: i32 = 64;
pub const IS_ALPHANUMERIC_PIN: i32 = 65;
pub const IS_ALPHANUMERIC_PIN_SET: i32 = 66;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEventInput {
    pub vkey: u16,
    pub scan_code: u16,
    pub unicode_char: Option<char>,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub is_injected: bool,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReplace {
    pub vkey: u16,
    pub delete_chars: u32,
    pub insert: String,
    pub expected_suffix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedKey {
    IgnoreModifier,
    InjectedOrNonAscii,
    ShortcutReset,
    EngineKey { key_kind: u8, codepoint: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineAction {
    PassThrough,
    Replace {
        delete_chars: u32,
        insert: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SnapshotInfo {
    pub mode: String,
    pub buf: String,
    pub aux: String,
    pub prev: String,
    pub trailing_count: u32,
}

pub trait SessionBridge {
    fn snapshot(&self) -> SnapshotInfo;
    fn process_key(&mut self, key_kind: u8, codepoint: u32, now_ms: u64) -> EngineAction;
    fn is_english_word(&self, word: &str) -> bool;
    fn is_vi_dict_word(&self, word: &str) -> bool;
}

pub struct FfiSessionBridge {
    pub session: *mut SmartKeySessionHandle,
    pub engine: *mut SmartKeyEngineHandle,
}

impl SessionBridge for FfiSessionBridge {
    fn snapshot(&self) -> SnapshotInfo {
        let mut snap = SmartKeySessionSnapshot::default();
        if self.session.is_null()
            || !unsafe { smartkey_session_debug_snapshot(self.session, &mut snap) }
        {
            return SnapshotInfo::default();
        }
        let slice_str = |ptr: *const std::ffi::c_char, len: u32| -> String {
            if ptr.is_null() || len == 0 {
                return String::new();
            }
            let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
            String::from_utf8_lossy(bytes).into_owned()
        };
        let mode = if snap.mode_utf8.is_null() {
            "Idle".to_string()
        } else {
            unsafe { CStr::from_ptr(snap.mode_utf8) }
                .to_string_lossy()
                .into_owned()
        };
        SnapshotInfo {
            mode,
            buf: slice_str(snap.buf_utf8, snap.buf_byte_len),
            aux: slice_str(snap.aux_utf8, snap.aux_byte_len),
            prev: slice_str(snap.prev_utf8, snap.prev_byte_len),
            trailing_count: snap.trailing_count,
        }
    }

    fn process_key(&mut self, key_kind: u8, codepoint: u32, now_ms: u64) -> EngineAction {
        if self.session.is_null() || self.engine.is_null() {
            return EngineAction::PassThrough;
        }
        let mut act = SmartKeyAction::default();
        let ok = unsafe {
            smartkey_session_process_key(
                self.session,
                self.engine,
                key_kind,
                codepoint,
                now_ms,
                &mut act,
            )
        };
        if !ok || act.kind != SMARTKEY_ACTION_REPLACE {
            return EngineAction::PassThrough;
        }
        let insert = if !act.insert_utf8.is_null() && act.insert_byte_len > 0 {
            let bytes = unsafe {
                std::slice::from_raw_parts(act.insert_utf8 as *const u8, act.insert_byte_len as usize)
            };
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            String::new()
        };
        EngineAction::Replace {
            delete_chars: act.delete_chars,
            insert,
        }
    }

    fn is_english_word(&self, word: &str) -> bool {
        if self.engine.is_null() || word.is_empty() {
            return false;
        }
        let Ok(c_word) = CString::new(word) else {
            return false;
        };
        unsafe { smartkey_engine_is_english_word(self.engine, c_word.as_ptr(), std::ptr::null_mut()) }
    }

    fn is_vi_dict_word(&self, word: &str) -> bool {
        if self.engine.is_null() || word.is_empty() {
            return false;
        }
        let Ok(c_word) = CString::new(word) else {
            return false;
        };
        unsafe { smartkey_engine_is_vi_dict_word(self.engine, c_word.as_ptr()) }
    }
}

pub fn is_modifier_vkey(vkey: u16) -> bool {
    matches!(
        vkey,
        VK_SHIFT
            | VK_LSHIFT
            | VK_RSHIFT
            | VK_CONTROL
            | VK_LCONTROL
            | VK_RCONTROL
            | VK_MENU
            | VK_LMENU
            | VK_RMENU
            | VK_LWIN
            | VK_RWIN
            | VK_APPS
            | VK_CAPITAL
            | VK_NUMLOCK
            | VK_SCROLL
    )
}

pub fn is_ctrl_vkey(vkey: u16) -> bool {
    matches!(vkey, VK_CONTROL | VK_LCONTROL | VK_RCONTROL)
}

pub fn is_shift_vkey(vkey: u16) -> bool {
    matches!(vkey, VK_SHIFT | VK_LSHIFT | VK_RSHIFT)
}

pub fn is_boundary_char(ch: char) -> bool {
    matches!(
        ch,
        ' ' | '\t'
            | ','
            | '.'
            | '!'
            | '?'
            | ';'
            | ':'
            | ')'
            | ']'
            | '}'
            | '"'
            | '('
            | '['
            | '{'
            | '`'
            | '\u{201C}'
            | '\u{201D}'
    )
}

pub fn classify_tsf_context<F>(
    secure_mode: bool,
    input_scopes: &[i32],
    classify_app: F,
) -> (u8, &'static str)
where
    F: FnOnce() -> (u8, &'static str),
{
    if secure_mode {
        return (SMARTKEY_PROFILE_BYPASS, "capability:secure_mode");
    }
    for &scope in input_scopes {
        match scope {
            IS_PASSWORD
            | IS_PRIVATE
            | IS_NUMERIC_PASSWORD
            | IS_NUMERIC_PIN
            | IS_ALPHANUMERIC_PIN
            | IS_ALPHANUMERIC_PIN_SET => {
                return (SMARTKEY_PROFILE_BYPASS, "capability:password");
            }
            IS_URL => {
                return (SMARTKEY_PROFILE_BYPASS, "capability:url");
            }
            IS_EMAIL_USERNAME | IS_EMAIL_SMTPEMAILADDRESS | IS_LOGINNAME => {
                return (SMARTKEY_PROFILE_BYPASS, "capability:email");
            }
            IS_DIGITS
            | IS_NUMBER
            | IS_CURRENCY_AMOUNTANDSYMBOL
            | IS_CURRENCY_AMOUNT
            | IS_TELEPHONE_FULLTELEPHONENUMBER
            | IS_TELEPHONE_COUNTRYCODE
            | IS_TELEPHONE_AREACODE
            | IS_TELEPHONE_LOCALNUMBER => {
                return (SMARTKEY_PROFILE_BYPASS, "capability:numeric_or_disabled");
            }
            _ => {}
        }
    }
    classify_app()
}

/// # Safety
///
/// `engine` must be either null or a valid, live pointer created by `smartkey_engine_new*`.
pub unsafe fn classify_app_via_ffi(
    engine: *const SmartKeyEngineHandle,
    app_utf8: &str,
) -> (u8, &'static str) {
    if engine.is_null() {
        return (SMARTKEY_PROFILE_PROSE, "prose");
    }
    let Ok(c_app) = CString::new(app_utf8) else {
        return (SMARTKEY_PROFILE_PROSE, "prose");
    };
    let mut reason_ptr: *const std::ffi::c_char = std::ptr::null();
    let prof = smartkey_engine_classify_app(engine, c_app.as_ptr(), &mut reason_ptr);
    let reason = if reason_ptr.is_null() {
        "prose"
    } else {
        CStr::from_ptr(reason_ptr).to_str().unwrap_or("prose")
    };
    let static_reason = match reason {
        "user-app-override" => "user-app-override",
        "unknown-app-policy" => "unknown-app-policy",
        "program-list" => "program-list",
        "browser" => "browser",
        "chat" => "chat",
        _ => "prose",
    };
    (prof, static_reason)
}

pub fn decode_key_input(ev: &KeyEventInput) -> DecodedKey {
    if ev.is_injected
        || ev.vkey == VK_PACKET
        || ev
            .unicode_char
            .is_some_and(|ch| !ch.is_ascii() && !is_boundary_char(ch))
    {
        return DecodedKey::InjectedOrNonAscii;
    }

    if is_modifier_vkey(ev.vkey) {
        return DecodedKey::IgnoreModifier;
    }

    if ev.vkey == VK_BACK && ev.ctrl && !ev.alt && !ev.win {
        return DecodedKey::EngineKey {
            key_kind: SMARTKEY_KEY_CTRL_BACKSPACE,
            codepoint: 0,
        };
    }

    if ev.ctrl || ev.alt || ev.win {
        return DecodedKey::ShortcutReset;
    }

    match ev.vkey {
        VK_BACK => DecodedKey::EngineKey {
            key_kind: SMARTKEY_KEY_BACKSPACE,
            codepoint: 0,
        },
        VK_RETURN => DecodedKey::EngineKey {
            key_kind: SMARTKEY_KEY_ENTER,
            codepoint: 0,
        },
        VK_TAB => DecodedKey::EngineKey {
            key_kind: SMARTKEY_KEY_TAB,
            codepoint: 0,
        },
        VK_ESCAPE | VK_PRIOR | VK_NEXT | VK_END | VK_HOME | VK_LEFT | VK_UP | VK_RIGHT
        | VK_DOWN | VK_INSERT | VK_DELETE => DecodedKey::EngineKey {
            key_kind: SMARTKEY_KEY_NAV_OR_CLICK,
            codepoint: 0,
        },
        _ => {
            if let Some(ch) = ev.unicode_char {
                let cp = ch as u32;
                if cp >= 0x20 && cp != 0x7F {
                    return DecodedKey::EngineKey {
                        key_kind: SMARTKEY_KEY_CHAR,
                        codepoint: cp,
                    };
                }
            }
            DecodedKey::IgnoreModifier
        }
    }
}

/// Validates the pre-caret UTF-16 buffer read from TSF against `delete_chars` and
/// optional `expected_suffix`, returning the exact UTF-16 `WCHAR` count to replace.
pub fn verify_pre_cursor_utf16(
    pre_utf16: &[u16],
    delete_chars: u32,
    expected_suffix: Option<&str>,
) -> Option<usize> {
    if delete_chars == 0 {
        return Some(0);
    }
    let need = delete_chars as usize;
    let mut scalars: Vec<char> = Vec::with_capacity(pre_utf16.len());
    for decoded in char::decode_utf16(pre_utf16.iter().copied()) {
        let ch = decoded.ok()?;
        scalars.push(ch);
    }
    if scalars.len() < need {
        return None;
    }
    let tail = &scalars[scalars.len() - need..];
    if tail.iter().any(|&c| matches!(c, '\r' | '\n' | '\t')) {
        return None;
    }
    if let Some(expected) = expected_suffix {
        if !expected.is_empty() {
            let expected_chars: Vec<char> = expected.chars().collect();
            if tail != expected_chars.as_slice() {
                return None;
            }
        }
    }
    let utf16_len: usize = tail.iter().map(|c| c.len_utf16()).sum();
    Some(utf16_len)
}

#[derive(Debug, Default)]
pub struct TsfKeyTracker {
    vi_mode_likely: bool,
    en_only_word_streak: u8,
    word_tainted: bool,
    current_word: String,
    pending_replace: Option<PendingReplace>,
    ctrl_shift_chord_clean: bool,
    last_backspace_ms: Option<u64>,
}

impl TsfKeyTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn vi_mode_likely(&self) -> bool {
        self.vi_mode_likely
    }

    pub fn word_tainted(&self) -> bool {
        self.word_tainted
    }

    pub fn en_only_word_streak(&self) -> u8 {
        self.en_only_word_streak
    }

    pub fn reset(&mut self) {
        self.word_tainted = false;
        self.current_word.clear();
        self.pending_replace = None;
        self.ctrl_shift_chord_clean = false;
        self.last_backspace_ms = None;
    }

    pub fn on_key_up(&mut self, vkey: u16, ctrl: bool, shift: bool, alt: bool, win: bool) {
        if self.ctrl_shift_chord_clean && (is_ctrl_vkey(vkey) || is_shift_vkey(vkey)) {
            if !alt && !win && (ctrl || shift) {
                self.vi_mode_likely = false;
                self.en_only_word_streak = 0;
            }
            self.ctrl_shift_chord_clean = false;
        }
    }

    pub fn on_test_key_down<B: SessionBridge>(
        &mut self,
        bridge: &mut B,
        ev: KeyEventInput,
    ) -> bool {
        self.pending_replace = None;
        if let Some(rep) = self.evaluate_key_down(bridge, ev) {
            self.pending_replace = Some(rep);
            true
        } else {
            false
        }
    }

    pub fn on_key_down<B: SessionBridge>(
        &mut self,
        bridge: &mut B,
        ev: KeyEventInput,
    ) -> Option<PendingReplace> {
        if let Some(stashed) = self.pending_replace.take() {
            if stashed.vkey == ev.vkey {
                return Some(stashed);
            }
        }
        self.evaluate_key_down(bridge, ev)
    }

    fn clear_vi_mode(&mut self) {
        self.vi_mode_likely = false;
        self.en_only_word_streak = 0;
    }

    fn note_word_boundary(&mut self, bridge: &mut impl SessionBridge) -> bool {
        let word = std::mem::take(&mut self.current_word);
        let tainted = self.word_tainted;
        self.word_tainted = false;
        if tainted {
            let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, 0);
            return false;
        }
        if word.is_empty() {
            return true;
        }
        let is_vi = bridge.is_vi_dict_word(&word);
        if self.vi_mode_likely && is_vi {
            self.en_only_word_streak = 0;
            let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, 0);
            return false;
        }
        if self.vi_mode_likely
            && word.chars().count() >= 3
            && bridge.is_english_word(&word)
            && !is_vi
        {
            self.en_only_word_streak = self.en_only_word_streak.saturating_add(1);
            if self.en_only_word_streak >= 2 {
                self.clear_vi_mode();
            }
        } else if !is_vi && word.chars().count() >= 2 {
            self.en_only_word_streak = 0;
        }
        true
    }

    fn evaluate_key_down<B: SessionBridge>(
        &mut self,
        bridge: &mut B,
        ev: KeyEventInput,
    ) -> Option<PendingReplace> {
        if is_modifier_vkey(ev.vkey) {
            if (is_ctrl_vkey(ev.vkey) || is_shift_vkey(ev.vkey))
                && ev.ctrl
                && ev.shift
                && !ev.alt
                && !ev.win
            {
                self.ctrl_shift_chord_clean = true;
            } else if !is_ctrl_vkey(ev.vkey) && !is_shift_vkey(ev.vkey) {
                self.ctrl_shift_chord_clean = false;
            }
            return None;
        }
        self.ctrl_shift_chord_clean = false;

        // Alt+Z is UniKey/OpenKey's default V/E toggle shortcut.
        if ev.alt && !ev.ctrl && !ev.win && (ev.vkey == b'Z' as u16) {
            self.clear_vi_mode();
            self.word_tainted = false;
            self.current_word.clear();
            let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, ev.now_ms);
            return None;
        }

        match decode_key_input(&ev) {
            DecodedKey::IgnoreModifier => None,
            DecodedKey::InjectedOrNonAscii => {
                self.vi_mode_likely = true;
                self.en_only_word_streak = 0;
                self.current_word.clear();
                self.word_tainted = !(ev.vkey == VK_PACKET
                    && ev.unicode_char.is_some_and(is_boundary_char));
                let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, ev.now_ms);
                None
            }
            DecodedKey::ShortcutReset => {
                self.word_tainted = false;
                self.current_word.clear();
                let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, ev.now_ms);
                None
            }
            DecodedKey::EngineKey {
                key_kind,
                codepoint,
            } => {
                if key_kind == SMARTKEY_KEY_BACKSPACE {
                    if let Some(prev_bs) = self.last_backspace_ms {
                        if ev.now_ms.saturating_sub(prev_bs) < 12 && !self.current_word.is_empty() {
                            // Synthetic rapid backspace burst from SendInput without dwExtraInfo.
                            self.vi_mode_likely = true;
                            self.en_only_word_streak = 0;
                            self.word_tainted = true;
                            self.current_word.clear();
                            self.last_backspace_ms = Some(ev.now_ms);
                            let _ = bridge.process_key(SMARTKEY_KEY_NAV_OR_CLICK, 0, ev.now_ms);
                            return None;
                        }
                    }
                    self.last_backspace_ms = Some(ev.now_ms);
                } else {
                    self.last_backspace_ms = None;
                }

                if key_kind == SMARTKEY_KEY_CHAR {
                    if let Some(ch) = char::from_u32(codepoint) {
                        if is_boundary_char(ch) {
                            if !self.note_word_boundary(bridge) {
                                return None;
                            }
                        } else if self.word_tainted {
                            return None;
                        } else {
                            self.current_word.push(ch);
                        }
                    }
                } else if key_kind == SMARTKEY_KEY_BACKSPACE {
                    if self.word_tainted {
                        self.current_word.pop();
                        return None;
                    }
                    self.current_word.pop();
                } else if matches!(
                    key_kind,
                    SMARTKEY_KEY_ENTER | SMARTKEY_KEY_TAB | SMARTKEY_KEY_NAV_OR_CLICK
                ) {
                    if matches!(key_kind, SMARTKEY_KEY_ENTER | SMARTKEY_KEY_TAB) {
                        if !self.note_word_boundary(bridge) {
                            return None;
                        }
                    } else {
                        self.word_tainted = false;
                        self.current_word.clear();
                    }
                } else if key_kind == SMARTKEY_KEY_CTRL_BACKSPACE {
                    self.word_tainted = false;
                    self.current_word.clear();
                }

                let snap_before = bridge.snapshot();
                match bridge.process_key(key_kind, codepoint, ev.now_ms) {
                    EngineAction::PassThrough => None,
                    EngineAction::Replace {
                        delete_chars,
                        insert,
                    } => {
                        let buf_chars = snap_before.buf.chars().count();
                        let expected_suffix = if (key_kind == SMARTKEY_KEY_CHAR
                            && matches!(
                                snap_before.mode.as_str(),
                                "Composing" | "EditingRestored" | "EditingPrevious"
                            )
                            && delete_chars as usize == buf_chars)
                            || (key_kind == SMARTKEY_KEY_BACKSPACE
                                && snap_before.mode == "JustAutocorrected"
                                && snap_before.trailing_count == 0
                                && delete_chars as usize == buf_chars)
                        {
                            Some(snap_before.buf)
                        } else {
                            None
                        };
                        self.current_word.clear();
                        self.word_tainted = false;
                        Some(PendingReplace {
                            vkey: ev.vkey,
                            delete_chars,
                            insert,
                            expected_suffix,
                        })
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smartkey_ffi::{
        smartkey_engine_free, smartkey_engine_new, smartkey_session_free, smartkey_session_new,
        SMARTKEY_PRESET_BALANCED, SMARTKEY_PROFILE_BROWSER,
    };

    fn ev_char(ch: char, now_ms: u64) -> KeyEventInput {
        KeyEventInput {
            vkey: ch.to_ascii_uppercase() as u16,
            scan_code: 0,
            unicode_char: Some(ch),
            ctrl: false,
            alt: false,
            shift: ch.is_ascii_uppercase(),
            win: false,
            is_injected: false,
            now_ms,
        }
    }

    fn ev_key(vkey: u16, now_ms: u64) -> KeyEventInput {
        KeyEventInput {
            vkey,
            scan_code: 0,
            unicode_char: None,
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            is_injected: false,
            now_ms,
        }
    }

    fn with_ffi_bridge(f: impl FnOnce(&mut FfiSessionBridge)) {
        let eng = unsafe {
            smartkey_engine_new(std::ptr::null(), std::ptr::null(), SMARTKEY_PRESET_BALANCED)
        };
        assert!(!eng.is_null());
        let sess = smartkey_session_new();
        assert!(!sess.is_null());
        let mut bridge = FfiSessionBridge {
            session: sess,
            engine: eng,
        };
        f(&mut bridge);
        unsafe {
            smartkey_session_free(sess);
            smartkey_engine_free(eng);
        }
    }

    #[test]
    fn test_key_down_stashes_action_and_falls_back_when_test_skipped() {
        with_ffi_bridge(|bridge| {
            let mut tracker = TsfKeyTracker::new();
            for (idx, ch) in ['t', 'e', 'h'].into_iter().enumerate() {
                let e = ev_char(ch, 100 + idx as u64 * 20);
                assert!(!tracker.on_test_key_down(bridge, e));
            }
            let space = ev_char(' ', 180);
            assert!(
                tracker.on_test_key_down(bridge, space),
                "OnTestKeyDown must eat boundary space when correction fires"
            );
            let act = tracker
                .on_key_down(bridge, space)
                .expect("OnKeyDown must pop stashed Replace once without double-eval");
            assert_eq!(act.delete_chars, 3);
            assert_eq!(act.insert, "the ");
            assert_eq!(act.expected_suffix.as_deref(), Some("teh"));

            // Stepwise undo: 1st BS deletes space (PassThrough), 2nd BS restores "teh" via legacy OnKeyDown directly.
            let bs1 = ev_key(VK_BACK, 240);
            assert!(!tracker.on_test_key_down(bridge, bs1));
            let bs2 = ev_key(VK_BACK, 300);
            let undo = tracker
                .on_key_down(bridge, bs2)
                .expect("OnKeyDown without prior OnTestKeyDown must still evaluate");
            assert_eq!(undo.delete_chars, 3);
            assert_eq!(undo.insert, "teh");
            assert_eq!(undo.expected_suffix.as_deref(), Some("the"));
        });
    }

    #[test]
    fn vi_mode_likely_shields_unaccented_vietnamese_and_recovers() {
        with_ffi_bridge(|bridge| {
            let mut tracker = TsfKeyTracker::new();

            // Simulate UniKey injecting a Vietnamese accented word via VK_PACKET.
            let packet = KeyEventInput {
                vkey: VK_PACKET,
                scan_code: 'ệ' as u16,
                unicode_char: Some('ệ'),
                ctrl: false,
                alt: false,
                shift: false,
                win: false,
                is_injected: true,
                now_ms: 100,
            };
            assert!(!tracker.on_test_key_down(bridge, packet));
            assert!(tracker.vi_mode_likely());
            assert!(tracker.word_tainted());
            assert!(!tracker.on_test_key_down(bridge, ev_char(' ', 120)));
            assert!(!tracker.word_tainted());

            // Type unaccented Vietnamese "anh " while vi_mode_likely is true -> MUST NOT turn into "an ".
            for (idx, ch) in ['a', 'n', 'h'].into_iter().enumerate() {
                assert!(!tracker.on_test_key_down(bridge, ev_char(ch, 200 + idx as u64 * 20)));
            }
            assert!(
                !tracker.on_test_key_down(bridge, ev_char(' ', 280)),
                "valid Vietnamese syllable 'anh' must stay untouched in vi_mode_likely"
            );

            // Real English typos ("teh ") still fix even while vi_mode_likely is active!
            for (idx, ch) in ['t', 'e', 'h'].into_iter().enumerate() {
                assert!(!tracker.on_test_key_down(bridge, ev_char(ch, 400 + idx as u64 * 20)));
            }
            assert!(tracker.on_test_key_down(bridge, ev_char(' ', 480)));
            let fixed = tracker.on_key_down(bridge, ev_char(' ', 480)).unwrap();
            assert_eq!(fixed.insert, "the ");

            // Typing 2 valid English-only words clears vi_mode_likely.
            for word in ["world", "system"] {
                for ch in word.chars() {
                    assert!(!tracker.on_test_key_down(bridge, ev_char(ch, 600)));
                }
                assert!(!tracker.on_test_key_down(bridge, ev_char(' ', 620)));
            }
            assert!(!tracker.vi_mode_likely());

            // Re-arm vi_mode_likely, then clear immediately via Ctrl+Shift chord release.
            assert!(!tracker.on_test_key_down(bridge, packet));
            assert!(tracker.vi_mode_likely());
            assert!(!tracker.on_test_key_down(
                bridge,
                KeyEventInput {
                    vkey: VK_LCONTROL,
                    scan_code: 0,
                    unicode_char: None,
                    ctrl: true,
                    alt: false,
                    shift: false,
                    win: false,
                    is_injected: false,
                    now_ms: 800,
                }
            ));
            assert!(!tracker.on_test_key_down(
                bridge,
                KeyEventInput {
                    vkey: VK_LSHIFT,
                    scan_code: 0,
                    unicode_char: None,
                    ctrl: true,
                    alt: false,
                    shift: true,
                    win: false,
                    is_injected: false,
                    now_ms: 810,
                }
            ));
            tracker.on_key_up(VK_LSHIFT, true, true, false, false);
            assert!(!tracker.vi_mode_likely());
        });
    }

    #[test]
    fn verify_pre_cursor_utf16_checks_exact_suffix_delimiters_and_surrogates() {
        let teh: Vec<u16> = "hello teh".encode_utf16().collect();
        assert_eq!(verify_pre_cursor_utf16(&teh, 3, Some("teh")), Some(3));
        assert_eq!(verify_pre_cursor_utf16(&teh, 3, Some("foo")), None);
        assert_eq!(verify_pre_cursor_utf16(&teh, 20, Some("teh")), None);

        let newline_before: Vec<u16> = "a\nb".encode_utf16().collect();
        assert_eq!(verify_pre_cursor_utf16(&newline_before, 2, None), None);

        // Astral emoji before cursor counts as 1 Unicode scalar but 2 UTF-16 code units.
        let astral: Vec<u16> = "a😀b".encode_utf16().collect();
        assert_eq!(verify_pre_cursor_utf16(&astral, 2, Some("😀b")), Some(3));

        // Lone high surrogate must be rejected cleanly.
        assert_eq!(verify_pre_cursor_utf16(&[0xD83D, b'a' as u16], 1, None), None);
    }

    #[test]
    fn input_scope_and_secure_mode_precedence() {
        assert_eq!(
            classify_tsf_context(true, &[], || (SMARTKEY_PROFILE_BROWSER, "browser")),
            (SMARTKEY_PROFILE_BYPASS, "capability:secure_mode")
        );
        assert_eq!(
            classify_tsf_context(false, &[IS_PRIVATE], || (SMARTKEY_PROFILE_PROSE, "prose")),
            (SMARTKEY_PROFILE_BYPASS, "capability:password")
        );
        assert_eq!(
            classify_tsf_context(false, &[IS_NUMERIC_PIN], || (SMARTKEY_PROFILE_PROSE, "prose")),
            (SMARTKEY_PROFILE_BYPASS, "capability:password")
        );
        assert_eq!(
            classify_tsf_context(false, &[IS_URL], || (SMARTKEY_PROFILE_PROSE, "prose")),
            (SMARTKEY_PROFILE_BYPASS, "capability:url")
        );
        assert_eq!(
            classify_tsf_context(false, &[IS_EMAIL_USERNAME], || (SMARTKEY_PROFILE_PROSE, "prose")),
            (SMARTKEY_PROFILE_BYPASS, "capability:email")
        );
        assert_eq!(
            classify_tsf_context(false, &[IS_DIGITS], || (SMARTKEY_PROFILE_PROSE, "prose")),
            (SMARTKEY_PROFILE_BYPASS, "capability:numeric_or_disabled")
        );
        assert_eq!(
            classify_tsf_context(false, &[], || (SMARTKEY_PROFILE_BROWSER, "browser")),
            (SMARTKEY_PROFILE_BROWSER, "browser")
        );
    }
}
