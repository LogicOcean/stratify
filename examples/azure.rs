//! Reading from Azure App Configuration.
//!
//! Requires the `azure` feature and a real store:
//!
//! ```bash
//! export APP_CONFIG_ENDPOINT=https://your-store.azconfig.io
//! cargo run --example azure --features azure
//! ```
//!
//! The credential is chosen here, by the caller, rather than by the crate.
//! `DeveloperToolsCredential` picks up an `az login` session on a workstation;
//! in Azure you would use `ManagedIdentityCredential` and distribute no secret
//! at all.

use azure_core::credentials::TokenCredential;
use std::sync::Arc;
use stratify::source::AzureAppConfigSource;
use stratify::ConfigBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::var("APP_CONFIG_ENDPOINT")
        .map_err(|_| "set APP_CONFIG_ENDPOINT to your App Configuration store URL")?;

    // `new` already returns an `Arc<Self>`, so there is nothing to wrap.
    let credential = azure_identity::DeveloperToolsCredential::new(None)?;

    // Constructed directly rather than via `ConfigBuilder::azure` so the label
    // filter can be set. Without a label, every label in the store is fetched
    // and a key present under several resolves unpredictably.
    let azure = AzureAppConfigSource::new(endpoint, credential as Arc<dyn TokenCredential>, 10)
        .with_label("production");

    let store = ConfigBuilder::default()
        .json("examples/config/base.json", 100)
        .source(azure)
        .build()
        .await?;

    println!("{}", serde_json::to_string_pretty(&store.full_config())?);
    Ok(())
}
