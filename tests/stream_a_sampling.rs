//! Stream A / A2 — `Sampler::should_sample` level semantics, driven through a
//! real subscriber rather than by inspecting the config struct.

#![cfg(feature = "logging")]

mod stream_a_support;

use stratify::logging::sampling::{SampleConfig, Sampler};
use stream_a_support::CaptureLayer;
use tracing::level_filters::LevelFilter;
use tracing::Level;
use tracing_subscriber::filter::DynFilterFn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::Registry;

/// Emit one event at every level through a subscriber gated by `config` and
/// report which levels reached the sink.
///
/// `DynFilterFn` (rather than `FilterFn`) is deliberate: `FilterFn` caches its
/// verdict per callsite as `Interest::always`/`never`, which would hide any
/// per-event behaviour under test.
fn levels_reaching_sink(config: SampleConfig) -> Vec<Level> {
    let sampler = Sampler::new(config);
    let sink = CaptureLayer::new();

    let subscriber = Registry::default()
        .with(DynFilterFn::new(move |meta, _| sampler.should_sample(meta)))
        .with(sink.clone());

    tracing::subscriber::with_default(subscriber, || {
        tracing::error!("error event");
        tracing::warn!("warn event");
        tracing::info!("info event");
        tracing::debug!("debug event");
        tracing::trace!("trace event");
    });

    sink.levels()
}

fn config_at(min_level: LevelFilter) -> SampleConfig {
    SampleConfig::new(1.0).with_min_level(min_level)
}

#[test]
fn should_sample_min_level_debug_keeps_debug_and_above_and_drops_trace() {
    // Arrange
    let config = config_at(LevelFilter::DEBUG);

    // Act
    let passed = levels_reaching_sink(config);

    // Assert
    assert_eq!(
        passed,
        vec![Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG],
        "min_level = DEBUG must keep ERROR/WARN/INFO/DEBUG and drop TRACE"
    );
}

#[test]
fn should_sample_min_level_warn_keeps_only_error_and_warn() {
    // Arrange
    let config = config_at(LevelFilter::WARN);

    // Act
    let passed = levels_reaching_sink(config);

    // Assert
    assert_eq!(passed, vec![Level::ERROR, Level::WARN]);
}

#[test]
fn should_sample_min_level_trace_keeps_every_level() {
    // Arrange
    let config = config_at(LevelFilter::TRACE);

    // Act
    let passed = levels_reaching_sink(config);

    // Assert
    assert_eq!(
        passed,
        vec![
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE
        ]
    );
}

#[test]
fn should_sample_min_level_off_drops_every_level() {
    // Arrange
    let config = config_at(LevelFilter::OFF);

    // Act
    let passed = levels_reaching_sink(config);

    // Assert
    assert!(passed.is_empty(), "min_level = OFF must drop everything");
}

#[test]
fn should_sample_rate_zero_drops_events_the_level_filter_would_admit() {
    // Arrange
    let config = SampleConfig::new(0.0).with_min_level(LevelFilter::TRACE);

    // Act
    let passed = levels_reaching_sink(config);

    // Assert
    assert!(passed.is_empty(), "rate 0.0 must drop everything");
}
