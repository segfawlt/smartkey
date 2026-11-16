use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use smartkey_core::lexicon::PackLexicon;
use smartkey_core::{
    is_vietnamese_dict_word, resolve_lotus_dict_path, simulate_lotus_word, DiagnosticEvent,
    EventLog, Preset, RuleStore, SmartKeyEngine,
};

static LOTUS_ENV_LOCK: Mutex<()> = Mutex::new(());

fn tmp_dir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "smartkey_lotus_auth_test_{tag}_{}_{}",
        std::process::id(),
        id
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

struct EnvVarGuard {
    key: &'static str,
    prev: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, val: Option<&Path>) -> Self {
        let prev = std::env::var_os(key);
        match val {
            Some(p) => std::env::set_var(key, p),
            None => std::env::remove_var(key),
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

#[test]
fn shuffled_identical_lotus_dict_emits_zero_drift_notices() {
    let _guard = LOTUS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bundled_vi = PackLexicon::from_bundled_vi().expect("bundled-vi enabled");
    assert!(!bundled_vi.syllable_dict().is_empty());

    let dict_dir = tmp_dir("shuffled");
    let dict_path = dict_dir.join("vietnamese.cm.dict");
    let mut lines: Vec<&str> = bundled_vi
        .syllable_dict()
        .iter()
        .rev()
        .map(Box::as_ref)
        .collect();
    if let Some(&first) = lines.first() {
        lines.push(first);
    }
    let content = format!("# shuffled test copy\n\n{}", lines.join("\n"));
    fs::write(&dict_path, content).unwrap();

    let _env = EnvVarGuard::set("SMARTKEY_LOTUS_DICT", Some(&dict_path));
    assert_eq!(resolve_lotus_dict_path().as_deref(), Some(dict_path.as_path()));

    let log_dir = tmp_dir("shuffled_log");
    let mut store = RuleStore::in_memory(Preset::Balanced);
    store.set_log_dir(Some(log_dir.clone()));

    let engine = SmartKeyEngine::new_from_dirs(&[], true, store);
    assert!(engine.is_vi_dict_word("tiếng"));
    assert!(engine.is_vi_dict_word("không"));

    let records = EventLog::read_records_in_range(&log_dir, None, None).unwrap();
    let drift_count = records
        .iter()
        .filter(|r| matches!(&r.event, DiagnosticEvent::PackNotice { notice, .. } if notice == "lotus_dict_drift"))
        .count();
    assert_eq!(drift_count, 0, "expected 0 drift notices: {records:?}");

    drop(_env);
    let _no_env = EnvVarGuard::set("SMARTKEY_LOTUS_DICT", None);
    if Path::new("/usr/share/fcitx5/lotus/vietnamese.cm.dict").is_file() {
        let live_log = tmp_dir("live_log");
        let mut live_store = RuleStore::in_memory(Preset::Balanced);
        live_store.set_log_dir(Some(live_log.clone()));
        let _live_eng = SmartKeyEngine::new_from_dirs(&[], true, live_store);
        let live_records = EventLog::read_records_in_range(&live_log, None, None).unwrap();
        let live_drift = live_records
            .iter()
            .filter(|r| matches!(&r.event, DiagnosticEvent::PackNotice { notice, .. } if notice == "lotus_dict_drift"))
            .count();
        assert_eq!(live_drift, 0, "live lotus dict unexpectedly drifted: {live_records:?}");
        let _ = fs::remove_dir_all(&live_log);
    }

    let _ = fs::remove_dir_all(&dict_dir);
    let _ = fs::remove_dir_all(&log_dir);
}

#[test]
fn modified_lotus_dict_acts_as_runtime_authority_and_emits_one_drift_notice() {
    let _guard = LOTUS_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dict_dir = tmp_dir("modified");
    let dict_path = dict_dir.join("vietnamese.cm.dict");
    // Custom phonologically valid syllable `tờng` (`towngf`, not in standard vietnamese.cm.dict) plus `tiếng`; omits `không`.
    fs::write(&dict_path, "tờng\ntiếng\n").unwrap();

    let _env = EnvVarGuard::set("SMARTKEY_LOTUS_DICT", Some(&dict_path));
    assert_eq!(resolve_lotus_dict_path().as_deref(), Some(dict_path.as_path()));

    // Standalone vi_telex functions also respect SMARTKEY_LOTUS_DICT runtime authority:
    assert!(is_vietnamese_dict_word("tờng"));
    assert!(!is_vietnamese_dict_word("không"));
    assert!(simulate_lotus_word("towngf").is_valid_vietnamese);

    let log_dir = tmp_dir("modified_log");
    let mut store = RuleStore::in_memory(Preset::Balanced);
    store.set_log_dir(Some(log_dir.clone()));

    let engine = SmartKeyEngine::new_from_dirs(&[], true, store);
    assert!(engine.is_vi_dict_word("tờng"));
    assert!(engine.is_vi_dict_word("tiếng"));
    assert!(!engine.is_vi_dict_word("không"));

    let records = EventLog::read_records_in_range(&log_dir, None, None).unwrap();
    let drift_count = records
        .iter()
        .filter(|r| matches!(&r.event, DiagnosticEvent::PackNotice { notice, .. } if notice == "lotus_dict_drift"))
        .count();
    assert_eq!(drift_count, 1, "expected exactly 1 drift notice: {records:?}");

    let _ = fs::remove_dir_all(&dict_dir);
    let _ = fs::remove_dir_all(&log_dir);
}
