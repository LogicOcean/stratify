//! Shared test support for the Stream A facade tests.
//!
//! Every gating fix in Stream A is only meaningful if it changes what an
//! actual sink receives, so these helpers give the tests a sink they can
//! assert against instead of inspecting builder fields.
#![allow(dead_code)]

use std::fmt;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use stratify::logging::file::Rotation;
use stratify::logging::FileConfig;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// A file sink writing to a single predictable file under `dir`.
pub fn file_config(dir: &Path) -> FileConfig {
    // NEVER keeps the output in one file instead of a date-stamped one.
    FileConfig::new(dir.to_string_lossy().as_ref()).with_rotation(Rotation::Never)
}

/// Every non-empty line written under `dir`, across all rotated files.
pub fn lines_written(dir: &Path) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in fs::read_dir(dir).expect("log directory unreadable") {
        let entry = entry.expect("log directory entry unreadable");
        if entry.path().is_file() {
            let body = fs::read_to_string(entry.path()).expect("log file unreadable");
            lines.extend(body.lines().filter(|l| !l.is_empty()).map(str::to_owned));
        }
    }
    lines
}

/// A single event as it arrived at the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    pub level: Level,
    pub target: String,
    pub message: String,
}

/// A `tracing` layer that records every event it is asked to write.
///
/// Clones share one buffer, so a test can hand one clone to the subscriber and
/// keep another to assert on.
#[derive(Debug, Clone, Default)]
pub struct CaptureLayer {
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

impl CaptureLayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every event that reached the sink, in arrival order.
    pub fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().expect("capture buffer poisoned").clone()
    }

    /// The level of every event that reached the sink, in arrival order.
    pub fn levels(&self) -> Vec<Level> {
        self.events().into_iter().map(|e| e.level).collect()
    }

    /// The `message` field of every event that reached the sink.
    pub fn messages(&self) -> Vec<String> {
        self.events().into_iter().map(|e| e.message).collect()
    }

    pub fn len(&self) -> usize {
        self.events.lock().expect("capture buffer poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut message = MessageVisitor::default();
        event.record(&mut message);

        let meta = event.metadata();
        self.events
            .lock()
            .expect("capture buffer poisoned")
            .push(CapturedEvent {
                level: *meta.level(),
                target: meta.target().to_owned(),
                message: message.0,
            });
    }
}

/// Extracts the implicit `message` field from an event.
#[derive(Default)]
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
        }
    }
}
