//! A writer that rolls to a new file once the current one reaches a size.
//!
//! `tracing-appender` rotates on time only. That leaves a service which is
//! quiet for a week and then chatty for an hour with one enormous file, and no
//! way to bound disk use between rotation points. This closes that gap.
//!
//! Retired files are numbered, newest first: `app.log.1` is the most recently
//! retired, and the highest number is the oldest. Numbering rather than
//! timestamping keeps the scheme independent of the clock, which matters when
//! several roll within the same second.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Rolls `directory/prefix` to `prefix.1`, `prefix.2`, … as it fills.
pub struct SizeRollingWriter {
    directory: PathBuf,
    prefix: String,
    max_bytes: u64,
    max_files: usize,
    compress: bool,
    file: Option<File>,
    written: u64,
}

impl SizeRollingWriter {
    /// Open, or create, the active log file.
    ///
    /// `max_files` counts retired files, so the total on disk is one more than
    /// this. Zero retires nothing: the file is truncated when it fills.
    pub fn new(
        directory: impl AsRef<Path>,
        prefix: impl Into<String>,
        max_bytes: u64,
        max_files: usize,
        compress: bool,
    ) -> io::Result<Self> {
        let directory = directory.as_ref().to_path_buf();
        fs::create_dir_all(&directory)?;
        let prefix = prefix.into();

        let path = directory.join(&prefix);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        // Start from the existing length so a restart does not reset the budget
        // and let the file grow without bound across restarts.
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);

        Ok(Self {
            directory,
            prefix,
            max_bytes,
            max_files,
            compress,
            file: Some(file),
            written,
        })
    }

    fn active_path(&self) -> PathBuf {
        self.directory.join(&self.prefix)
    }

    fn retired_path(&self, index: usize) -> PathBuf {
        self.directory.join(format!("{}.{index}", self.prefix))
    }

    /// Shift every retired file one number older, drop what falls off the end,
    /// and move the active file into slot 1.
    fn roll(&mut self) -> io::Result<()> {
        // Close before renaming: Windows refuses to rename an open file, and
        // dropping the handle here also flushes it.
        self.file = None;

        if self.max_files == 0 {
            fs::remove_file(self.active_path()).ok();
        } else {
            // Oldest first, so nothing is overwritten before it has moved.
            for index in (1..=self.max_files).rev() {
                let from = self.retired_path(index);
                if !from.exists() {
                    continue;
                }
                if index == self.max_files {
                    fs::remove_file(&from).ok();
                } else {
                    fs::rename(&from, self.retired_path(index + 1)).ok();
                }
            }
            let retired = self.retired_path(1);
            fs::rename(self.active_path(), &retired).ok();

            if self.compress {
                compress_in_place(&retired);
            }
        }

        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.active_path())?,
        );
        self.written = 0;
        Ok(())
    }
}

impl Write for SizeRollingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Roll before writing rather than after, so a line is never split
        // across two files.
        if self.max_bytes > 0
            && self.written + buf.len() as u64 > self.max_bytes
            && self.written > 0
        {
            self.roll()?;
        }

        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file is not open"))?;
        let count = file.write(buf)?;
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

/// Gzip a retired file in place, replacing it with `<name>.gz`.
///
/// Best-effort: a failure leaves the uncompressed file, which is the safe
/// outcome. Losing logs to save disk would be the wrong trade.
#[cfg(feature = "compression")]
fn compress_in_place(path: &Path) {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let Ok(contents) = fs::read(path) else {
        return;
    };
    let target = PathBuf::from(format!("{}.gz", path.display()));
    let Ok(file) = File::create(&target) else {
        return;
    };
    let mut encoder = GzEncoder::new(file, Compression::default());
    if encoder.write_all(&contents).is_ok() && encoder.finish().is_ok() {
        fs::remove_file(path).ok();
    } else {
        fs::remove_file(&target).ok();
    }
}

#[cfg(not(feature = "compression"))]
fn compress_in_place(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lk-roll-{}-{label}", std::process::id()))
    }

    #[test]
    fn rolls_once_the_size_is_exceeded() {
        // Arrange
        let dir = scratch("rolls");
        let _ = fs::remove_dir_all(&dir);
        let mut writer = SizeRollingWriter::new(&dir, "app.log", 32, 3, false).expect("opens");

        // Act
        for _ in 0..8 {
            writer.write_all(b"0123456789\n").expect("writes");
        }
        writer.flush().expect("flushes");

        // Assert
        assert!(dir.join("app.log").exists(), "active file must exist");
        assert!(dir.join("app.log.1").exists(), "a retired file must exist");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn never_exceeds_the_retained_file_count() {
        // Arrange: the point of the cap is bounded disk use.
        let dir = scratch("cap");
        let _ = fs::remove_dir_all(&dir);
        let mut writer = SizeRollingWriter::new(&dir, "app.log", 16, 2, false).expect("opens");

        // Act
        for _ in 0..40 {
            writer.write_all(b"0123456789\n").expect("writes");
        }
        writer.flush().expect("flushes");

        // Assert
        assert!(!dir.join("app.log.3").exists(), "retention cap exceeded");
        let count = fs::read_dir(&dir).expect("readable").count();
        assert!(count <= 3, "{count} files on disk, expected at most 3");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_is_never_split_across_two_files() {
        // Arrange
        let dir = scratch("whole");
        let _ = fs::remove_dir_all(&dir);
        let mut writer = SizeRollingWriter::new(&dir, "app.log", 20, 3, false).expect("opens");

        // Act
        writer.write_all(b"first-line-here\n").expect("writes");
        writer.write_all(b"second-line-here\n").expect("writes");
        writer.flush().expect("flushes");

        // Assert: each file holds whole lines.
        for entry in fs::read_dir(&dir).expect("readable").flatten() {
            let body = fs::read_to_string(entry.path()).expect("readable");
            if !body.is_empty() {
                assert!(body.ends_with('\n'), "split line in {body:?}");
            }
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_keeps_its_length_across_a_restart() {
        // Arrange: resetting the budget on restart would let a file grow
        // without bound.
        let dir = scratch("resume");
        let _ = fs::remove_dir_all(&dir);
        {
            let mut writer = SizeRollingWriter::new(&dir, "app.log", 64, 2, false).expect("opens");
            writer.write_all(b"0123456789\n").expect("writes");
            writer.flush().expect("flushes");
        }

        // Act
        let writer = SizeRollingWriter::new(&dir, "app.log", 64, 2, false).expect("reopens");

        // Assert
        assert_eq!(writer.written, 11);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zero_retained_files_truncates_rather_than_retiring() {
        // Arrange
        let dir = scratch("zero");
        let _ = fs::remove_dir_all(&dir);
        let mut writer = SizeRollingWriter::new(&dir, "app.log", 16, 0, false).expect("opens");

        // Act
        for _ in 0..10 {
            writer.write_all(b"0123456789\n").expect("writes");
        }
        writer.flush().expect("flushes");

        // Assert
        assert!(!dir.join("app.log.1").exists(), "nothing should be retired");
        assert!(dir.join("app.log").exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
