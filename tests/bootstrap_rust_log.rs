//! `init` and `RUST_LOG` through the store, not the process environment.
//!
//! Its own binary because installing the global subscriber happens once per
//! process, and the invalid-directives case must run in the same process
//! without installing anything.

#![cfg(feature = "logging")]

use std::fs;
use std::io::Write;

fn write_config(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    fs::create_dir_all(dir).expect("scratch directory");
    let path = dir.join("config.toml");
    let mut file = fs::File::create(&path).expect("config file");
    file.write_all(body.as_bytes()).expect("config written");
    path
}

#[tokio::test]
async fn rust_log_from_the_store_drives_the_filter_when_no_level_is_set() {
    // Arrange: no [logging].level, but a rust_log key in the same store —
    // which is where a .env-supplied RUST_LOG lands, and which must win the
    // way the store's precedence says, not the way the process environment
    // happens to be.
    let dir = std::env::temp_dir().join("stratify_bootstrap_rust_log_test");
    let _ = fs::remove_dir_all(&dir);
    let log_dir = dir.join("logs");
    let config = write_config(
        &dir,
        &format!(
            r#"
rust_log = "warn"

[logging.file]
directory = "{}"
format = "text"
"#,
            log_dir.display()
        ),
    );

    let mut options = stratify::Options::new("rust-log-test");
    options.config_file = config;
    options.dotenv = false;

    // Act
    let boot = stratify::init_with(options)
        .await
        .expect("init succeeds with rust_log from the store");
    tracing::info!("info that the store filter excludes");
    tracing::warn!("warn that survives");
    boot.logging.flush();

    // Assert
    let entry = fs::read_dir(&log_dir)
        .expect("log directory")
        .filter_map(Result::ok)
        .next()
        .expect("a log file");
    let body = fs::read_to_string(entry.path()).expect("readable");
    let _ = fs::remove_dir_all(&dir);

    assert!(body.contains("warn that survives"), "got: {body}");
    assert!(
        !body.contains("info that the store filter excludes"),
        "rust_log=warn from the store must filter INFO: {body}"
    );
}

#[tokio::test]
async fn an_invalid_rust_log_in_the_store_is_a_startup_error() {
    // Arrange: sourced from configuration, a typo is a startup error naming
    // the key — not the lenient best-effort parse the process-env fallback
    // performs.
    let dir = std::env::temp_dir().join("stratify_bootstrap_bad_rust_log_test");
    let _ = fs::remove_dir_all(&dir);
    let config = write_config(&dir, "rust_log = \"not[a]filter\"\n");

    let mut options = stratify::Options::new("bad-rust-log-test");
    options.config_file = config;
    options.dotenv = false;

    // Act
    let result = stratify::init_with(options).await;
    let _ = fs::remove_dir_all(&dir);

    // Assert: fails before any subscriber is installed, naming the key.
    match result {
        Err(e) => assert!(e.to_string().contains("rust_log"), "got: {e}"),
        Ok(_) => panic!("an unparseable rust_log must be a startup error"),
    }
}
