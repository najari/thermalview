//! Alarm event file: one row per raise, clear, pause or resume, appended to `alarm_log.csv` in
//! the save folder. The file is opened for each event (they are rare), so it can be read or
//! copied while the app runs. Excel-friendly like the measurement CSV: UTF-8 with BOM.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "alarm_log.csv";
const HEADER: &str = "time,event,rule,value_C";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Raised,
    Cleared,
    Paused,
    Resumed,
}

impl Event {
    fn name(self) -> &'static str {
        match self {
            Event::Raised => "raised",
            Event::Cleared => "cleared",
            Event::Paused => "paused",
            Event::Resumed => "resumed",
        }
    }
}

/// Append one event; the header is written when the file is new. `what` is the rule (or why
/// monitoring paused); `value` the reading that changed the state.
pub fn append(dir: &Path, event: Event, what: &str, value: Option<f32>) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let mut out = OpenOptions::new().create(true).append(true).open(&path)?;
    if out.metadata()?.len() == 0 {
        out.write_all("\u{FEFF}".as_bytes())?;
        writeln!(out, "{HEADER}")?;
    }
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let value = value.map(|v| format!("{v:.2}")).unwrap_or_default();
    writeln!(out, "{now},{},{},{value}", event.name(), quote(what))?;
    Ok(path)
}

/// A CSV field: quoted when it holds a comma or a quote.
fn quote(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_rows_under_one_header() {
        let dir = std::env::temp_dir().join(format!("thermalview_alarmlog_{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        append(&dir, Event::Raised, "S1 40.0 °C 이상", Some(41.234)).unwrap();
        append(&dir, Event::Paused, "카메라 영상 없음", None).unwrap();
        let path = append(&dir, Event::Cleared, "a, \"b\"", Some(39.0)).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        let lines: Vec<&str> = text.trim_start_matches('\u{FEFF}').lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], HEADER);
        assert!(
            lines[1].ends_with(",raised,S1 40.0 °C 이상,41.23"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].ends_with(",paused,카메라 영상 없음,"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].ends_with(",cleared,\"a, \"\"b\"\"\",39.00"),
            "{}",
            lines[3]
        );
        assert_eq!(text.matches('\u{FEFF}').count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}
