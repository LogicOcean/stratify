//! Layering a base file, an environment override file, and environment variables.
//!
//! ```bash
//! cargo run --example basic
//! APP_DATABASE__HOST=from-env cargo run --example basic
//! ```

use stratify::config::Builder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Builder::default()
        // Remember: lower number wins. base.json is the fallback, so it gets
        // the highest number.
        .json("examples/config/base.json", 100)
        .yaml("examples/config/production.yaml", 50)
        .env("APP_", "__", 10)
        .build()
        .await?;

    println!("app.name      = {:?}", store.get_str("app.name"));
    println!("database.host = {:?}", store.get_str("database.host"));
    println!("database.port = {:?}", store.get_str("database.port"));
    println!("database.tls  = {:?}", store.get_str("database.tls"));

    // database.host comes from the YAML, which outranks the JSON. database.port
    // is untouched by the YAML and survives from the JSON: the override merged
    // into the base rather than replacing the whole `database` object.
    Ok(())
}
