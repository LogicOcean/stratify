//! Stream A / A6 — every public config type must be constructible and fully
//! configurable from outside the crate.
//!
//! `tests/` compiles as a separate crate, which is the only place
//! `error[E0639]: cannot create non-exhaustive struct using struct expression`
//! reproduces. A unit test inside `src/` passes against an unfixed
//! `#[non_exhaustive]` type and proves nothing.
//!
//! Every construction below uses exactly the pattern the doc examples show.

#![cfg(feature = "logging")]

mod stream_a_support;

use stratify::logging::file::Rotation;
use stratify::logging::rate_limit::RateLimit;
use stratify::logging::sampling::SampleConfig;
use stratify::logging::{ConsoleConfig, FileConfig, JsonConfig};
use stream_a_support::lines_written;
use tempfile::TempDir;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::filter::EnvFilter;

#[test]
fn console_config_is_fully_configurable_from_an_external_crate() {
    // Arrange / Act — every public field, set through the documented pattern.
    let config = ConsoleConfig::default()
        .with_color(false)
        .with_thread_ids(false)
        .with_target(false)
        .with_lossy(true);

    // Assert
    assert!(!config.use_color);
    assert!(!config.thread_ids);
    assert!(!config.target);
    assert!(config.lossy);
}

#[test]
fn json_config_is_fully_configurable_from_an_external_crate() {
    // Arrange / Act
    let config = JsonConfig::default()
        .with_span_list(false)
        .with_flatten(false)
        .with_lossy(true);

    // Assert
    assert!(!config.span_list);
    assert!(!config.flatten);
    assert!(config.lossy);
}

#[test]
fn sample_config_is_fully_configurable_from_an_external_crate() {
    // Arrange / Act
    let config = SampleConfig::new(0.25).with_min_level(LevelFilter::DEBUG);

    // Assert
    assert_eq!(config.rate, 0.25);
    assert_eq!(config.min_level, LevelFilter::DEBUG);
    assert_eq!(SampleConfig::default().with_rate(0.5).rate, 0.5);
}

#[test]
fn sample_config_clamps_an_out_of_range_rate() {
    // Arrange / Act — a probability outside [0, 1] is a caller mistake, and
    // silently trusting it would make `should_sample` behave unpredictably.
    let too_high = SampleConfig::new(7.5);
    let too_low = SampleConfig::default().with_rate(-2.0);

    // Assert
    assert_eq!(too_high.rate, 1.0);
    assert_eq!(too_low.rate, 0.0);
}

#[test]
fn rate_limit_is_fully_configurable_from_an_external_crate() {
    // Arrange / Act
    let custom = RateLimit::new(250, 15);

    // Assert
    assert_eq!(custom.max_events, 250);
    assert_eq!(custom.per_secs, 15);
    assert_eq!(RateLimit::per_second(10).per_secs, 1);
    assert_eq!(RateLimit::per_minute(10).per_secs, 60);
}

#[test]
fn file_config_is_fully_configurable_from_an_external_crate() {
    // Arrange / Act
    let config = FileConfig::new("/var/log/example")
        .with_rotation(Rotation::Hourly)
        .with_retention_days(14)
        .with_json_config(JsonConfig::default().with_span_list(false));

    // Assert
    assert_eq!(config.directory, "/var/log/example");
    assert_eq!(config.rotation, Rotation::Hourly);
    assert_eq!(config.retention_days, 14);
    assert!(!config.json_config.span_list);
}

/// Constructibility is worthless if the results cannot be handed to the
/// builder, so this drives all five through `build()` to a real sink.
#[test]
fn every_config_type_composes_into_a_working_subscriber() {
    // Arrange
    let dir = TempDir::new().expect("tempdir");
    let (subscriber, handle) = stratify::logging::builder()
        .console(ConsoleConfig::default().with_color(false).with_lossy(true))
        .json(JsonConfig::default().with_flatten(false))
        .file(
            FileConfig::new(dir.path().to_string_lossy().as_ref())
                .with_rotation(Rotation::Never)
                .with_json_config(JsonConfig::default().with_span_list(true)),
        )
        .sampling(SampleConfig::new(1.0).with_min_level(LevelFilter::INFO))
        .rate_limit(RateLimit::new(3, 60))
        .with_filter(EnvFilter::new("trace"))
        .build()
        .expect("build failed");

    // Act
    {
        let _default = tracing::subscriber::set_default(subscriber);
        for i in 0..10 {
            tracing::info!(index = i, "configured event");
        }
        tracing::debug!("below the sampler's min_level");
    }
    handle.flush();

    // Assert — the rate limit of 3 is what actually reached the file.
    assert_eq!(lines_written(dir.path()).len(), 3);
}

#[test]
fn the_reexported_tracing_reaches_the_sinks() {
    // Arrange: a consumer with no `tracing` dependency of its own uses the
    // re-export; the macros are `$crate`-hygienic, so emission and delivery
    // must work end to end through `stratify::logging::tracing` alone.
    use stratify::logging::tracing;

    let dir = std::env::temp_dir().join(format!("lk-reexport-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(stratify::logging::EnvFilter::new("info"))
        .file(
            stratify::logging::FileConfig::new(dir.to_string_lossy().to_string())
                .with_format(stratify::logging::FileFormat::Text),
        )
        .build()
        .expect("builds");

    // Act
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("reexported", via = "stratify");
        let _entered = span.enter();
        tracing::info!("emitted through the re-export");
    });
    handle.flush();
    drop(handle);

    // Assert
    let entry = std::fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = std::fs::read_to_string(entry.path()).expect("readable");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        body.contains("emitted through the re-export"),
        "got: {body}"
    );
    assert!(
        body.contains("via=\"stratify\""),
        "span fields render: {body}"
    );
}
