//! Why lower numbers win, shown rather than described.
//!
//! ```bash
//! cargo run --example precedence
//! ```

use stratify::ConfigBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Three sources all defining database.host. The TOML file has the lowest
    // priority number, so it is the one that wins.
    let store = ConfigBuilder::default()
        .json("examples/config/base.json", 100) // host = localhost
        .yaml("examples/config/production.yaml", 50) // host = db.prod.internal
        .toml("examples/config/app.toml", 10) // host = db.toml.internal
        .build()
        .await?;

    println!(
        "winner: database.host = {:?}",
        store.get_str("database.host")
    );
    assert_eq!(
        store.get_str("database.host").as_deref(),
        Some("db.toml.internal")
    );

    // The point of the inverted ordering: adding a more specific source later
    // means giving it a smaller number, not renumbering everything already
    // there. Think ranking, not weight.
    println!(
        "app.workers survives from the base file: {:?}",
        store.get_str("app.workers")
    );
    Ok(())
}
