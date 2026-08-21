//! Azure App Configuration source.
//!
//! Reads key-values from an App Configuration store over its REST API,
//! authenticating with an Entra ID token. Available under the `azure` feature.

use crate::config::error::Error;
use crate::config::source::{nesting, Source};
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
    /// Distinguishes a plain value from a Key Vault reference.
    #[serde(default)]
    content_type: Option<String>,
}

/// The content type App Configuration stamps on a Key Vault reference.
///
/// Matched as a prefix because the store appends `;charset=utf-8`.
const KEY_VAULT_REF_CONTENT_TYPE: &str = "application/vnd.microsoft.appconfig.keyvaultref+json";

/// Scope requested for the Key Vault data plane.
const KEY_VAULT_SCOPE: &str = "https://vault.azure.net/.default";

/// Key Vault REST API version for secret reads. Pinned for the same reason as
/// [`API_VERSION`].
const KEY_VAULT_API_VERSION: &str = "7.4";

/// The body of a Key Vault reference: `{"uri": "https://…/secrets/name"}`.
#[derive(Deserialize)]
struct KeyVaultReference {
    uri: String,
}

/// The part of a Key Vault secret response this source reads.
#[derive(Deserialize)]
struct KeyVaultSecret {
    value: String,
}

/// Whether an item is a Key Vault reference rather than a plain value.
fn is_key_vault_ref(content_type: Option<&str>) -> bool {
    content_type.is_some_and(|ct| ct.starts_with(KEY_VAULT_REF_CONTENT_TYPE))
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
/// [`EnvSource`](crate::config::source::EnvSource) — `Database:Host` becomes
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
/// use stratify::config::source::AzureAppConfigSource;
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
    resolve_key_vault: bool,
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
            resolve_key_vault: false,
        }
    }

    /// Resolve Key Vault references into the secrets they point at.
    ///
    /// App Configuration can hold a *reference* to a Key Vault secret instead
    /// of a value — the common enterprise setup, because the secret then lives
    /// under Key Vault's access control and rotation. Such a key's value is a
    /// JSON envelope naming the secret's URI, not the secret.
    ///
    /// Off by default, deliberately: resolving widens this source's reach from
    /// one App Configuration store to every vault the references name, and a
    /// scope that grows silently is how a config library ends up reading
    /// secrets nobody asked it to. With this off, *encountering* a reference
    /// is an error naming the key — loud rather than a JSON envelope
    /// masquerading as a configuration value.
    ///
    /// Resolution reuses the credential this source was built with, so the
    /// identity needs the `Key Vault Secrets User` role (or equivalent) on
    /// each referenced vault.
    pub fn with_key_vault_resolution(mut self) -> Self {
        self.resolve_key_vault = true;
        self
    }

    /// Fetch one referenced secret from Key Vault.
    async fn resolve_secret(
        &self,
        key: &str,
        envelope: &str,
        token: &str,
    ) -> Result<String, Error> {
        let reference: KeyVaultReference = serde_json::from_str(envelope).map_err(|e| {
            Error::Other(format!(
                "App Configuration key {key:?} is a Key Vault reference with an unreadable envelope: {e}"
            ))
        })?;

        // Built by hand: this reqwest is compiled with minimal features, and
        // both parts of the query string are constants with nothing to encode.
        let separator = if reference.uri.contains('?') {
            '&'
        } else {
            '?'
        };
        let url = format!(
            "{}{}api-version={}",
            reference.uri, separator, KEY_VAULT_API_VERSION
        );
        let response = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "Key Vault request for {key:?} ({}) failed: {e}",
                    reference.uri
                ))
            })?;

        let status = response.status();
        if !status.is_success() {
            // As with App Configuration, the body carries the actionable
            // detail: a missing role assignment, a disabled secret.
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "Key Vault returned {status} for {key:?} ({}): {}",
                reference.uri,
                body.trim()
            )));
        }

        let secret: KeyVaultSecret = response.json().await.map_err(|e| {
            Error::Other(format!(
                "Key Vault response for {key:?} was not a secret: {e}"
            ))
        })?;
        Ok(secret.value)
    }

    /// Replace every Key Vault reference in `items` with its secret, or reject
    /// the first reference when resolution is off.
    async fn resolve_references(&self, items: &mut [KeyValue]) -> Result<(), Error> {
        if !items
            .iter()
            .any(|kv| is_key_vault_ref(kv.content_type.as_deref()))
        {
            return Ok(());
        }

        if !self.resolve_key_vault {
            let key = items
                .iter()
                .find(|kv| is_key_vault_ref(kv.content_type.as_deref()))
                .map(|kv| kv.key.clone())
                .unwrap_or_default();
            return Err(Error::Other(format!(
                "App Configuration key {key:?} is a Key Vault reference, and resolution \
                 is not enabled on this source; call with_key_vault_resolution() to \
                 resolve references, or store the value directly"
            )));
        }

        // One vault token covers every reference: the scope is the data
        // plane, not a single vault.
        let token = self
            .credential
            .get_token(&[KEY_VAULT_SCOPE], None)
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "could not acquire an Entra ID token for {KEY_VAULT_SCOPE}: {e}"
                ))
            })?;

        for kv in items
            .iter_mut()
            .filter(|kv| is_key_vault_ref(kv.content_type.as_deref()))
        {
            let Some(envelope) = kv.value.clone() else {
                continue;
            };
            kv.value = Some(
                self.resolve_secret(&kv.key, &envelope, token.token.secret())
                    .await?,
            );
        }
        Ok(())
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
    async fn fetch_all(&self, token: &str) -> Result<Vec<KeyValue>, Error> {
        let mut url = self.first_page_url();
        let mut collected = Vec::new();

        loop {
            let response = self
                .client
                .get(&url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| Error::Other(format!("App Configuration request failed: {e}")))?;

            let status = response.status();
            if !status.is_success() {
                // The body often carries the actionable detail (wrong scope,
                // missing role assignment), so it is worth surfacing.
                let body = response.text().await.unwrap_or_default();
                return Err(Error::Other(format!(
                    "App Configuration returned {status}: {}",
                    body.trim()
                )));
            }

            let page: KeyValuePage = response.json().await.map_err(|e| {
                Error::Other(format!(
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

    async fn load(&self) -> Result<Value, Error> {
        let token = self
            .credential
            .get_token(&[SCOPE], None)
            .await
            .map_err(|e| {
                Error::Other(format!(
                    "could not acquire an Entra ID token for {SCOPE}: {e}"
                ))
            })?;

        let mut items = self.fetch_all(token.token.secret()).await?;
        self.resolve_references(&mut items).await?;
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
                content_type: None,
            },
            KeyValue {
                key: "Database:Port".to_string(),
                value: Some("5432".to_string()),
                content_type: None,
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
            content_type: None,
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
                content_type: None,
            },
            KeyValue {
                key: "absent".to_string(),
                value: None,
                content_type: None,
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

    #[test]
    fn key_vault_refs_are_recognized_by_content_type_prefix() {
        // Arrange / Act / Assert: the store appends a charset suffix, so the
        // match must be a prefix, and anything else must not match.
        assert!(is_key_vault_ref(Some(
            "application/vnd.microsoft.appconfig.keyvaultref+json;charset=utf-8"
        )));
        assert!(is_key_vault_ref(Some(
            "application/vnd.microsoft.appconfig.keyvaultref+json"
        )));
        assert!(!is_key_vault_ref(Some("application/json")));
        assert!(!is_key_vault_ref(None));
    }

    #[tokio::test]
    async fn a_reference_with_resolution_off_is_an_error_naming_the_key() {
        // Arrange: resolution never enabled, one reference among plain values.
        let source =
            AzureAppConfigSource::new("https://example.invalid", Arc::new(FakeCredential), 10);
        let mut items = vec![
            KeyValue {
                key: "Database:Host".to_string(),
                value: Some("db.internal".to_string()),
                content_type: None,
            },
            KeyValue {
                key: "Database:Password".to_string(),
                value: Some(r#"{"uri":"https://v.vault.azure.net/secrets/db-pw"}"#.to_string()),
                content_type: Some(
                    "application/vnd.microsoft.appconfig.keyvaultref+json;charset=utf-8"
                        .to_string(),
                ),
            },
        ];

        // Act
        let result = source.resolve_references(&mut items).await;

        // Assert: loud, names the key, and points at the switch — a JSON
        // envelope must never masquerade as a configuration value.
        match result {
            Err(Error::Other(message)) => {
                assert!(message.contains("Database:Password"), "got: {message}");
                assert!(
                    message.contains("with_key_vault_resolution"),
                    "got: {message}"
                );
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn plain_values_pass_resolution_untouched() {
        // Arrange: no references at all; resolution must be a no-op that
        // performs no I/O and asks for no token.
        let source =
            AzureAppConfigSource::new("https://example.invalid", Arc::new(FakeCredential), 10);
        let mut items = vec![KeyValue {
            key: "Database:Host".to_string(),
            value: Some("db.internal".to_string()),
            content_type: Some("application/json".to_string()),
        }];

        // Act
        let result = source.resolve_references(&mut items).await;

        // Assert
        assert!(result.is_ok());
        assert_eq!(items[0].value.as_deref(), Some("db.internal"));
    }

    #[tokio::test]
    async fn an_unreadable_reference_envelope_is_an_error_naming_the_key() {
        // Arrange: resolution on, but the envelope is not the JSON App
        // Configuration writes. Fails at the parse, before any network.
        let source =
            AzureAppConfigSource::new("https://example.invalid", Arc::new(FakeCredential), 10)
                .with_key_vault_resolution();
        let mut items = vec![KeyValue {
            key: "Database:Password".to_string(),
            value: Some("not-json".to_string()),
            content_type: Some("application/vnd.microsoft.appconfig.keyvaultref+json".to_string()),
        }];

        // Act
        let result = source.resolve_references(&mut items).await;

        // Assert
        match result {
            Err(Error::Other(message)) => {
                assert!(message.contains("Database:Password"), "got: {message}");
                assert!(message.contains("envelope"), "got: {message}");
            }
            other => panic!("expected an envelope error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn load_surfaces_a_transport_failure_rather_than_panicking() {
        // Arrange: an endpoint that cannot resolve.
        let source =
            AzureAppConfigSource::new("https://nonexistent.invalid", Arc::new(FakeCredential), 10);

        // Act
        let result = source.load().await;

        // Assert
        assert!(matches!(result, Err(Error::Other(_))));
    }
}
