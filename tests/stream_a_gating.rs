//! Stream A / A1 — `Builder::rate_limit` and `Builder::sampling` must have an
//! observable effect at a sink, not merely be stored on the builder.
//!
//! Every assertion here counts lines that actually reached a file on disk.

#![cfg(feature = "logging")]

mod stream_a_support;

use stratify::logging::rate_limit::RateLimit;
use stratify::logging::sampling::SampleConfig;
use stratify::logging::Builder;
use stream_a_support::{file_config, lines_written};
use tempfile::TempDir;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::EnvFilter;

/// Number of events every burst test emits. Comfortably above any limit used
/// below, so "the excess was dropped" is unambiguous.
const BURST: usize = 200;

/// Emit `BURST` info events through `builder`, then drain and return what the
/// file sink actually received.
fn burst_through(builder: Builder, dir: &TempDir) -> Vec<String> {
    let (subscriber, handle) = builder
        .file(file_config(dir.path()))
        .with_filter(EnvFilter::new("trace"))
        .build()
        .expect("build failed");

    {
        let _default = tracing::subscriber::set_default(subscriber);
        for i in 0..BURST {
            tracing::info!(index = i, "burst event");
        }
    }
    handle.flush();
    lines_written(dir.path())
}

#[test]
fn without_gating_every_event_reaches_the_sink() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");

    // Act
    let written = burst_through(stratify::logging::builder(), &dir);

    // Assert — establishes the baseline the gating tests are measured against.
    assert_eq!(written.len(), BURST);
}

#[test]
fn rate_limit_drops_events_beyond_the_configured_budget() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let budget = 5;

    // Act
    let written = burst_through(
        stratify::logging::builder().rate_limit(RateLimit::per_minute(budget)),
        &dir,
    );

    // Assert — the bucket starts full and a sub-millisecond burst refills
    // essentially nothing, so exactly `budget` events survive.
    assert_eq!(
        written.len(),
        budget as usize,
        "rate_limit(per_minute({budget})) let {} of {BURST} events through",
        written.len()
    );
}

#[test]
fn sampling_rate_zero_lets_nothing_reach_the_sink() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let sampling = SampleConfig::new(0.0);

    // Act
    let written = burst_through(stratify::logging::builder().sampling(sampling), &dir);

    // Assert
    assert!(
        written.is_empty(),
        "sampling rate 0.0 still wrote {} lines",
        written.len()
    );
}

#[test]
fn sampling_rate_one_lets_everything_reach_the_sink() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let sampling = SampleConfig::new(1.0);

    // Act
    let written = burst_through(stratify::logging::builder().sampling(sampling), &dir);

    // Assert
    assert_eq!(written.len(), BURST);
}

#[test]
fn sampling_min_level_gates_the_sink_through_the_builder() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let sampling = SampleConfig::default().with_min_level(LevelFilter::WARN);

    let (subscriber, handle) = stratify::logging::builder()
        .sampling(sampling)
        .file(file_config(dir.path()))
        .with_filter(EnvFilter::new("trace"))
        .build()
        .expect("build failed");

    // Act
    {
        let _default = tracing::subscriber::set_default(subscriber);
        tracing::error!("kept error");
        tracing::warn!("kept warn");
        tracing::info!("dropped info");
        tracing::debug!("dropped debug");
        tracing::trace!("dropped trace");
    }
    handle.flush();

    // Assert
    let written = lines_written(dir.path());
    assert_eq!(written.len(), 2, "written: {written:#?}");
    assert!(written[0].contains("kept error"));
    assert!(written[1].contains("kept warn"));
}

#[test]
fn sampling_runs_before_rate_limiting() {
    // Arrange — sampling drops everything, so the limiter must never be
    // consulted and its bucket must stay full.
    let dir = TempDir::new().expect("tempdir");
    let sampling = SampleConfig::new(0.0);

    // Act
    let written = burst_through(
        stratify::logging::builder()
            .sampling(sampling)
            .rate_limit(RateLimit::per_minute(5)),
        &dir,
    );

    // Assert
    assert!(written.is_empty(), "written: {written:#?}");
}

#[test]
fn gating_does_not_suppress_spans() {
    // Arrange — a rate limiter with a spent budget must not disable spans,
    // or every enclosing span in the process would silently disappear.
    let dir = TempDir::new().expect("tempdir");

    let (subscriber, handle) = stratify::logging::builder()
        .rate_limit(RateLimit::per_minute(1))
        .file(file_config(dir.path()))
        .with_filter(EnvFilter::new("trace"))
        .build()
        .expect("build failed");

    // Act
    {
        let _default = tracing::subscriber::set_default(subscriber);
        for _ in 0..10 {
            let span = tracing::info_span!("gated_span", kind = "test");
            let _entered = span.enter();
            tracing::info!("inside span");
        }
    }
    handle.flush();

    // Assert — one event survives the budget, and it still carries its span.
    let written = lines_written(dir.path());
    assert_eq!(written.len(), 1, "written: {written:#?}");
    assert!(
        written[0].contains("gated_span"),
        "span context was lost: {}",
        written[0]
    );
}
