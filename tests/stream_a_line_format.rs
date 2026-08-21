//! Caller-supplied line formatting.
//!
//! The assertion that matters is what reaches the sink, not that the builder
//! accepted the formatter.

#![cfg(feature = "logging")]

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use stratify::logging::{ConsoleConfig, FileConfig, FileFormat, LineFormatter, SpanScope};
use tracing::subscriber::with_default;

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lk-line-{}-{label}", std::process::id()))
}

/// A layout deliberately unlike the built-in one, so a passing test cannot be
/// the default output coincidentally matching.
struct Pipes;

impl LineFormatter for Pipes {
    fn format_line(
        &self,
        writer: &mut dyn std::fmt::Write,
        event: &tracing::Event<'_>,
        _spans: &SpanScope,
    ) -> std::fmt::Result {
        write!(
            writer,
            "|{}|{}|",
            event.metadata().level(),
            event.metadata().target()
        )
    }
}

/// Captures what a formatter is asked to write, proving it was consulted.
#[derive(Clone)]
struct Recording(Arc<Mutex<Vec<String>>>);

impl LineFormatter for Recording {
    fn format_line(
        &self,
        writer: &mut dyn std::fmt::Write,
        event: &tracing::Event<'_>,
        _spans: &SpanScope,
    ) -> std::fmt::Result {
        let line = format!("recorded:{}", event.metadata().level());
        self.0.lock().expect("not poisoned").push(line.clone());
        write!(writer, "{line}")
    }
}

#[test]
fn a_custom_formatter_controls_the_file_line() {
    // Arrange
    let dir = scratch("file");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(Pipes)
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || tracing::info!("ignored by this formatter"));
    handle.flush();
    drop(handle);

    // Assert
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let contents = fs::read_to_string(entry.path()).expect("readable");
    assert!(contents.starts_with("|INFO|"), "got: {contents}");
    assert!(
        !contents.contains("ignored by this formatter"),
        "the formatter decides what appears, got: {contents}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_custom_formatter_is_consulted_for_the_console() {
    // Arrange
    let recorder = Recording(Arc::new(Mutex::new(Vec::new())));
    let seen = recorder.0.clone();
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .console_format(recorder)
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!("one");
        tracing::warn!("two");
    });
    handle.flush();

    // Assert
    let lines = seen.lock().expect("not poisoned");
    assert_eq!(lines.len(), 2, "the formatter must see every event");
    assert_eq!(lines[0], "recorded:INFO");
    assert_eq!(lines[1], "recorded:WARN");
}

#[test]
fn one_formatter_type_serves_both_sinks() {
    // Arrange: the trait is independent of the subscriber type precisely so a
    // formatter written once works for either sink, at their different depths.
    let dir = scratch("both");
    let _ = fs::remove_dir_all(&dir);

    // Act
    let built = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .console_format(Pipes)
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(Pipes)
        .build();

    // Assert
    assert!(
        built.is_ok(),
        "the same formatter type must serve both sinks"
    );
    drop(built);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn without_a_formatter_the_built_in_layout_is_unchanged() {
    // Arrange: the hook must be opt-in.
    let dir = scratch("default");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || tracing::info!("built-in layout"));
    handle.flush();
    drop(handle);

    // Assert
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let contents = fs::read_to_string(entry.path()).expect("readable");
    assert!(contents.contains("built-in layout"), "got: {contents}");
    assert!(contents.contains("INFO"), "got: {contents}");
    assert!(!contents.starts_with('|'), "got: {contents}");

    let _ = fs::remove_dir_all(&dir);
}

/// Renders the enclosing span scope, to prove it reaches the formatter.
struct WithSpans;

impl LineFormatter for WithSpans {
    fn format_line(
        &self,
        writer: &mut dyn std::fmt::Write,
        _event: &tracing::Event<'_>,
        spans: &SpanScope,
    ) -> std::fmt::Result {
        let names: Vec<_> = spans.spans().iter().map(|s| s.name).collect();
        write!(
            writer,
            "scope=[{}] current={:?}",
            names.join(">"),
            spans.current().map(|s| s.name)
        )
    }
}

#[test]
fn a_formatter_receives_the_enclosing_span_scope() {
    // Arrange
    let dir = scratch("spans");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(WithSpans)
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        let outer = tracing::info_span!("outer");
        let _outer = outer.enter();
        let inner = tracing::info_span!("inner");
        let _inner = inner.enter();
        tracing::info!("nested");
    });
    handle.flush();
    drop(handle);

    // Assert: outermost first, and the innermost is reachable directly.
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    assert!(body.contains("scope=[outer>inner]"), "got: {body}");
    assert!(body.contains(r#"current=Some("inner")"#), "got: {body}");
}

#[test]
fn a_formatter_outside_any_span_gets_an_empty_scope() {
    // Arrange
    let dir = scratch("nospan");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(WithSpans)
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || tracing::info!("bare"));
    handle.flush();
    drop(handle);

    // Assert
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    assert!(body.contains("scope=[] current=None"), "got: {body}");
}
