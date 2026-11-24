use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use smartkey_core::event_log::date_string;

const DEFAULT_MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ROTATIONS_PER_DAY: u32 = 1;

pub struct TraceLogger {
    log_dir: PathBuf,
    max_file_bytes: u64,
    active_date: String,
    file: Option<File>,
    bytes_written: u64,
    rotated_today: u32,
    start: Instant,
    last_mono_ms: u64,
}

impl TraceLogger {
    pub fn new(log_dir: PathBuf) -> Self {
        Self {
            log_dir,
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            active_date: String::new(),
            file: None,
            bytes_written: 0,
            rotated_today: 0,
            start: Instant::now(),
            last_mono_ms: 0,
        }
    }

    pub fn mono_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub fn epoch_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }

    pub fn close_if_open(&mut self) {
        self.file = None;
        self.bytes_written = 0;
        self.active_date.clear();
        self.rotated_today = 0;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_event(
        &mut self,
        ctx_id: u64,
        app: &str,
        ev: &str,
        out: &str,
        vkey: Option<u16>,
        ch: Option<&str>,
        del: Option<u32>,
        txt: Option<&str>,
        s0: &str,
        s1: Option<&str>,
    ) {
        let ts_ms = Self::epoch_ms();
        let mono_ms = self.mono_ms();
        let dt_ms = if self.last_mono_ms == 0 || mono_ms < self.last_mono_ms {
            0
        } else {
            mono_ms - self.last_mono_ms
        };
        self.last_mono_ms = mono_ms;

        let mut obj = serde_json::Map::new();
        obj.insert("v".to_string(), serde_json::Value::from(1u8));
        obj.insert("ts_ms".to_string(), serde_json::Value::from(ts_ms));
        obj.insert("mono_ms".to_string(), serde_json::Value::from(mono_ms));
        obj.insert("dt_ms".to_string(), serde_json::Value::from(dt_ms));
        obj.insert("ic".to_string(), serde_json::Value::from(ctx_id));
        obj.insert("app".to_string(), serde_json::Value::from(app));
        obj.insert("fe".to_string(), serde_json::Value::from("tsf"));
        obj.insert("im".to_string(), serde_json::Value::from("smartkey-en"));
        obj.insert("ev".to_string(), serde_json::Value::from(ev));
        obj.insert("out".to_string(), serde_json::Value::from(out));
        if let Some(vk) = vkey {
            obj.insert("code".to_string(), serde_json::Value::from(vk));
        }
        if let Some(c) = ch.filter(|s| !s.is_empty()) {
            obj.insert("ch".to_string(), serde_json::Value::from(c));
        }
        if let Some(d) = del {
            obj.insert("del".to_string(), serde_json::Value::from(d));
        }
        if let Some(t) = txt.filter(|s| !s.is_empty()) {
            obj.insert("txt".to_string(), serde_json::Value::from(t));
        }
        obj.insert("s0".to_string(), serde_json::Value::from(s0));
        if let Some(next_s) = s1.filter(|s| !s.is_empty() && *s != s0) {
            obj.insert("s1".to_string(), serde_json::Value::from(next_s));
        }

        let Ok(line) = serde_json::to_string(&serde_json::Value::Object(obj)) else {
            return;
        };
        self.write_line(ts_ms, &line);
    }

    fn write_line(&mut self, ts_ms: u64, line: &str) {
        let day = date_string(ts_ms);
        if day != self.active_date {
            self.close_if_open();
            self.active_date = day;
        }
        let needed = (line.len() + 1) as u64;
        if self.file.is_none() && !self.open_for_active_day() {
            return;
        }
        if self.bytes_written.saturating_add(needed) > self.max_file_bytes
            && !self.rotate_or_stop()
        {
            return;
        }
        if let Some(file) = self.file.as_mut() {
            if writeln!(file, "{line}").is_ok() {
                self.bytes_written = self.bytes_written.saturating_add(needed);
            }
        }
    }

    fn active_path(&self) -> PathBuf {
        self.log_dir
            .join(format!("trace-{}.jsonl", self.active_date))
    }

    fn open_for_active_day(&mut self) -> bool {
        if self.log_dir.as_os_str().is_empty() || self.active_date.is_empty() {
            return false;
        }
        if fs::create_dir_all(&self.log_dir).is_err() {
            return false;
        }
        let path = self.active_path();
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                self.bytes_written = f.metadata().map(|m| m.len()).unwrap_or(0);
                let rot_path = PathBuf::from(format!("{}.1", path.display()));
                self.rotated_today = u32::from(rot_path.exists());
                self.file = Some(f);
                true
            }
            Err(_) => false,
        }
    }

    fn rotate_or_stop(&mut self) -> bool {
        if self.rotated_today >= MAX_ROTATIONS_PER_DAY {
            return false;
        }
        let path = self.active_path();
        let rot_path = PathBuf::from(format!("{}.1", path.display()));
        self.file = None;
        let _ = fs::rename(&path, &rot_path);
        self.rotated_today += 1;
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => {
                self.bytes_written = 0;
                self.file = Some(f);
                true
            }
            Err(_) => false,
        }
    }
}

pub fn format_snapshot_compact(
    mode: &str,
    buf: &str,
    aux: &str,
    prev: &str,
    trailing_count: u32,
) -> String {
    let mut out = match mode {
        "Idle" => "Idle".to_string(),
        "Composing" => {
            if aux.is_empty() || aux == buf {
                format!("Composing({buf})")
            } else {
                format!("Composing({buf},peak={aux})")
            }
        }
        "AfterWord" => {
            if trailing_count > 0 {
                format!("AfterWord({buf},sp={trailing_count})")
            } else {
                format!("AfterWord({buf})")
            }
        }
        "JustAutocorrected" => {
            format!("JustAutocorrected({buf}<-{aux},tb={trailing_count})")
        }
        "EditingRestored" => format!("EditingRestored({buf}<-{aux})"),
        _ => format!("{mode}({buf})"),
    };
    if !prev.is_empty() {
        out.push_str(&format!("[prev={prev}]"));
    }
    out
}

pub fn prune_log_dir_now(dir: &Path, keep_days: u32) {
    smartkey_core::EventLog::prune_old_files(dir, TraceLogger::epoch_ms(), keep_days);
}
