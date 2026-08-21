//! File rotation strategies and retention cleanup for the logging facade.

/// File rotation strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rotation {
    /// Rotate logs daily at midnight (tracing-appender `Rotation::DAILY`).
    Daily,
    /// Rotate logs hourly at minute 0 (tracing-appender `Rotation::HOURLY`).
    Hourly,
    /// Never rotate logs — append to a single file indefinitely.
    Never,
    /// Roll once the active file reaches `max_bytes`, keeping `max_files`
    /// retired files.
    ///
    /// Time-based rotation cannot bound disk use between its rotation points: a
    /// service quiet for a week and chatty for an hour still produces one
    /// enormous file. This bounds it directly.
    Size {
        /// Size at which the active file is retired.
        max_bytes: u64,
        /// How many retired files to keep. The total on disk is one more.
        max_files: usize,
    },
}

/// Remove rotated log files older than `max_age_days`.
///
/// Scans `directory` for files whose name starts with `prefix` (typically
/// `"app.log"`) and removes those with modification times older than the
/// retention threshold.  Non-matching files and directories are ignored.
/// Errors (permissions, missing files) are silently skipped — this is
/// best-effort cleanup meant for startup hygiene, not for audit.
///
/// # Example
///
/// ```rust
/// use stratify::logging::file::cleanup_old_files;
/// // Remove rotated logs older than 30 days from /var/log/myapp
/// cleanup_old_files("/var/log/myapp", "app.log", 30);
/// ```
pub fn cleanup_old_files(directory: &str, prefix: &str, max_age_days: u32) {
    let dir = match std::fs::read_dir(directory) {
        Ok(d) => d,
        Err(_) => return,
    };

    let cutoff = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| {
            since.saturating_sub(std::time::Duration::from_secs(max_age_days as u64 * 86400))
        }) {
        Ok(threshold) => threshold,
        Err(_) => return,
    };

    for entry in dir.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(prefix) {
            continue;
        }
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if modified < UNIX_EPOCH + cutoff {
            let _ = std::fs::remove_file(&path);
        }
    }
}

use std::time::UNIX_EPOCH;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::time::Duration;

    #[test]
    fn cleanup_ignores_missing_directory() {
        // Should not panic on non-existent directory
        cleanup_old_files("/nonexistent/log/dir", "app.log", 30);
    }

    #[test]
    fn cleanup_removes_old_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        // Create a file with an old modification time
        let old_file = dir.path().join("app.log.2020-01-01");
        File::create(&old_file).unwrap();

        // Set mtime to Jan 1 2020
        let old_time = UNIX_EPOCH + Duration::from_secs(1577836800); // 2020-01-01
        filetime::set_file_mtime(&old_file, filetime::FileTime::from_system_time(old_time))
            .unwrap();

        cleanup_old_files(&dir_path, "app.log", 30);

        assert!(!old_file.exists(), "old file should have been cleaned up");
    }

    #[test]
    fn cleanup_preserves_recent_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        let recent_file = dir.path().join("app.log.2026-06-01");
        File::create(&recent_file).unwrap();

        cleanup_old_files(&dir_path, "app.log", 30);

        assert!(recent_file.exists(), "recent file should be preserved");
    }

    #[test]
    fn cleanup_ignores_unrelated_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let dir_path = dir.path().to_string_lossy().to_string();

        let unrelated = dir.path().join("other.log.2020-01-01");
        File::create(&unrelated).unwrap();

        cleanup_old_files(&dir_path, "app.log", 30);

        assert!(unrelated.exists(), "unrelated prefix should be ignored");
    }
}
