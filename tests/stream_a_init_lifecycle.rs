//! Stream A / A4 — `init()` / `reset()` semantics on the happy path.
//!
//! `init()` installs a process-wide subscriber, so this file holds exactly one
//! test; a second one in the same binary would race it for the global slot.
//! The single test walks the lifecycle in order and labels each phase, because
//! the phases genuinely cannot be observed independently.

#![cfg(feature = "logging")]

mod stream_a_support;

use stratify::logging::Error;
use stream_a_support::{file_config, lines_written};
use tempfile::TempDir;
use tracing_subscriber::filter::EnvFilter;

const BEFORE_RESET: usize = 50;
const AFTER_RESET: usize = 25;

fn emit(count: usize, tag: &str) {
    for i in 0..count {
        tracing::info!(index = i, tag, "lifecycle event");
    }
}

#[test]
fn init_then_reset_keeps_logging_alive_and_leaves_a_recoverable_state() {
    let dir = TempDir::new().expect("tempdir");

    // ── init() installs the subscriber and records the handle ──────────
    stratify::logging::builder()
        .file(file_config(dir.path()))
        .with_filter(EnvFilter::new("trace"))
        .init()
        .expect("init failed");

    assert!(stratify::logging::is_initialized());

    // A second init() is refused, and says so specifically.
    let again = stratify::logging::builder()
        .init()
        .expect_err("logging is already initialized");
    assert!(
        matches!(again, Error::AlreadyInitialized),
        "expected AlreadyInitialized, got {again:?}"
    );

    // Reload was not requested, and that is a distinct failure from "not
    // initialized" — a caller can tell "you forgot .reloadable()" from
    // "you forgot .init()".
    let reload = stratify::logging::reload_filter(EnvFilter::new("debug"))
        .expect_err("this subscriber was not built reloadable");
    assert!(
        matches!(reload, Error::ReloadNotEnabled),
        "expected ReloadNotEnabled, got {reload:?}"
    );

    emit(BEFORE_RESET, "before");
    stratify::logging::flush().expect("global flush failed");
    assert_eq!(lines_written(dir.path()).len(), BEFORE_RESET);

    // ── reset() forgets the handle but must not strand the writers ─────
    let kept = stratify::logging::handle().expect("handle should be reachable");
    stratify::logging::reset();

    assert!(!stratify::logging::is_initialized());
    assert!(stratify::logging::handle().is_none());
    let flushed = stratify::logging::flush().expect_err("there is no stored handle");
    assert!(
        matches!(flushed, Error::NotInitialized),
        "expected NotInitialized, got {flushed:?}"
    );

    // The subscriber is still installed and still owns its writers, so events
    // after reset() are written rather than queued for a worker that has gone.
    emit(AFTER_RESET, "after");
    kept.flush();
    assert_eq!(
        lines_written(dir.path()).len(),
        BEFORE_RESET + AFTER_RESET,
        "reset() discarded events into a queue nobody reads"
    );

    // ── a later init() fails honestly, and stores nothing ──────────────
    let retry = stratify::logging::builder()
        .file(file_config(dir.path()))
        .init()
        .expect_err("the global subscriber is still installed");

    assert!(
        matches!(retry, Error::SetGlobalDefault(_)),
        "reset() should not make init() claim it is already initialized: {retry:?}"
    );
    assert!(
        !stratify::logging::is_initialized(),
        "the failed retry stored a handle"
    );
}
