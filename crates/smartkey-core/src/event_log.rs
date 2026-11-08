use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::store::DiagnosticEvent;

/// Verbosity level for the durable JSONL event log (`[diagnostics].log_level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EventLogLevel {
    /// Log corrections, undos, manual edits, near-miss unknowns, real-word context misses,
    /// rule graduations/supersessions, and app profile transitions.
    #[default]
    Interesting,
    /// Log every committed word (including unchanged valid/bypassed tokens).
    All,
}

impl EventLogLevel {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "all" | "verbose" | "debug" => Self::All,
            _ => Self::Interesting,
        }
    }
}

/// `[diagnostics]` configuration parsed from `rules.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    pub event_log: bool,
    pub log_level: EventLogLevel,
    pub keep_days: u32,
    pub near_miss_band: f32,
    pub annotate_rules: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            event_log: true,
            log_level: EventLogLevel::Interesting,
            keep_days: 14,
            near_miss_band: 0.5,
            annotate_rules: true,
        }
    }
}

/// Resolves `$XDG_STATE_HOME/smartkey/logs` (defaulting to `~/.local/state/smartkey/logs`).
pub fn default_state_log_dir() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        let trimmed = xdg.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join("smartkey/logs");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".local/state/smartkey/logs");
        }
    }
    PathBuf::from(".smartkey/logs")
}

/// Returns current wall-clock time in milliseconds since `UNIX_EPOCH`.
pub fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Howard Hinnant's `civil_from_days` algorithm: converts days since 1970-01-01 to `(year, month, day)`.
pub fn civil_from_days(z_in: i64) -> (i32, u32, u32) {
    let z = z_in + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d)
}

/// Howard Hinnant's `days_from_civil` algorithm: converts `(year, month, day)` to days since 1970-01-01.
pub fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let y = (year as i64) - if month <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32; // [0, 399]
    let m = month;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + (doe as i64) - 719_468
}

/// Formats `epoch_ms` as `"YYYY-MM-DD"`.
pub fn date_string(epoch_ms: u64) -> String {
    let days = (epoch_ms / 86_400_000) as i64;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn parse_event_filename_days(file_name: &str) -> Option<i64> {
    let stem = file_name
        .strip_prefix("events-")?
        .strip_suffix(".jsonl")?;
    let mut parts = stem.split('-');
    let y_str = parts.next()?;
    let m_str = parts.next()?;
    let d_str = parts.next()?;
    if parts.next().is_some() || y_str.len() != 4 || m_str.len() != 2 || d_str.len() != 2 {
        return None;
    }
    let y: i32 = y_str.parse().ok()?;
    let m: u32 = m_str.parse().ok()?;
    let d: u32 = d_str.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Append-only day-partitioned JSONL writer.
///
/// Never reads `$HOME` implicitly; the log directory is always injected by the host
/// (`None` in unit tests and in-memory stores by default).
#[derive(Debug, Clone, Default)]
pub struct EventLog {
    dir: Option<PathBuf>,
    app: Option<String>,
    last_pruned_date: Option<String>,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_dir(&mut self, dir: Option<PathBuf>) {
        self.dir = dir;
        self.last_pruned_date = None;
    }

    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    pub fn set_app(&mut self, app: Option<&str>) {
        self.app = app.map(str::to_string);
    }

    pub fn app(&self) -> Option<&str> {
        self.app.as_deref()
    }

    /// Evaluates level and near-miss band filters for a `DiagnosticEvent`.
    pub fn should_record(diag: &DiagnosticsConfig, ev: &DiagnosticEvent) -> bool {
        if !diag.event_log {
            return false;
        }
        match ev {
            DiagnosticEvent::WordUnchanged { .. } => diag.log_level == EventLogLevel::All,
            DiagnosticEvent::UnfixedUnknown {
                best_candidate,
                best_score,
                threshold,
                ..
            } => {
                if diag.log_level == EventLogLevel::All || diag.near_miss_band <= 0.0 {
                    return true;
                }
                best_candidate.is_some() && *best_score >= diag.near_miss_band * (*threshold)
            }
            _ => true,
        }
    }

    pub fn record(&mut self, diag: &DiagnosticsConfig, lang: &str, ev: &DiagnosticEvent) {
        self.record_at(diag, lang, ev, current_epoch_ms());
    }

    pub fn record_at(
        &mut self,
        diag: &DiagnosticsConfig,
        lang: &str,
        ev: &DiagnosticEvent,
        ts_ms: u64,
    ) {
        let Some(dir) = self.dir.as_ref().cloned() else {
            return;
        };
        if !Self::should_record(diag, ev) {
            return;
        }

        let date = date_string(ts_ms);
        if fs::create_dir_all(&dir).is_err() {
            return;
        }

        if self.last_pruned_date.as_deref() != Some(date.as_str()) {
            Self::prune_old_files(&dir, ts_ms, diag.keep_days);
            self.last_pruned_date = Some(date.clone());
        }

        let Ok(Value::Object(mut obj)) = serde_json::to_value(ev) else {
            return;
        };
        Self::stamp_envelope(&mut obj, ts_ms, self.app.as_deref(), lang);

        let Ok(line) = serde_json::to_string(&Value::Object(obj)) else {
            return;
        };

        let path = dir.join(format!("events-{date}.jsonl"));
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }

    fn stamp_envelope(
        obj: &mut Map<String, Value>,
        ts_ms: u64,
        host_app: Option<&str>,
        lang: &str,
    ) {
        obj.insert("v".to_string(), Value::from(1u8));
        obj.insert("ts_ms".to_string(), Value::from(ts_ms));
        if !obj.contains_key("app") {
            match host_app {
                Some(a) => {
                    obj.insert("app".to_string(), Value::from(a));
                }
                None => {
                    obj.insert("app".to_string(), Value::Null);
                }
            }
        }
        obj.insert("lang".to_string(), Value::from(lang));
    }

    pub fn prune_old_files(dir: &Path, now_ms: u64, keep_days: u32) {
        if keep_days == 0 {
            return;
        }
        let today_days = (now_ms / 86_400_000) as i64;
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(file_days) = parse_event_filename_days(&name) {
                if today_days - file_days >= keep_days as i64 {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MatchKind;

    #[test]
    fn civil_date_roundtrip_and_formatting() {
        assert_eq!(date_string(0), "1970-01-01");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        // 2026-11-08 00:00:00 UTC
        let days = days_from_civil(2026, 11, 8);
        assert_eq!(civil_from_days(days), (2026, 11, 8));
        let ms = (days as u64) * 86_400_000 + 12_345;
        assert_eq!(date_string(ms), "2026-11-08");
    }

    #[test]
    fn near_miss_band_and_level_filtering() {
        let mut cfg = DiagnosticsConfig::default();
        let near_miss = DiagnosticEvent::UnfixedUnknown {
            word: "tehre".to_string(),
            best_candidate: Some("there".to_string()),
            best_score: 0.09,
            threshold: 0.14,
            total_count: 1,
        };
        let far_miss = DiagnosticEvent::UnfixedUnknown {
            word: "xyzzy".to_string(),
            best_candidate: Some("fuzzy".to_string()),
            best_score: 0.03,
            threshold: 0.14,
            total_count: 1,
        };
        let no_cand = DiagnosticEvent::UnfixedUnknown {
            word: "qwfp".to_string(),
            best_candidate: None,
            best_score: 0.0,
            threshold: 0.14,
            total_count: 1,
        };
        let unchanged = DiagnosticEvent::WordUnchanged {
            word: "hello".to_string(),
            reason: MatchKind::ValidWord,
        };

        assert!(EventLog::should_record(&cfg, &near_miss));
        assert!(!EventLog::should_record(&cfg, &far_miss));
        assert!(!EventLog::should_record(&cfg, &no_cand));
        assert!(!EventLog::should_record(&cfg, &unchanged));

        cfg.log_level = EventLogLevel::All;
        assert!(EventLog::should_record(&cfg, &far_miss));
        assert!(EventLog::should_record(&cfg, &no_cand));
        assert!(EventLog::should_record(&cfg, &unchanged));

        cfg.event_log = false;
        assert!(!EventLog::should_record(&cfg, &near_miss));
    }
}
