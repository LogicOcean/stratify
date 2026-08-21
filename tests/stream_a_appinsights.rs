//! Application Insights export.
//!
//! No live resource is contacted: construction is lazy, so a well-formed but
//! unroutable connection string exercises everything short of the network.

#![cfg(feature = "appinsights")]

use std::fs;
use std::path::PathBuf;
use stratify::logging::Error;
use stratify::logging::{appinsights::AppInsightsConfig, ConsoleConfig, FileConfig, FileFormat};
use tracing::subscriber::with_default;

const UNROUTABLE: &str = "InstrumentationKey=00000000-1111-2222-3333-444444444444;\
                          IngestionEndpoint=https://example.invalid/";

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lk-ai-{}-{label}", std::process::id()))
}

#[test]
fn app_insights_composes_with_the_other_sinks() {
    // Arrange: the combination the feature exists for.
    let dir = scratch("compose");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .app_insights(AppInsightsConfig::new(UNROUTABLE, "lk-test"))
        .build()
        .expect("all three sinks must build together");

    // Act
    with_default(subscriber, || tracing::info!("three sinks"));
    handle.flush();

    // Assert: the file sink is unaffected by the exporter's presence.
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    drop(handle);
    let _ = fs::remove_dir_all(&dir);
    assert!(body.contains("three sinks"), "got: {body}");
}

#[test]
fn a_malformed_connection_string_fails_the_build_rather_than_panicking() {
    // Arrange
    let built = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .app_insights(AppInsightsConfig::new("garbage", "svc"))
        .build();

    // Assert: a misconfiguration is a returned error, so a caller can choose to
    // carry on without the exporter.
    assert!(matches!(built.map(|_| ()), Err(Error::AppInsights(_))));
}

#[test]
fn flushing_with_an_unreachable_endpoint_does_not_hang_the_caller() {
    // Arrange: an endpoint that cannot resolve is the common failure in a
    // misconfigured environment, and it must not wedge shutdown.
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .app_insights(AppInsightsConfig::new(UNROUTABLE, "svc"))
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || tracing::info!("unreachable"));
    handle.flush();

    // Assert: reaching here is the assertion.
}

#[test]
fn from_lookup_is_an_error_when_the_variable_is_absent() {
    // Arrange: the absent case is expressed as a lookup that finds nothing,
    // rather than by clearing the real environment. `std::env::remove_var` is
    // `unsafe` because it races any concurrent read, and the harness runs
    // tests on many threads at once, so the old form was unsound as well as
    // order-dependent.

    // Act
    let result = AppInsightsConfig::from_lookup("svc", |_| None);

    // Assert: absence is a configuration choice, reported rather than assumed.
    assert!(matches!(result, Err(Error::AppInsights(_))));
}

#[test]
fn from_lookup_builds_a_config_when_the_variable_is_present() {
    // Arrange
    let present = |_: &str| Some("InstrumentationKey=00000000-0000-0000-0000-000000000000".into());

    // Act
    let config = AppInsightsConfig::from_lookup("svc", present).expect("a value is present");

    // Assert
    assert_eq!(config.service_name, "svc");
}
