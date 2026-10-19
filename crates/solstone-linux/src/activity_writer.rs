// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteFault {
    ShortWrite,
    Newline,
    Sync,
    ShortWriteAndRollback,
}

pub struct ActivityWriter {
    segment_dir: PathBuf,
    hms: String,
    current_file: Option<File>,
    current_path: Option<PathBuf>,
    /// Bytes known to be a complete, synced prefix of the file.
    durable_len: u64,
    rollback_required: bool,
    #[cfg(test)]
    rollback_failures: usize,
    #[cfg(test)]
    fault: Option<WriteFault>,
}

/// A new id every file. Scanning names still on disk would reuse `1` after the
/// segment directory is removed.
fn artifact_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(u128::from(sequence) << 64)
        .wrapping_add(u128::from(std::process::id()));
    let mut bytes = mixed.to_le_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

impl ActivityWriter {
    pub fn new(segment_dir: PathBuf, hms: String) -> Self {
        Self {
            segment_dir,
            hms,
            current_file: None,
            current_path: None,
            durable_len: 0,
            rollback_required: false,
            #[cfg(test)]
            rollback_failures: 0,
            #[cfg(test)]
            fault: None,
        }
    }

    /// The next closed row starts a new file. This writer does not reopen one.
    pub fn next_artifact(&mut self) {
        self.current_file = None;
        self.current_path = None;
        self.durable_len = 0;
        self.rollback_required = false;
    }

    #[cfg(test)]
    fn fail_next(&mut self, fault: WriteFault) {
        self.fault = Some(fault);
    }

    pub fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    fn open_fresh(&mut self) -> io::Result<()> {
        fs::create_dir_all(&self.segment_dir)?;
        let mut last_error = None;
        for _ in 0..8 {
            let filename = format!("{}_{}_desktop_activity.jsonl", self.hms, artifact_id());
            let path = self.segment_dir.join(filename);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    self.current_path = Some(path);
                    self.current_file = Some(file);
                    self.durable_len = 0;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "activity file id collided")
        }))
    }

    /// JSON, the newline, and the sync land together. A failed attempt rolls
    /// back to the last synced row so a retry cannot duplicate a partial line.
    pub fn write_row(&mut self, row: &DesktopActivityRow) -> io::Result<()> {
        if self.rollback_required {
            self.rollback(self.durable_len)?;
            self.rollback_required = false;
        }
        let serialized = serde_json::to_vec(row)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if self.current_file.is_none() {
            self.open_fresh()?;
        }
        let durable = self.durable_len;
        if let Err(error) = self.append_record(&serialized) {
            self.rollback_required = true;
            self.rollback(durable)?;
            self.rollback_required = false;
            return Err(error);
        }
        Ok(())
    }

    fn append_record(&mut self, payload: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        let fault = self.fault.take();
        let file = self
            .current_file
            .as_mut()
            .ok_or_else(|| io::Error::other("activity file is not open"))?;
        #[cfg(test)]
        if matches!(
            fault,
            Some(WriteFault::ShortWrite | WriteFault::ShortWriteAndRollback)
        ) {
            let mid = payload.len() / 2;
            if mid > 0 {
                file.write_all(&payload[..mid])?;
            }
            if fault == Some(WriteFault::ShortWriteAndRollback) {
                self.rollback_failures = 2;
            }
            return Err(io::Error::other("short-write"));
        }
        file.write_all(payload)?;
        #[cfg(test)]
        if fault == Some(WriteFault::Newline) {
            return Err(io::Error::other("newline"));
        }
        file.write_all(b"\n")?;
        file.flush()?;
        #[cfg(test)]
        if fault == Some(WriteFault::Sync) {
            return Err(io::Error::other("sync"));
        }
        file.sync_all()?;
        self.durable_len = self
            .durable_len
            .saturating_add(payload.len() as u64)
            .saturating_add(1);
        Ok(())
    }

    fn rollback(&mut self, durable: u64) -> io::Result<()> {
        #[cfg(test)]
        if self.rollback_failures > 0 {
            self.rollback_failures -= 1;
            return Err(io::Error::other("rollback-unavailable"));
        }
        let Some(file) = self.current_file.as_mut() else {
            return Ok(());
        };
        file.set_len(durable)?;
        file.seek(SeekFrom::Start(durable))?;
        file.sync_all()?;
        self.durable_len = durable;
        Ok(())
    }

    pub fn close(&mut self) {
        self.current_file = None;
        self.current_path = None;
        self.durable_len = 0;
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
    fn failed_rollback_blocks_append_until_durable_prefix_is_restored() {
        let tmp = tempfile::tempdir().unwrap();
        let mut writer = ActivityWriter::new(tmp.path().to_owned(), "120000".into());
        let first = DesktopActivityRow::activity(0, 1000, "native".into(), "active".into());
        let next = DesktopActivityRow::activity(1000, 2000, "native".into(), "idle".into());
        writer.write_row(&first).unwrap();
        let path = writer.current_path().unwrap().to_owned();
        writer.fail_next(WriteFault::ShortWriteAndRollback);
        assert!(writer.write_row(&next).is_err());
        let dirty = fs::read(&path).unwrap();
        assert!(writer.write_row(&next).is_err());
        assert_eq!(fs::read(&path).unwrap(), dirty);
        writer.write_row(&next).unwrap();
        assert_eq!(
            read_activity_rows_from_dir(tmp.path()).unwrap(),
            vec![first, next]
        );
    }

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
        let first = writer.current_path().unwrap().to_path_buf();
        writer.next_artifact();
        writer.write_row(&row3).unwrap();
        let second = writer.current_path().unwrap().to_path_buf();
        writer.close();

        assert_ne!(first, second);
        assert!(
            first.ends_with("_desktop_activity.jsonl")
                || first
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with("_desktop_activity.jsonl")
        );
        assert!(
            second
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_desktop_activity.jsonl")
        );
        assert!(
            !first
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains("_1_desktop_activity")
        );

        let mut read_rows = read_activity_rows_from_dir(&seg_dir).unwrap();
        read_rows.sort_by_key(|row| row.ts);
        assert_eq!(read_rows, vec![row1, row2, row3]);
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

        let file1 = {
            let mut writer1 = ActivityWriter::new(seg_dir.clone(), hms.clone());
            writer1.write_row(&row1).unwrap();
            let path = writer1.current_path().unwrap().to_path_buf();
            writer1.close();
            path
        };
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

        let file2 = {
            let mut writer2 = ActivityWriter::new(seg_dir.clone(), hms.clone());
            writer2.write_row(&row2).unwrap();
            let path = writer2.current_path().unwrap().to_path_buf();
            writer2.close();
            path
        };

        assert_eq!(fs::read(&file1).unwrap(), file1_bytes);
        assert_ne!(file1, file2);
        assert!(
            file2
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_desktop_activity.jsonl")
        );

        let mut rows = read_activity_rows_from_dir(&seg_dir).unwrap();
        rows.sort_by_key(|row| row.ts);
        assert_eq!(rows, vec![row1, row2]);
    }

    #[test]
    fn removed_directory_does_not_reuse_the_artifact_id() {
        let tmp = tempfile::tempdir().unwrap();
        let seg_dir = tmp.path().join("120000.incomplete");
        let row = DesktopActivityRow::activity(1, 2, "unavailable".into(), "active".into());
        let first = {
            let mut writer = ActivityWriter::new(seg_dir.clone(), "120000".into());
            writer.write_row(&row).unwrap();
            writer.current_path().unwrap().to_path_buf()
        };
        let first_name = first.file_name().unwrap().to_os_string();
        fs::remove_dir_all(&seg_dir).unwrap();
        let mut writer = ActivityWriter::new(seg_dir, "120000".into());
        writer.write_row(&row).unwrap();
        let second_name = writer
            .current_path()
            .unwrap()
            .file_name()
            .unwrap()
            .to_os_string();
        assert_ne!(first_name, second_name);
    }

    #[test]
    fn failed_append_keeps_prior_rows_and_retries_once() {
        for fault in [
            WriteFault::ShortWrite,
            WriteFault::Newline,
            WriteFault::Sync,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let seg_dir = tmp.path().join("120000.incomplete");
            let mut writer = ActivityWriter::new(seg_dir, "120000".into());
            let row1 =
                DesktopActivityRow::activity(1_000, 2_000, "unavailable".into(), "active".into());
            let row2 =
                DesktopActivityRow::activity(2_000, 3_000, "unavailable".into(), "idle".into());
            writer.write_row(&row1).unwrap();
            let path = writer.current_path().unwrap().to_path_buf();
            writer.fail_next(fault);
            assert!(writer.write_row(&row2).is_err());
            let during = fs::read_to_string(&path).unwrap();
            assert_eq!(during.lines().count(), 1);
            assert!(!during.contains("idle"));
            writer.write_row(&row2).unwrap();
            let after = fs::read_to_string(&path).unwrap();
            assert_eq!(after.lines().count(), 2);
            let rows = read_activity_rows_from_dir(path.parent().unwrap()).unwrap();
            assert_eq!(rows, vec![row1, row2]);
        }
    }
}
