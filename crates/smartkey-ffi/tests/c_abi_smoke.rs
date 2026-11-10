use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;

use smartkey_ffi::*;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

#[test]
fn ffi_null_and_keystroke_roundtrip_safe() {
    unsafe {
        // Null-pointer resilience
        let mut act = SmartKeyAction::default();
        assert!(!smartkey_session_process_key(
            ptr::null_mut(),
            ptr::null_mut(),
            SMARTKEY_KEY_CHAR,
            b'a' as u32,
            0,
            &mut act
        ));
        smartkey_engine_free(ptr::null_mut());
        smartkey_session_free(ptr::null_mut());

        let eng = smartkey_engine_new(ptr::null(), ptr::null(), SMARTKEY_PRESET_BALANCED);
        assert!(!eng.is_null());

        // Non-NULL empty or invalid UTF-8 paths must fail (`NULL`), never silently degrade to in-memory
        let empty_path = CString::new("").unwrap();
        assert!(
            smartkey_engine_new(empty_path.as_ptr(), ptr::null(), SMARTKEY_PRESET_BALANCED)
                .is_null()
        );
        let bad_utf8: [u8; 3] = [0xff, 0xfe, 0x00];
        assert!(smartkey_engine_new(
            bad_utf8.as_ptr() as *const _,
            ptr::null(),
            SMARTKEY_PRESET_BALANCED
        )
        .is_null());

        let sess = smartkey_session_new();
        assert!(!sess.is_null());

        // Bilingual frequency query (`"is"` is a very high-frequency English word, `"khum"` is not)
        let is_c = CString::new("is").unwrap();
        let khum_c = CString::new("khum").unwrap();
        let mut freq: u8 = 0;
        assert!(smartkey_engine_is_english_word(
            eng,
            is_c.as_ptr(),
            &mut freq
        ));
        assert!(freq >= 180, "expected high freq for 'is', got {freq}");
        assert!(!smartkey_engine_is_english_word(
            eng,
            khum_c.as_ptr(),
            &mut freq
        ));

        // Type `"chagnes "` -> Replace(7, "changes ")
        for ch in "chagnes".chars() {
            assert!(smartkey_session_process_key(
                sess,
                eng,
                SMARTKEY_KEY_CHAR,
                ch as u32,
                10,
                &mut act
            ));
            assert_eq!(act.kind, SMARTKEY_ACTION_PASS_THROUGH);
        }
        assert!(smartkey_session_process_key(
            sess,
            eng,
            SMARTKEY_KEY_CHAR,
            b' ' as u32,
            20,
            &mut act
        ));
        assert_eq!(act.kind, SMARTKEY_ACTION_REPLACE);
        assert_eq!(act.delete_chars, 7);
        let inserted = CStr::from_ptr(act.insert_utf8).to_str().unwrap();
        assert_eq!(inserted, "changes ");

        // `1x BackSpace` -> Replace(8, "chagnes")
        assert!(smartkey_session_process_key(
            sess,
            eng,
            SMARTKEY_KEY_BACKSPACE,
            0,
            30,
            &mut act
        ));
        assert_eq!(act.kind, SMARTKEY_ACTION_REPLACE);
        assert_eq!(act.delete_chars, 8);
        let restored = CStr::from_ptr(act.insert_utf8).to_str().unwrap();
        assert_eq!(restored, "chagnes");

        // Mobile hooks + pair block query
        let fomr = CString::new("fomr").unwrap();
        let from = CString::new("from").unwrap();
        let form = CString::new("form").unwrap();
        assert!(!smartkey_engine_is_pair_blocked(
            eng,
            fomr.as_ptr(),
            from.as_ptr()
        ));
        smartkey_mobile_on_auto_commit(sess, fomr.as_ptr(), from.as_ptr());
        smartkey_mobile_on_manual_fix(sess, eng, fomr.as_ptr(), form.as_ptr());
        assert!(smartkey_engine_is_pair_blocked(
            eng,
            fomr.as_ptr(),
            from.as_ptr()
        ));

        smartkey_session_free(sess);
        smartkey_engine_free(eng);
    }
}

#[test]
fn cpp17_links_smartkey_header_and_staticlib_cleanly() {
    let root = workspace_root();
    let header_dir = root.join("crates/smartkey-ffi/include");

    // Ensure `libsmartkey_ffi.a` is built in debug target dir
    let status = Command::new(env!("CARGO"))
        .args(["build", "-p", "smartkey-ffi"])
        .current_dir(&root)
        .status()
        .expect("invoke cargo build -p smartkey-ffi");
    assert!(status.success(), "cargo build -p smartkey-ffi failed");

    let staticlib = root.join("target/debug/libsmartkey_ffi.a");
    assert!(
        staticlib.exists(),
        "expected staticlib at {}",
        staticlib.display()
    );

    let tmp_dir = std::env::temp_dir().join(format!("smartkey_cpp_smoke_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();

    let cpp_src = tmp_dir.join("smoke.cpp");
    let cpp_bin = tmp_dir.join("smoke_bin");
    fs::write(
        &cpp_src,
        r#"#include "smartkey.h"
#include <cassert>
#include <cstring>
#include <string>

static void apply(std::u32string& buf, uint8_t key_kind, uint32_t cp, const SmartKeyAction& act) {
    if (act.kind == SMARTKEY_ACTION_REPLACE) {
        assert(act.delete_chars <= buf.size());
        buf.erase(buf.size() - act.delete_chars);
        std::string ins(act.insert_utf8, act.insert_byte_len);
        for (unsigned char c : ins) {
            buf.push_back(static_cast<char32_t>(c));
        }
    } else if (key_kind == SMARTKEY_KEY_CHAR) {
        buf.push_back(static_cast<char32_t>(cp));
    } else if (key_kind == SMARTKEY_KEY_BACKSPACE && !buf.empty()) {
        buf.pop_back();
    }
}

int main() {
    SmartKeyEngineHandle* eng = smartkey_engine_new(nullptr, nullptr, SMARTKEY_PRESET_BALANCED);
    assert(eng != nullptr);
    SmartKeySessionHandle* sess = smartkey_session_new();
    assert(sess != nullptr);

    std::u32string buf;
    SmartKeyAction act{};

    const char* text = "teh ";
    for (const char* p = text; *p; ++p) {
        bool ok = smartkey_session_process_key(
            sess, eng, SMARTKEY_KEY_CHAR, static_cast<uint32_t>(*p), 100, &act
        );
        assert(ok);
        apply(buf, SMARTKEY_KEY_CHAR, static_cast<uint32_t>(*p), act);
    }
    assert(buf == U"the ");

    // 1x BackSpace snaps back to "teh"
    bool ok = smartkey_session_process_key(sess, eng, SMARTKEY_KEY_BACKSPACE, 0, 120, &act);
    assert(ok);
    apply(buf, SMARTKEY_KEY_BACKSPACE, 0, act);
    assert(buf == U"teh");

    // Verify Telex passthrough 2x graduation over C ABI
    assert(!smartkey_engine_is_telex_passthrough(eng, "aur"));
    assert(!smartkey_session_record_telex_passthrough_repair(sess, eng, "aur"));
    assert(smartkey_session_record_telex_passthrough_repair(sess, eng, "aur"));
    assert(smartkey_engine_is_telex_passthrough(eng, "aur"));

    smartkey_session_free(sess);
    smartkey_engine_free(eng);
    return 0;
}
"#,
    )
    .unwrap();

    let compile = Command::new("g++")
        .args([
            "-std=c++17",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            header_dir.to_str().unwrap(),
            cpp_src.to_str().unwrap(),
            staticlib.to_str().unwrap(),
            "-lpthread",
            "-ldl",
            "-lm",
            "-o",
            cpp_bin.to_str().unwrap(),
        ])
        .output()
        .expect("run g++");
    assert!(
        compile.status.success(),
        "g++ failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&cpp_bin).output().expect("run smoke_bin");
    assert!(
        run.status.success(),
        "smoke_bin failed:\nstderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}
