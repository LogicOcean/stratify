//! Azure App Configuration source.
//!
//! Reads key-values from an App Configuration store over its REST API,
//! authenticating with an Entra ID token. Available under the `azure` feature.

use crate::error::ConfigError;
use crate::source::{nesting, Source};
use async_trait::async_trait;
use azure_core::credentials::TokenCredential;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Scope requested for the App Configuration data plane.
const SCOPE: &str = "https://azconfig.io/.default";

/// REST API version. Pinned deliberately: a floating version would change the
/// response shape underneath a released crate.
const API_VERSION: &str = "1.0";

/// Separator App Configuration conventionally uses for hierarchy, following the
/// .NET configuration convention (`Database:Host`).
const DEFAULT_SEPARATOR: &str = ":";

/// One key-value as returned by the App Configuration API.
#[derive(Deserialize)]
struct KeyValue {
    key: String,
    /// Absent for a key with no value, which the API permits.
    #[serde(default)]
    value: Option<String>,
}

/// A page of key-values, with an optional link to the next.
#[derive(Deserialize)]
struct KeyValuePage {
    #[serde(default)]
    items: Vec<KeyValue>,
    /// App Configuration paginates; this is the continuation link.
    #[serde(rename = "@nextLink", default)]
    next_link: Option<String>,
}

/// Azure App Configuration source.
///
/// Fetches every key-value in the store, optionally filtered to one label, and
/// maps the keys into nested configuration using the same rules as
/// [`EnvSource`](crate::source::EnvSource) — `Database:Host` becomes
/// `{"database": {"host": ...}}`.
///
/// # Credentials
///
/// The credential is supplied by the caller rather than chosen here, so that a
/// service can use a managed identity in Azure and a developer credential
/// locally without this crate deciding for it. Any
/// [`TokenCredential`] implementation works, which also makes the source
/// testable against a fake.
///
/// ```rust,no_run
/// # async fn _example() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
/// use stratify::source::AzureAppConfigSource;
/// // `azure_identity::ManagedIdentityCredential` in Azure,
/// // `azure_identity::DeveloperToolsCredential` on a workstation.
/// # let credential: Arc<dyn azure_core::credentials::TokenCredential> = todo!();
/// let source = AzureAppConfigSource::new(
///     "https://my-store.azconfig.io",
///     credential,
///     10,
/// )
/// .with_label("production");
/// # Ok(()) }
/// ```
pub struct AzureAppConfigSource {
    endpoint: String,
    credential: Arc<dyn TokenCredential>,
    label: Option<String>,
    separator: String,
    priority: u32,
    client: reqwest::Client,
}

impl AzureAppConfigSource {
    /// Create a source for an App Configuration store.
    ///
    /// `endpoint` is the store URL, for example `https://my-store.azconfig.io`.
    /// A trailing slash is accepted and normalised away.
    ///
    /// `priority` follows the crate convention: lower numbers win.
    pub fn new(
        endpoint: impl Into<String>,
        credential: Arc<dyn TokenCredential>,
        priority: u32,
    ) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            credential,
            label: None,
            separator: DEFAULT_SEPARATOR.to_string(),
            priority,
            client: reqwest::Client::new(),
        }
    }

    /// Restrict the fetch to a single label, such as an environment name.
    ///
    /// Without this, every key-value in the store is fetched regardless of
    /// label, and a key that exists under several labels resolves
    /// unpredictably. Setting a label is the usual intent.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Override the hierarchy separator. Defaults to `:`.
    pub fn with_separator(mut self, separator: impl Into<String>) -> Self {
        self.separator = separator.into();
        self
    }

    /// Supply a pre-built HTTP client, for connection pooling or custom timeouts.
    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    /// The URL for the first page of key-values.
    fn first_page_url(&self) -> String {
        match &self.label {
            Some(label) => format!(
                "{}/kv?api-version={}&label={}",
                self.endpoint,
                API_VERSION,
                urlencode(label)
            ),
            None => format!("{}/kv?api-version={}", self.endpoint, API_VERSION),
        }
    }

    /// Fetch every page, following `@nextLink` until exhausted.
    async fn fetch_all(&self, token: &str) -> Result<Vec<KeyValue>, ConfigError> {
        let mut url = self.first_page_url();
        let mut collected = Vec::new();

        loop {
            let response = self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| {
                    ConfigError::Other(format!("App Configuration request failed: {e}"))
                })?;

            let status = response.status();
            if !status.is_success() {
                // The body often carries the actionable detail (wrong scope,
                // missing role assignment), so it is worth surfacing.
                let body = response.text().await.unwrap_or_default();
                return Err(ConfigError::Other(format!(
                    "App Configuration returned {status}: {}",
                    body.trim()
                )));
            }

            let page: KeyValuePage = response.json().await.map_err(|e| {
                ConfigError::Other(format!(
                    "App Configuration response was not valid JSON: {e}"
                ))
            })?;
            collected.extend(page.items);

            match page.next_link {
                // `@nextLink` is relative to the store endpoint.
                Some(link) if !link.is_empty() => {
                    url = if link.starts_with("http") {
                        link
                    } else {
                        format!("{}{}", self.endpoint, link)
                    };
                }
                _ => break,
            }
        }

        Ok(collected)
    }

    /// Convert fetched key-values into the flat, dot-delimited form the shared
    /// nesting helper expects.
    fn to_flat(&self, items: Vec<KeyValue>) -> HashMap<String, String> {
        items
            .into_iter()
            .filter_map(|kv| kv.value.map(|value| (kv.key, value)))
            .map(|(key, value)| (key.to_lowercase().replace(&self.separator, "."), value))
            .collect()
    }
}

/// Percent-encode the characters that can appear in a label and break a query.
///
/// Deliberately minimal rather than pulling a URL crate in for one parameter.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[async_trait]
impl Source for AzureAppConfigSource {
    fn name(&self) -> &str {
        "azure_app_config"
    }

    fn priority(&self) -> u32 {
        self.priority
    }

    async fn load(&self) -> Result<Value, ConfigError> {
        let token = self
            .credential
            .get_token(&[SCOPE], None)
            .await
            .map_err(|e| {
                ConfigError::Other(format!(
                    "could not acquire an Entra ID token for {SCOPE}: {e}"
                ))
            })?;

        let items = self.fetch_all(token.token.secret()).await?;
        tracing::debug!(
            endpoint = %self.endpoint,
            label = ?self.label,
            count = items.len(),
            "loaded key-values from Azure App Configuration"
        );
        nesting::dot_keys_to_json(self.to_flat(items).iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use azure_core::credentials::{AccessToken, Secret};
    use azure_core::time::OffsetDateTime;

    /// A credential that hands back a fixed token without contacting Entra ID.
    #[derive(Debug)]
    struct FakeCredential;

    #[async_trait]
    impl TokenCredential for FakeCredential {
        async fn get_token(
            &self,
            _scopes: &[&str],
            _options: Option<azure_core::credentials::TokenRequestOptions<'_>>,
        ) -> azure_core::Result<AccessToken> {
            Ok(AccessToken {
                token: Secret::new("fake-token".to_string()),
                expires_on: OffsetDateTime::now_utc(),
            })
        }
    }

    fn source() -> AzureAppConfigSource {
        AzureAppConfigSource::new("https://example.azconfig.io", Arc::new(FakeCredential), 10)
    }

    #[test]
    fn a_trailing_slash_on_the_endpoint_is_normalised() {
        // Arrange / Act
        let source =
            AzureAppConfigSource::new("https://example.azconfig.io/", Arc::new(FakeCredential), 10);

        // Assert: otherwise every request URL would contain a double slash.
        assert_eq!(source.endpoint, "https://example.azconfig.io");
    }

    #[test]
    fn the_first_page_url_omits_the_label_when_unset() {
        // Arrange / Act
        let url = source().first_page_url();

        // Assert
        assert_eq!(url, "https://example.azconfig.io/kv?api-version=1.0");
    }

    #[test]
    fn the_first_page_url_includes_an_encoded_label() {
        // Arrange: labels can contain characters that would break the query.
        let url = source().with_label("prod env").first_page_url();

        // Assert
        assert_eq!(
            url,
            "https://example.azconfig.io/kv?api-version=1.0&label=prod%20env"
        );
    }

    #[test]
    fn keys_are_lowercased_and_split_on_the_separator() {
        // Arrange
        let items = vec![
            KeyValue {
                key: "Database:Host".to_string(),
                value: Some("pg.internal".to_string()),
            },
            KeyValue {
                key: "Database:Port".to_string(),
                value: Some("5432".to_string()),
            },
        ];

        // Act
        let flat = source().to_flat(items);

        // Assert
        assert_eq!(
            flat.get("database.host").map(String::as_str),
            Some("pg.internal")
        );
        assert_eq!(flat.get("database.port").map(String::as_str), Some("5432"));
    }

    #[test]
    fn a_custom_separator_is_honoured() {
        // Arrange
        let items = vec![KeyValue {
            key: "db__host".to_string(),
            value: Some("pg".to_string()),
        }];

        // Act
        let flat = source().with_separator("__").to_flat(items);

        // Assert
        assert_eq!(flat.get("db.host").map(String::as_str), Some("pg"));
    }

    #[test]
    fn a_key_with_no_value_is_skipped() {
        // Arrange: the API permits a key-value whose value is null.
        let items = vec![
            KeyValue {
                key: "present".to_string(),
                value: Some("yes".to_string()),
            },
            KeyValue {
                key: "absent".to_string(),
                value: None,
            },
        ];

        // Act
        let flat = source().to_flat(items);

        // Assert
        assert_eq!(flat.len(), 1);
        assert!(!flat.contains_key("absent"));
    }

    #[test]
    fn source_metadata_matches_the_trait_contract() {
        // Arrange / Act
        let source = source();

        // Assert
        assert_eq!(source.name(), "azure_app_config");
        assert_eq!(source.priority(), 10);
    }

    #[test]
    fn urlencode_escapes_reserved_characters() {
        // Arrange / Act / Assert
        assert_eq!(urlencode("plain"), "plain");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a/b"), "a%2Fb");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("keep-_.~"), "keep-_.~");
    }

    #[test]
    fn a_page_deserialises_from_the_documented_shape() {
        // Arrange: the response shape this source depends on.
        let body = r#"{
            "items": [
                {"key": "App:Name", "value": "nse", "label": "prod"},
                {"key": "App:Empty", "value": null, "label": null}
            ],
            "@nextLink": "/kv?api-version=1.0&after=xyz"
        }"#;

        // Act
        let page: KeyValuePage = serde_json::from_str(body).expect("the documented shape parses");

        // Assert
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].key, "App:Name");
        assert_eq!(page.items[0].value.as_deref(), Some("nse"));
        assert!(page.items[1].value.is_none());
        assert_eq!(
            page.next_link.as_deref(),
            Some("/kv?api-version=1.0&after=xyz")
        );
    }

    #[test]
    fn a_final_page_has_no_next_link() {
        // Arrange
        let body = r#"{"items": []}"#;

        // Act
        let page: KeyValuePage = serde_json::from_str(body).expect("parses");

        // Assert
        assert!(page.items.is_empty());
        assert!(page.next_link.is_none());
    }

    #[tokio::test]
    async fn load_surfaces_a_transport_failure_rather_than_panicking() {
        // Arrange: an endpoint that cannot resolve.
        let source =
            AzureAppConfigSource::new("https://nonexistent.invalid", Arc::new(FakeCredential), 10);

        // Act
        let result = source.load().await;

        // Assert
        assert!(matches!(result, Err(ConfigError::Other(_))));
    }
}
