//! End-to-end coverage for `stratify::init_with`: a real config file, a real
//! store, a real subscriber install, and the handle it returns.
//!
//! Lives in its own integration binary because `init` installs the global
//! subscriber, which can only happen once per process.

#![cfg(feature = "logging")]

use std::fs;
use std::io::Write;

#[tokio::test]
async fn init_reads_config_builds_logging_and_returns_both_halves() {
    // Arrange: a config file carrying both application settings and a
    // [logging] block that writes to a scratch directory.
    let dir = std::env::temp_dir().join("stratify_bootstrap_init_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch directory");
    let log_dir = dir.join("logs");

    let config_path = dir.join("config.toml");
    let mut file = fs::File::create(&config_path).expect("config file");
    write!(
        file,
        r#"
[database]
host = "db.internal"

[logging]
level = "info"

[logging.file]
directory = "{}"
format = "text"
"#,
        log_dir.display()
    )
    .expect("config written");
    drop(file);

    let mut options = stratify::Options::new("bootstrap-test");
    options.config_file = config_path;
    options.dotenv = false;

    // Act
    let boot = stratify::init_with(options)
        .await
        .expect("init succeeds on a valid config");
    tracing::info!("a line for the file sink");
    boot.logging.flush();

    // Assert: the config half answers from the same file the logging half was
    // built from, and the file sink received both the install line and ours.
    assert_eq!(
        boot.config.get_str("database.host").as_deref(),
        Some("db.internal")
    );

    let entry = fs::read_dir(&log_dir)
        .expect("log directory exists")
        .filter_map(Result::ok)
        .next()
        .expect("a log file was created");
    let body = fs::read_to_string(entry.path()).expect("log file readable");
    assert!(
        body.contains("configuration loaded, logging installed"),
        "the install line must be the first record: {body}"
    );
    assert!(body.contains("a line for the file sink"), "got: {body}");

    let _ = fs::remove_dir_all(&dir);
}
