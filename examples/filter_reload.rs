//! Reload a facade filter at runtime.
//!
//! Run with `cargo run --example filter_reload`. It writes `visible at info`
//! and `visible after reload` to stderr. The first DEBUG event is filtered out.

use stratify::logging::ConsoleConfig;
use tracing_subscriber::filter::EnvFilter;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let console = ConsoleConfig::default()
        .with_color(false)
        .with_thread_ids(false)
        .with_target(false);

    stratify::logging::builder()
        .console(console)
        .with_filter(EnvFilter::new("info"))
        .reloadable()
        .init()?;

    tracing::debug!("hidden before reload");
    tracing::info!("visible at info");

    stratify::logging::reload_filter(EnvFilter::new("debug"))?;
    tracing::debug!("visible after reload");

    stratify::logging::flush()?;
    Ok(())
}
