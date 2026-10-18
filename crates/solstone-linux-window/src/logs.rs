// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

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
                let half = max_bytes / 2;
                let head = &text[..half];
                let tail_start = text.len() - half;
                let tail = &text[tail_start..];
                let omitted = text.len() - (head.len() + tail.len());
                let content = format!("{head}[omitted {omitted} bytes]{tail}");
                FrozenLog {
                    content,
                    outcome: LogOutcome::Partial,
                }
            }
        }
    }
}

pub fn save_frozen_log(frozen: &FrozenLog, dest_path: &Path) -> std::io::Result<()> {
    std::fs::write(dest_path, frozen.content.as_bytes())
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
