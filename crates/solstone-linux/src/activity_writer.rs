// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use url::Url;

pub const MAX_STRING_BYTES: usize = 4096;
pub const MAX_BODY_BYTES: usize = 65536;
pub const MAX_AGE_MS: u64 = 5000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesktopActivityPayload {
    Foreground {
        app: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        app_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Activity {
        state: String,
    },
    Availability {
        capability: String,
        status: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DesktopActivityRow {
    pub v: u32,
    pub t: String,
    #[serde(flatten)]
    pub payload: DesktopActivityPayload,
    pub ts: u64,
    pub end_ts: u64,
    pub method: String,
}

impl DesktopActivityRow {
    pub fn foreground(
        ts: u64,
        end_ts: u64,
        method: String,
        app: String,
        app_id: Option<String>,
        title: Option<String>,
        url: Option<String>,
    ) -> Self {
        assert!(end_ts >= ts);
        Self {
            v: 1,
            t: "desktop_activity".into(),
            payload: DesktopActivityPayload::Foreground {
                app,
                app_id,
                title,
                url,
            },
            ts,
            end_ts,
            method,
        }
    }

    pub fn activity(ts: u64, end_ts: u64, method: String, state: String) -> Self {
        assert!(end_ts >= ts);
        Self {
            v: 1,
            t: "desktop_activity".into(),
            payload: DesktopActivityPayload::Activity { state },
            ts,
            end_ts,
            method,
        }
    }

    pub fn availability(
        ts: u64,
        end_ts: u64,
        method: String,
        capability: String,
        status: String,
    ) -> Self {
        assert!(end_ts >= ts);
        Self {
            v: 1,
            t: "desktop_activity".into(),
            payload: DesktopActivityPayload::Availability { capability, status },
            ts,
            end_ts,
            method,
        }
    }
}

pub fn sanitize_url(raw: &str) -> Option<String> {
    if raw.len() > MAX_STRING_BYTES {
        return None;
    }
    let mut parsed = Url::parse(raw).ok()?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    parsed.set_query(None);
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

pub struct ActivityWriter {
    segment_dir: PathBuf,
    hms: String,
    artifact_counter: u64,
    current_file: Option<File>,
    current_path: Option<PathBuf>,
}

impl ActivityWriter {
    pub fn new(segment_dir: PathBuf, hms: String) -> Self {
        let mut max_counter = 0;
        if segment_dir.exists()
            && let Ok(entries) = fs::read_dir(&segment_dir)
        {
            let prefix = format!("{}_", hms);
            let suffix = "_desktop_activity.jsonl";
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if name.starts_with(&prefix) && name.ends_with(suffix) {
                    let middle = &name[prefix.len()..name.len() - suffix.len()];
                    if let Ok(n) = middle.parse::<u64>()
                        && n > max_counter
                    {
                        max_counter = n;
                    }
                }
            }
        }
        let artifact_counter = if max_counter > 0 { max_counter + 1 } else { 1 };
        Self {
            segment_dir,
            hms,
            artifact_counter,
            current_file: None,
            current_path: None,
        }
    }

    pub fn next_artifact(&mut self) {
        self.artifact_counter += 1;
        self.current_file = None;
        self.current_path = None;
    }

    pub fn write_row(&mut self, row: &DesktopActivityRow) -> io::Result<()> {
        let serialized = serde_json::to_string(row)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        if self.current_file.is_none() {
            fs::create_dir_all(&self.segment_dir)?;
            let mut attempts = 0;
            loop {
                let filename = format!(
                    "{}_{}_desktop_activity.jsonl",
                    self.hms, self.artifact_counter
                );
                let path = self.segment_dir.join(filename);
                match OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file) => {
                        self.current_path = Some(path);
                        self.current_file = Some(file);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        attempts += 1;
                        self.artifact_counter += 1;
                        if attempts >= 8 {
                            return Err(e);
                        }
                    }
                    Err(e) => return Err(e),
                }
            }
        }

        if let Some(ref mut file) = self.current_file {
            file.write_all(serialized.as_bytes())?;
            file.write_all(b"\n")?;
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }

    pub fn close(&mut self) {
        self.current_file = None;
        self.current_path = None;
    }
}

pub fn read_activity_rows_from_dir(dir: &Path) -> io::Result<Vec<DesktopActivityRow>> {
    let mut files: Vec<PathBuf> = Vec::new();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with("_desktop_activity.jsonl") && entry.path().is_file() {
            files.push(entry.path());
        }
    }
    files.sort_by(|a, b| {
        let a_str = a.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let b_str = b.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let a_parts = a_str.split('_').collect::<Vec<_>>();
        let b_parts = b_str.split('_').collect::<Vec<_>>();
        if a_parts.len() >= 2 && b_parts.len() >= 2 {
            let hms_cmp = a_parts[0].cmp(b_parts[0]);
            if !hms_cmp.is_eq() {
                return hms_cmp;
            }
            if let (Ok(na), Ok(nb)) = (a_parts[1].parse::<u64>(), b_parts[1].parse::<u64>()) {
                return na.cmp(&nb);
            }
        }
        a.cmp(b)
    });

    let mut rows = Vec::new();
    for file_path in files {
        let f = File::open(&file_path)?;
        let reader = BufReader::new(f);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let row: DesktopActivityRow = serde_json::from_str(&line)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            rows.push(row);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_url() {
        assert_eq!(
            sanitize_url("https://example.com/path?foo=bar#frag"),
            Some("https://example.com/path".into())
        );
        assert_eq!(
            sanitize_url("http://user:pass@example.com:8080/path/to/page?q=1"),
            Some("http://example.com:8080/path/to/page".into())
        );
        assert_eq!(sanitize_url("ftp://example.com/file"), None);
        assert_eq!(sanitize_url("file:///etc/passwd"), None);
        assert_eq!(sanitize_url("javascript:alert(1)"), None);
        assert_eq!(sanitize_url("not a url"), None);

        let long_url = format!("https://example.com/{}", "a".repeat(4090));
        assert!(long_url.len() > MAX_STRING_BYTES);
        assert_eq!(sanitize_url(&long_url), None);
    }

    #[test]
    fn test_activity_writer_and_reader() {
        let tmp = tempfile::tempdir().unwrap();
        let seg_dir = tmp.path().join("20261019_120000.incomplete");
        let mut writer = ActivityWriter::new(seg_dir.clone(), "120000".into());

        let row1 = DesktopActivityRow::foreground(
            1000,
            2000,
            "gnome-shell".into(),
            "Firefox".into(),
            Some("org.mozilla.firefox".into()),
            Some("My Title".into()),
            Some("https://example.com/docs".into()),
        );
        let row2 =
            DesktopActivityRow::activity(2000, 3000, "idle-detector".into(), "active".into());
        let row3 = DesktopActivityRow::availability(
            3000,
            4000,
            "gnome-shell".into(),
            "url".into(),
            "unavailable".into(),
        );

        writer.write_row(&row1).unwrap();
        writer.write_row(&row2).unwrap();
        writer.next_artifact();
        writer.write_row(&row3).unwrap();
        writer.close();

        let file1 = seg_dir.join("120000_1_desktop_activity.jsonl");
        let file2 = seg_dir.join("120000_2_desktop_activity.jsonl");
        assert!(file1.exists());
        assert!(file2.exists());

        let read_rows = read_activity_rows_from_dir(&seg_dir).unwrap();
        assert_eq!(read_rows.len(), 3);
        assert_eq!(read_rows[0], row1);
        assert_eq!(read_rows[1], row2);
        assert_eq!(read_rows[2], row3);
    }

    #[test]
    fn writer_restart_does_not_reopen_prior_artifact() {
        let tmp = tempfile::tempdir().unwrap();
        let seg_dir = tmp.path().join("20261019_120000.incomplete");
        let hms = "120000".to_string();

        let row1 = DesktopActivityRow::foreground(
            1000,
            2000,
            "gnome-shell".into(),
            "Firefox".into(),
            Some("org.mozilla.firefox".into()),
            Some("Title 1".into()),
            None,
        );

        {
            let mut writer1 = ActivityWriter::new(seg_dir.clone(), hms.clone());
            writer1.write_row(&row1).unwrap();
            writer1.close();
        }

        let file1 = seg_dir.join("120000_1_desktop_activity.jsonl");
        assert!(file1.exists());
        let file1_bytes = fs::read(&file1).unwrap();

        let row2 = DesktopActivityRow::foreground(
            2000,
            3000,
            "gnome-shell".into(),
            "Alacritty".into(),
            Some("Alacritty".into()),
            Some("Terminal".into()),
            None,
        );

        {
            let mut writer2 = ActivityWriter::new(seg_dir.clone(), hms.clone());
            writer2.write_row(&row2).unwrap();
            writer2.close();
        }

        // The first file's bytes and name are unchanged.
        assert_eq!(fs::read(&file1).unwrap(), file1_bytes);

        // The second row is in a different *_desktop_activity.jsonl.
        let file2 = seg_dir.join("120000_2_desktop_activity.jsonl");
        assert!(file2.exists());

        // read_activity_rows_from_dir returns both rows.
        let rows = read_activity_rows_from_dir(&seg_dir).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], row1);
        assert_eq!(rows[1], row2);
    }
}
