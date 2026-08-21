//! Minimal `stratify::logging` setup.
//!
//! Run with `cargo run --example logging_basic --features logging`. It writes an INFO line containing
//! `server started` to stderr.

use stratify::logging::ConsoleConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let console = ConsoleConfig::default()
        .with_color(false)
        .with_thread_ids(false)
        .with_target(false);

    stratify::logging::builder().console(console).init()?;

    tracing::info!(request_id = "req-42", "server started");

    stratify::logging::flush()?;
    Ok(())
}
