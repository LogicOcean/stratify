//! Stream A / A5 — the facade's failures must be matchable, not just
//! stringifiable.
//!
//! These are the cases reachable without touching the global subscriber; the
//! lifecycle variants (`AlreadyInitialized`, `SetGlobalDefault`,
//! `ReloadNotEnabled`) are asserted in `stream_a_init_lifecycle.rs` and
//! `stream_a_init_rollback.rs`, which own the process-wide slot.

#![cfg(feature = "logging")]

use stratify::logging::Error;
use stratify::logging::FileConfig;
use tracing_subscriber::filter::EnvFilter;

#[test]
fn flush_before_init_reports_not_initialized() {
    // Arrange / Act
    let error = stratify::logging::flush().expect_err("nothing has been initialized");

    // Assert
    assert!(
        matches!(error, Error::NotInitialized),
        "expected NotInitialized, got {error:?}"
    );
}

#[test]
fn reload_filter_before_init_reports_not_initialized() {
    // Arrange / Act
    let error = stratify::logging::reload_filter(EnvFilter::new("debug"))
        .expect_err("nothing has been initialized");

    // Assert
    assert!(
        matches!(error, Error::NotInitialized),
        "expected NotInitialized, got {error:?}"
    );
}

#[test]
fn an_unusable_log_directory_reports_io() {
    // Arrange — /proc is a read-only virtual filesystem, so create_dir_all
    // under it fails without needing a permissions dance.
    let builder = stratify::logging::builder().file(FileConfig::new("/proc/stratify-cannot-exist"));

    // Act — `build()`'s Ok type is an opaque `impl Subscriber`, which is not
    // Debug, so unwrap the error side by hand.
    let error = builder
        .build()
        .err()
        .expect("the directory cannot be created");

    // Assert — a caller can tell a filesystem problem from a lifecycle one and
    // fall back to console-only logging.
    assert!(matches!(error, Error::Io(_)), "expected Io, got {error:?}");
}

#[test]
fn distinct_failures_are_distinct_variants() {
    // Arrange
    let lifecycle = stratify::logging::flush().expect_err("not initialized");
    let filesystem = stratify::logging::builder()
        .file(FileConfig::new("/proc/stratify-cannot-exist"))
        .build()
        .err()
        .expect("the directory cannot be created");

    // Act — what a caller can actually branch on.
    let describe = |error: &Error| match error {
        Error::NotInitialized => "lifecycle",
        Error::Io(_) => "filesystem",
        _ => "other",
    };

    // Assert
    assert_eq!(describe(&lifecycle), "lifecycle");
    assert_eq!(describe(&filesystem), "filesystem");
}

#[test]
fn error_messages_stay_human_readable() {
    // Arrange / Act
    let error = stratify::logging::flush().expect_err("not initialized");

    // Assert — typing the error must not cost the operator a useful message.
    assert_eq!(error.to_string(), "logging is not initialized");
}
