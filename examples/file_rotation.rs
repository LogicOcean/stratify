//! Hourly file rotation with seven-day retention.
//!
//! Run with `cargo run --example file_rotation`. The example proves startup
//! retention by removing an eight-day-old file, writes one JSON event to an
//! hourly `app.log.*` file, prints its path, and removes the temporary folder.

use std::fs;
use std::time::{Duration, SystemTime};

use stratify::logging::file::Rotation;
use stratify::logging::FileConfig;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!(
        "loggingkit-file-rotation-example-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory)?;

    let stale_file = directory.join("app.log.stale");
    fs::write(&stale_file, b"stale\n")?;
    let eight_days_ago = SystemTime::now()
        .checked_sub(Duration::from_secs(8 * 24 * 60 * 60))
        .ok_or("the system clock is before the unix epoch")?;
    filetime::set_file_mtime(
        &stale_file,
        filetime::FileTime::from_system_time(eight_days_ago),
    )?;

    let file = FileConfig::new(directory.to_string_lossy().into_owned())
        .with_rotation(Rotation::Hourly)
        .with_retention_days(7);

    stratify::logging::builder().file(file).init()?;

    assert!(!stale_file.exists(), "startup retention did not run");

    tracing::info!(job = "example", "hourly file output");
    stratify::logging::flush()?;

    let log_path = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("app.log"))
        })
        .ok_or("no log file was written")?;
    let output = fs::read_to_string(&log_path)?;
    assert!(output.contains("hourly file output"));

    println!("wrote hourly log: {}", log_path.display());
    fs::remove_dir_all(&directory)?;
    Ok(())
}
