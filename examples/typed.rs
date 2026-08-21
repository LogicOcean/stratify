//! Deserialising a section into your own struct.
//!
//! ```bash
//! cargo run --example typed
//! ```

use serde::Deserialize;
use stratify::config::Builder;

#[derive(Debug, Deserialize)]
struct Database {
    host: String,
    // Note the types: file and environment sources both yield strings, so
    // anything non-string needs `deserialize_with` or a String field. This is
    // the sharp edge people hit first.
    port: String,
    tls: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Builder::default()
        .json("examples/config/base.json", 100)
        .yaml("examples/config/production.yaml", 50)
        .build()
        .await?;

    let database: Database = store.get("database")?;
    println!("host = {}", database.host);
    println!("port = {}", database.port);
    println!("tls  = {}", database.tls);

    // A missing section is an error rather than a default, so a typo in the key
    // fails loudly instead of silently handing back an empty struct.
    match store.get::<Database>("databse") {
        Ok(_) => println!("unexpected"),
        Err(e) => println!("typo in the key is an error: {e}"),
    }
    Ok(())
}
