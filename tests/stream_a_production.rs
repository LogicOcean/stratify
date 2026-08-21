//! Global fields, panic capture, dropped-line accounting, console target,
//! file prefix and timestamp selection.

#![cfg(feature = "logging")]

use std::fs;
use std::path::PathBuf;

use stratify::logging::{ConsoleConfig, ConsoleTarget, FileConfig, FileFormat, TimestampFormat};
use tracing::subscriber::with_default;

fn scratch(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lk-prod-{}-{label}", std::process::id()))
}

/// Emit one event to a text file sink and return what landed on disk.
fn write_one(
    config: FileConfig,
    build: impl FnOnce(stratify::logging::Builder) -> stratify::logging::Builder,
) -> String {
    let dir = PathBuf::from(config.directory.clone());
    let _ = fs::remove_dir_all(&dir);
    let builder = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(config);
    let (subscriber, handle) = build(builder).build().expect("builds");
    with_default(subscriber, || tracing::info!("event body"));
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let name = entry.file_name().to_string_lossy().into_owned();
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);
    format!("{name}\n{body}")
}

#[test]
fn global_fields_appear_on_every_text_line() {
    // Arrange
    let dir = scratch("globals");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);

    // Act
    let out = write_one(config, |b| {
        b.global_field("service", "nse-api")
            .global_field("version", "1.2.3")
    });

    // Assert
    assert!(out.contains("service=nse-api"), "got: {out}");
    assert!(out.contains("version=1.2.3"), "got: {out}");
}

#[test]
fn a_repeated_global_field_key_replaces_rather_than_duplicates() {
    // Arrange
    let dir = scratch("dupe");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);

    // Act
    let out = write_one(config, |b| {
        b.global_field("env", "staging").global_field("env", "prod")
    });

    // Assert
    assert!(out.contains("env=prod"), "got: {out}");
    assert!(!out.contains("env=staging"), "got: {out}");
}

#[test]
fn without_global_fields_there_is_no_leading_space() {
    // Arrange: the empty-prefix path must render exactly as it did before.
    let dir = scratch("nolead");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);

    // Act
    let out = write_one(config, |b| b);

    // Assert
    let line = out.lines().nth(1).expect("a log line");
    assert!(!line.starts_with(' '), "leading space in: {line:?}");
}

#[test]
fn the_file_prefix_is_configurable() {
    // Arrange: two services sharing a directory need distinct names.
    let dir = scratch("prefix");
    let config = FileConfig::new(dir.to_string_lossy().to_string())
        .with_format(FileFormat::Text)
        .with_prefix("nse-api.log");

    // Act
    let out = write_one(config, |b| b);

    // Assert
    let name = out.lines().next().expect("a file name");
    assert!(name.starts_with("nse-api.log"), "got: {name}");
}

#[test]
fn timestamps_can_be_omitted() {
    // Arrange
    let dir = scratch("nots");
    let config = FileConfig::new(dir.to_string_lossy().to_string())
        .with_format(FileFormat::Text)
        .with_timestamp(TimestampFormat::None);

    // Act
    let out = write_one(config, |b| b);

    // Assert: no year at the start of the line.
    let line = out.lines().nth(1).expect("a log line");
    assert!(
        !line.starts_with("202"),
        "timestamp still present: {line:?}"
    );
    assert!(line.contains("event body"), "got: {line}");
}

#[test]
fn the_console_target_is_selectable() {
    // Arrange / Act: stdout must build as readily as the stderr default.
    let built = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default().with_target_stream(ConsoleTarget::Stdout))
        .build();

    // Assert
    assert!(built.is_ok());
}

#[test]
fn dropped_lines_start_at_zero_and_are_reportable() {
    // Arrange
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || tracing::info!("one"));
    handle.flush();
    let dropped = handle.dropped_lines();

    // Assert: a healthy sink reports nothing dropped, and the total is
    // reachable without knowing which sinks were configured.
    assert_eq!(dropped.total(), 0);
    assert!(!dropped.any());
}

#[test]
fn panic_capture_installs_without_disturbing_the_previous_hook() {
    // Arrange: the hook chains, so whatever the harness installed still runs.
    let built = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .capture_panics()
        .build();

    // Assert
    assert!(built.is_ok());

    // Act: a caught panic must still unwind normally.
    let result = std::panic::catch_unwind(|| panic!("deliberate"));
    assert!(result.is_err(), "panics must still propagate");
}

#[test]
fn redaction_masks_configured_field_values() {
    // Arrange
    let dir = scratch("redact");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(config)
        .redact(["password", "token"])
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!(password = "hunter2", user = "alice", "login attempt");
    });
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    // Assert
    assert!(!body.contains("hunter2"), "the secret survived: {body}");
    assert!(body.contains("password=[redacted]"), "got: {body}");
    assert!(
        body.contains("user=\"alice\""),
        "unrelated fields must survive: {body}"
    );
}

#[test]
fn redaction_is_case_insensitive_on_the_field_name() {
    // Arrange: callers should not have to guess the casing used at the call site.
    let dir = scratch("redactcase");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(config)
        .redact(["AUTHORIZATION"])
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!(authorization = "Bearer abc123", "request");
    });
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    // Assert
    assert!(!body.contains("abc123"), "the secret survived: {body}");
}

#[test]
fn without_redaction_values_are_untouched() {
    // Arrange: opt-in, and the buffering it needs must not run otherwise.
    let dir = scratch("noredact");
    let config = FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text);

    // Act
    let out = write_one(config, |b| b);

    // Assert
    assert!(out.contains("event body"), "got: {out}");
    assert!(!out.contains("[redacted]"), "got: {out}");
}

#[test]
fn size_rotation_bounds_the_file_and_retires_older_ones() {
    // Arrange
    let dir = scratch("sizeroll");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(
            FileConfig::new(dir.to_string_lossy().to_string())
                .with_format(FileFormat::Text)
                .with_rotation(stratify::logging::file::Rotation::Size {
                    max_bytes: 512,
                    max_files: 2,
                }),
        )
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        for i in 0..200 {
            tracing::info!(
                iteration = i,
                "a reasonably long line to fill the file quickly"
            );
        }
    });
    handle.flush();
    drop(handle);

    // Assert: rolled, and the retention cap held.
    assert!(dir.join("app.log").exists(), "active file must exist");
    assert!(dir.join("app.log.1").exists(), "a retired file must exist");
    assert!(!dir.join("app.log.3").exists(), "retention cap exceeded");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_syslog_sink_builds_and_does_not_disturb_other_sinks() {
    // Arrange: a host with no syslog socket is normal and must not fail.
    let dir = scratch("syslog");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .syslog(stratify::logging::syslog::SyslogConfig::new("lk-test"))
        .build()
        .expect("a syslog sink must build even with no daemon present");

    // Act
    with_default(subscriber, || tracing::info!("alongside syslog"));
    handle.flush();
    drop(handle);

    // Assert: the file sink is unaffected by syslog's presence.
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);
    assert!(body.contains("alongside syslog"), "got: {body}");
}

#[test]
fn redaction_applies_to_a_custom_file_formatter_too() {
    // Arrange: a caller who configured `redact` reasonably assumes it applies
    // everywhere. A custom formatter opting out silently is the dangerous
    // shape, because the configuration looks correct.
    use stratify::logging::{LineFormatter, SpanScope};

    struct Passthrough;
    impl LineFormatter for Passthrough {
        fn format_line(
            &self,
            writer: &mut dyn std::fmt::Write,
            event: &tracing::Event<'_>,
            _spans: &SpanScope,
        ) -> std::fmt::Result {
            // Render the fields so a secret would appear if unmasked.
            struct Visit<'a>(&'a mut dyn std::fmt::Write);
            impl tracing::field::Visit for Visit<'_> {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    let _ = write!(self.0, "{}={:?} ", field.name(), value);
                }
            }
            let mut visit = Visit(writer);
            event.record(&mut visit);
            Ok(())
        }
    }

    let dir = scratch("customredact");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(Passthrough)
        .redact(["password"])
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!(password = "hunter2", "login");
    });
    handle.flush();
    drop(handle);

    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    // Assert
    assert!(!body.contains("hunter2"), "the secret survived: {body}");
}

#[test]
fn queue_depth_is_reportable_and_drains_to_zero() {
    // Arrange
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        for i in 0..50 {
            tracing::info!(i, "queued");
        }
    });
    handle.flush();

    // Assert: after a drain nothing is outstanding. The value before a flush is
    // deliberately not asserted — it is sampled without a lock and racing the
    // worker would make the test flaky.
    let depth = handle.queue_depth();
    assert_eq!(depth.total(), 0, "a drained queue must be empty");
    assert_eq!(depth.max(), 0);
}

#[test]
fn a_per_sink_filter_narrows_only_that_sink() {
    // Arrange: file at INFO, console narrowed to WARN.
    let dir = scratch("sinkfilter");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .console(ConsoleConfig::default())
        .console_filter("warn")
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!("info only in the file");
        tracing::warn!("warn in both");
    });
    handle.flush();
    drop(handle);

    // Assert: the file sink keeps the INFO the console filter excluded.
    let entry = fs::read_dir(&dir)
        .expect("directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    assert!(body.contains("info only in the file"), "got: {body}");
    assert!(body.contains("warn in both"), "got: {body}");
}

#[test]
fn an_invalid_per_sink_filter_is_a_startup_error() {
    // Arrange: a typo must be reported, not silently ignored.
    let built = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .console(ConsoleConfig::default())
        .console_filter("this is not a filter=???")
        .build();

    // Assert
    assert!(
        built.map(|_| ()).is_err(),
        "a bad directive must fail the build"
    );
}

#[test]
fn a_file_filter_applies_to_a_custom_file_formatter_too() {
    // Arrange: the custom-text file sink is built by a different path from the
    // built-in one, so it can silently miss configuration the caller set. A
    // filter that is accepted and then ignored is the dangerous shape: the
    // configuration reads correctly and the sink records what it excluded.
    use stratify::logging::{LineFormatter, SpanScope};

    struct Plain;
    impl LineFormatter for Plain {
        fn format_line(
            &self,
            writer: &mut dyn std::fmt::Write,
            event: &tracing::Event<'_>,
            _spans: &SpanScope,
        ) -> std::fmt::Result {
            write!(writer, "[{}]", event.metadata().level())
        }
    }

    let dir = scratch("filefilter-custom");
    let _ = fs::remove_dir_all(&dir);
    let (subscriber, handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .file(FileConfig::new(dir.to_string_lossy().to_string()).with_format(FileFormat::Text))
        .file_format(Plain)
        .file_filter("warn")
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!("excluded by the file filter");
        tracing::warn!("kept by the file filter");
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
        body.contains("[WARN]"),
        "the WARN line must survive: {body}"
    );
    assert!(
        !body.contains("[INFO]"),
        "the file filter must apply to a custom formatter: {body}"
    );
}
