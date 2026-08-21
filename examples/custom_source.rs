//! Implementing `Source` for a backend this crate does not ship.
//!
//! ```bash
//! cargo run --example custom_source
//! ```

use async_trait::async_trait;
use serde_json::{json, Value};
use stratify::config::{Builder, Error, Source};

/// A source backed by something that is not a file: a database, an HTTP
/// endpoint, a secret store. Here it is a canned value, but the shape is the
/// same, and `load` being async is what lets a real one do I/O without blocking.
struct InMemorySource {
    priority: u32,
}

#[async_trait]
impl Source for InMemorySource {
    fn name(&self) -> &str {
        "in-memory"
    }

    fn priority(&self) -> u32 {
        self.priority
    }

    async fn load(&self) -> Result<Value, Error> {
        // A real implementation would await a query here.
        Ok(json!({
            "database": { "host": "from-custom-source" },
            "feature_flags": { "new_ui": "true" }
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Builder::default()
        .json("examples/config/base.json", 100)
        .source(InMemorySource { priority: 20 })
        .build()
        .await?;

    println!(
        "database.host        = {:?}",
        store.get_str("database.host")
    );
    println!(
        "database.port        = {:?}",
        store.get_str("database.port")
    );
    println!(
        "feature_flags.new_ui = {:?}",
        store.get_str("feature_flags.new_ui")
    );

    // The custom source outranks the JSON for the key they share, and
    // contributes a key the JSON never had. Both are just deep merging.
    Ok(())
}
