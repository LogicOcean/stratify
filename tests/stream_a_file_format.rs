//! The file sink's wire format.
//!
//! JSON has always been the only option. These assert that `Text` produces a
//! human-readable line and that `Json` is unchanged, because the default has to
//! stay exactly as it was for every existing caller.

#![cfg(feature = "logging")]

use std::fs;
use std::path::PathBuf;

use stratify::logging::{ConsoleConfig, FileConfig, FileFormat};
use tracing::subscriber::with_default;

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lk-format-{}-{label}", std::process::id()))
}

/// Emit one event to a file sink in `format` and return what landed on disk.
fn write_one(dir: &PathBuf, format: FileFormat) -> String {
    let _ = fs::remove_dir_all(dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(format))
        .build()
        .expect("the file sink should build");

    with_default(subscriber, || {
        tracing::info!(request_id = "req-7", "hello from the file sink");
    });
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(dir)
        .expect("the log directory should exist")
        .filter_map(Result::ok)
        .next()
        .expect("a log file should have been written");
    fs::read_to_string(entry.path()).expect("the log file should be readable")
}

#[test]
fn text_format_writes_a_human_readable_line() {
    // Arrange
    let dir = scratch("text");

    // Act
    let contents = write_one(&dir, FileFormat::Text);

    // Assert: readable, and emphatically not a JSON object.
    assert!(
        !contents.trim_start().starts_with('{'),
        "text format must not emit JSON, got: {contents}"
    );
    assert!(
        contents.contains("hello from the file sink"),
        "got: {contents}"
    );
    assert!(contents.contains("INFO"), "got: {contents}");
    assert!(
        contents.contains("req-7"),
        "structured fields survive: {contents}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn text_format_writes_no_ansi_escapes() {
    // Arrange: colour codes in a file are noise, and a shipper reading it later
    // has no terminal to render them.
    let dir = scratch("noansi");

    // Act
    let contents = write_one(&dir, FileFormat::Text);

    // Assert
    assert!(
        !contents.contains('\u{1b}'),
        "escape codes reached the file"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn json_remains_the_default() {
    // Arrange: every existing caller relies on this, so the default must not
    // have moved.
    let dir = scratch("default");

    // Act
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()))
        .build()
        .expect("builds");
    with_default(subscriber, || tracing::info!("default format"));
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(&dir)
        .expect("directory exists")
        .filter_map(Result::ok)
        .next()
        .expect("a log file was written");
    let contents = fs::read_to_string(entry.path()).expect("readable");

    // Assert
    assert!(
        contents.trim_start().starts_with('{'),
        "the default must still be JSON, got: {contents}"
    );
    assert_eq!(FileFormat::default(), FileFormat::Json);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn json_format_is_still_available_explicitly() {
    // Arrange
    let dir = scratch("json");

    // Act
    let contents = write_one(&dir, FileFormat::Json);

    // Assert
    assert!(contents.trim_start().starts_with('{'), "got: {contents}");
    assert!(contents.contains("\"level\":\"INFO\""), "got: {contents}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn span_fields_in_the_file_carry_no_ansi_even_with_color_off_everywhere() {
    // Arrange: every sink colourless — the configuration a service captured
    // into Log Analytics runs. Span fields used to arrive with italic/dim
    // escape codes anyway, because the *layer's* ANSI flag, not the event
    // format's, decides how span fields are cached, and it defaulted on.
    let dir = std::env::temp_dir().join(format!("lk-ansi-off-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(stratify::logging::EnvFilter::new("info"))
        .console(ConsoleConfig::default().with_color(false))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .build()
        .expect("builds");

    // Act: the event fires inside a span carrying fields, which is what the
    // per-request middleware of a web service produces on every line.
    with_default(subscriber, || {
        let span = tracing::info_span!("http_request", method = "GET", path = "/health");
        let _entered = span.enter();
        tracing::info!(latency = 3, "finished");
    });
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

    assert!(body.contains("http_request"), "the span renders: {body}");
    assert!(
        !body.contains('\u{1b}'),
        "no escape codes anywhere in the file: {body:?}"
    );
}

#[test]
fn a_colored_console_cannot_bleed_ansi_into_the_file() {
    // Arrange: console colour *on*, beside a text file. These used to share
    // one span-field cache, so whichever layer formatted a span first decided
    // what the other printed — the reason services had to turn console colour
    // off to protect their files. The file sink now has its own cache.
    let dir = std::env::temp_dir().join(format!("lk-ansi-bleed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(stratify::logging::EnvFilter::new("info"))
        .console(ConsoleConfig::default().with_color(true))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        let span = tracing::info_span!("http_request", method = "GET");
        let _entered = span.enter();
        tracing::info!("finished");
    });
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

    assert!(
        !body.contains('\u{1b}'),
        "console colour must stay out of the file: {body:?}"
    );
}
