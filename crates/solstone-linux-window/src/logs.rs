// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use solstone_linux::copy::{CopyKey, PluralKey, plural_copy};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogOutcome {
    Empty,
    Missing,
    Unreadable,
    Failed,
    Complete,
    Partial,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenLog {
    pub content: String,
    pub outcome: LogOutcome,
}

pub const MAX_PREVIEW_BYTES: usize = 256 * 1024;

pub fn process_log_content(
    raw: Result<Option<String>, std::io::ErrorKind>,
    max_bytes: usize,
) -> FrozenLog {
    match raw {
        Err(std::io::ErrorKind::NotFound) => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Missing,
        },
        Err(std::io::ErrorKind::PermissionDenied) => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Unreadable,
        },
        Err(_) => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Failed,
        },
        Ok(None) => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Empty,
        },
        Ok(Some(text)) if text.trim().is_empty() => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Empty,
        },
        Ok(Some(text)) => {
            if text.len() <= max_bytes {
                FrozenLog {
                    content: text,
                    outcome: LogOutcome::Complete,
                }
            } else {
                let reserve = plural_copy(
                    PluralKey::OmittedBytes,
                    text.len(),
                    &crate::format::format_integer(text.len() as u64),
                )
                .len();
                let half = max_bytes.saturating_sub(reserve) / 2;
                let mut head_end = half;
                while !text.is_char_boundary(head_end) {
                    head_end -= 1;
                }
                let head = &text[..head_end];
                let mut tail_start = text.len() - half;
                while !text.is_char_boundary(tail_start) {
                    tail_start += 1;
                }
                let tail = &text[tail_start..];
                let omitted = text.len() - (head.len() + tail.len());
                let note = plural_copy(
                    PluralKey::OmittedBytes,
                    omitted,
                    &crate::format::format_integer(omitted as u64),
                );
                let mut content = format!("{head}{note}{tail}");
                if content.len() > max_bytes {
                    content.clear();
                }
                FrozenLog {
                    content,
                    outcome: LogOutcome::Partial,
                }
            }
        }
    }
}

pub fn save_frozen_log(frozen: &FrozenLog, dest_path: &Path) -> std::io::Result<()> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let parent = dest_path.parent().unwrap_or(Path::new("."));
    let temp = parent.join(format!(
        ".solstone-logs-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    write_log_copy(&frozen.content, dest_path, &temp)
}

fn write_log_copy(content: &str, dest_path: &Path, temp: &Path) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(temp)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temp, dest_path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}

pub fn collect_recent_logs() -> FrozenLog {
    let output = solstone_linux::bounded_process::run(
        std::process::Command::new("journalctl").args([
            "--user-unit=solstone-linux.service",
            "--no-pager",
            "--quiet",
            "-n",
            "1000",
            "--since",
            "24 hours ago",
        ]),
        Some(std::time::Duration::from_secs(10)),
        MAX_PREVIEW_BYTES,
    );
    match output {
        Err(_) => FrozenLog {
            content: String::new(),
            outcome: LogOutcome::Failed,
        },
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let partial = output.truncated
                || output.timed_out
                || !output.status.success()
                || !output.stderr.is_empty();
            if partial {
                if text.is_empty() {
                    return FrozenLog {
                        content: String::new(),
                        outcome: LogOutcome::Failed,
                    };
                }
                let note = CopyKey::LogsGap.as_str();
                let mut end = text.len().min(MAX_PREVIEW_BYTES.saturating_sub(note.len()));
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str(note);
                return FrozenLog {
                    content: text,
                    outcome: LogOutcome::Partial,
                };
            }
            process_log_content(Ok(Some(text)), MAX_PREVIEW_BYTES)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_log_outcomes() {
        assert_eq!(
            process_log_content(Err(std::io::ErrorKind::NotFound), 100).outcome,
            LogOutcome::Missing
        );
        assert_eq!(
            process_log_content(Err(std::io::ErrorKind::PermissionDenied), 100).outcome,
            LogOutcome::Unreadable
        );
        assert_eq!(
            process_log_content(Err(std::io::ErrorKind::Other), 100).outcome,
            LogOutcome::Failed
        );
        assert_eq!(
            process_log_content(Ok(None), 100).outcome,
            LogOutcome::Empty
        );
        assert_eq!(
            process_log_content(Ok(Some("   \n".into())), 100).outcome,
            LogOutcome::Empty
        );
        assert_eq!(
            process_log_content(Ok(Some("hello log".into())), 100).outcome,
            LogOutcome::Complete
        );
    }

    #[test]
    fn exclusive_creation_collision_never_deletes_an_unowned_file() {
        let root = std::path::PathBuf::from("/var/tmp")
            .join(format!("solstone-log-collision-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let destination = root.join("saved.txt");
        let temporary = root.join("preexisting.tmp");
        std::fs::write(&destination, b"old copy").unwrap();
        std::fs::write(&temporary, b"unowned material").unwrap();
        assert_eq!(
            write_log_copy("new copy", &destination, &temporary)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"old copy");
        assert_eq!(std::fs::read(&temporary).unwrap(), b"unowned material");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_export_preserves_the_destination_and_removes_its_temporary_file() {
        let root = std::path::PathBuf::from("/var/tmp")
            .join(format!("solstone-log-failure-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let destination = root.join("existing");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("kept"), b"previous material").unwrap();
        let preview = FrozenLog {
            content: "new preview".into(),
            outcome: LogOutcome::Complete,
        };
        assert!(save_frozen_log(&preview, &destination).is_err());
        assert_eq!(
            std::fs::read(destination.join("kept")).unwrap(),
            b"previous material"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn truncation_respects_multibyte_boundaries_and_total_preview_limit() {
        let text = "📔日本語é".repeat(100);
        for limit in [0, 1, 20, 48, 100, 101] {
            let frozen = process_log_content(Ok(Some(text.clone())), limit);
            assert!(frozen.content.len() <= limit);
            assert_eq!(frozen.outcome, LogOutcome::Partial);
        }
    }

    #[test]
    fn test_truncation_inserts_fixed_omitted_bytes_and_save_does_not_reread() {
        let mut source_content = "start_prefix".to_string();
        for _ in 0..500 {
            source_content.push_str("0123456789");
        }
        source_content.push_str("end_suffix");

        let frozen = process_log_content(Ok(Some(source_content.clone())), 100);
        assert_eq!(frozen.outcome, LogOutcome::Partial);
        assert!(frozen.content.contains("[omitted "));
        assert!(frozen.content.contains(" bytes]"));

        // Mutate the original source to ensure save writes only the frozen bytes
        source_content.push_str("_mutated_after_preview");

        let test_dir = std::path::PathBuf::from("/var/tmp")
            .join(format!("solstone-log-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&test_dir);
        let target_file = test_dir.join("saved-logs.txt");
        save_frozen_log(&frozen, &target_file).unwrap();

        let saved = std::fs::read_to_string(&target_file).unwrap();
        let _ = std::fs::remove_dir_all(&test_dir);
        assert_eq!(saved, frozen.content);
        assert!(!saved.contains("_mutated_after_preview"));
        assert!(saved.contains("[omitted "));
    }
}
