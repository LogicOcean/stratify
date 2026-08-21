//! Stream A / A4 — a failed `init()` must leave the singleton clean.
//!
//! `set_global_default` is process-wide, so this file holds exactly one test:
//! it deliberately occupies the global subscriber slot first, and any second
//! test in the same binary would race it.

#![cfg(feature = "logging")]

use stratify::logging::ConsoleConfig;
use stratify::logging::Error;
use tracing_subscriber::Registry;

#[test]
fn a_failed_init_leaves_the_singleton_clean_and_does_not_block_a_retry() {
    // Arrange — occupy the global subscriber slot, exactly as another library
    // in the process might have. `init()` will now get past `build()` and fail
    // at `set_global_default`.
    tracing::subscriber::set_global_default(Registry::default())
        .expect("nothing should have claimed the global subscriber yet");

    // Act
    let first = stratify::logging::builder()
        .console(ConsoleConfig::default())
        .init();

    // Assert — the failure must be reported, and nothing may be stored.
    let first = first.expect_err("init() should have failed to set the global");
    assert!(
        matches!(first, Error::SetGlobalDefault(_)),
        "expected SetGlobalDefault, got {first:?}"
    );
    assert!(
        !stratify::logging::is_initialized(),
        "init() stored the handle despite failing to install a subscriber"
    );
    assert!(
        stratify::logging::handle().is_none(),
        "a handle is reachable for a subscriber that was never installed"
    );

    // Act — retry. The earlier failure must not have poisoned the state.
    let second = stratify::logging::builder()
        .console(ConsoleConfig::default())
        .init();

    // Assert — it still fails, but for the real reason (the global slot is
    // taken), not because the first attempt left AlreadyInitialized behind.
    // This is the distinction the typed error exists to make legible.
    let second = second.expect_err("the global slot is still taken");
    assert!(
        matches!(second, Error::SetGlobalDefault(_)),
        "the failed first attempt made the state unrecoverable: {second:?}"
    );
    assert!(!stratify::logging::is_initialized());
}
