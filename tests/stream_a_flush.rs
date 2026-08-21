//! Stream A / A3 — `flush()` must actually drain the non-blocking queue.
//!
//! Every test here reads the file sink *while the subscriber is still alive*.
//! Dropping the handle has always drained as a side effect of dropping the
//! worker guards; that proves nothing about `flush()`, so nothing is dropped
//! before the assertion and no test sleeps.
//!
//! These state the guarantee, but they are not the proof. A file sink accepts
//! a line in microseconds, so the background worker often finishes on its own
//! and most of these tests would pass against a `flush()` that did nothing —
//! the exact false green this crate has been bitten by. The deterministic
//! proof lives in `src/facade/flush.rs`, where the sink is instrumented to take
//! a millisecond per line so an early return is impossible to miss.

#![cfg(feature = "logging")]

mod stream_a_support;

use stratify::logging::Handle;
use stream_a_support::{file_config, lines_written};
use tempfile::TempDir;
use tracing_subscriber::filter::EnvFilter;

const EVENTS: usize = 500;

/// Install a file-backed subscriber for the current thread and hand back its
/// handle plus the guard keeping the subscriber alive.
fn file_subscriber(dir: &TempDir) -> (Handle, tracing::subscriber::DefaultGuard) {
    let (subscriber, handle) = stratify::logging::builder()
        .file(file_config(dir.path()))
        .with_filter(EnvFilter::new("trace"))
        .build()
        .expect("build failed");

    let default = tracing::subscriber::set_default(subscriber);
    (handle, default)
}

fn emit(count: usize, tag: &str) {
    for i in 0..count {
        tracing::info!(index = i, tag, "flush event");
    }
}

#[test]
fn flush_drains_every_queued_event_to_disk_before_returning() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (handle, _default) = file_subscriber(&dir);
    emit(EVENTS, "first");

    // Act
    handle.flush();

    // Assert — no sleep, no drop: the events are on disk because flush put
    // them there.
    let written = lines_written(dir.path());
    assert_eq!(
        written.len(),
        EVENTS,
        "flush() returned with {} of {EVENTS} events still queued",
        written.len()
    );
}

#[test]
fn flush_is_repeatable_and_the_writer_survives_it() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (handle, _default) = file_subscriber(&dir);

    // Act
    emit(EVENTS, "first");
    handle.flush();
    let after_first = lines_written(dir.path()).len();

    emit(EVENTS, "second");
    handle.flush();
    let after_second = lines_written(dir.path());

    // Assert — a flush must not be a one-shot shutdown.
    assert_eq!(after_first, EVENTS);
    assert_eq!(after_second.len(), EVENTS * 2);
    assert!(after_second
        .iter()
        .any(|l| l.contains("\"tag\":\"second\"")));
}

#[test]
fn flushing_an_empty_queue_is_harmless() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (handle, _default) = file_subscriber(&dir);

    // Act
    handle.flush();
    handle.flush();
    emit(1, "after");
    handle.flush();

    // Assert
    assert_eq!(lines_written(dir.path()).len(), 1);
}

#[test]
fn a_cloned_handle_can_flush() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (handle, _default) = file_subscriber(&dir);
    let clone = handle.clone();
    emit(EVENTS, "clone");

    // Act
    clone.flush();

    // Assert — a clone that silently cannot flush is worse than no clone.
    assert_eq!(lines_written(dir.path()).len(), EVENTS);
}

#[test]
fn a_clone_keeps_the_writers_alive_after_the_original_is_dropped() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (handle, _default) = file_subscriber(&dir);
    let clone = handle.clone();

    // Act
    drop(handle);
    emit(EVENTS, "after-drop");
    clone.flush();

    // Assert
    assert_eq!(lines_written(dir.path()).len(), EVENTS);
}
