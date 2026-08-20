# stratify

[![crates.io](https://img.shields.io/crates/v/stratify.svg)](https://crates.io/crates/stratify)
[![docs.rs](https://docs.rs/stratify/badge.svg)](https://docs.rs/stratify)
[![CI](https://github.com/LogicOcean/stratify/actions/workflows/ci.yml/badge.svg)](https://github.com/LogicOcean/stratify/actions/workflows/ci.yml)
[![MIT licensed](https://img.shields.io/crates/l/stratify.svg)](./LICENSE)

Layered configuration for Rust — pluggable sources, priority merging, typed access.

Stack configuration from files, environment variables and Azure App Configuration,
merge them by declared precedence, and read the result as typed values.

```rust
use stratify::ConfigBuilder;

let store = ConfigBuilder::default()
    .json("config/base.json", 100)
    .yaml("config/override.yaml", 50)
    .env("APP_", "__", 10)
    .build()
    .await?;

let host: String = store.get_str("database.host").unwrap();
let port: u64 = store.get_u64("database.port").unwrap();
```

## Precedence

**Lower priority number wins.** A source at priority 10 overrides one at 100.

That is the opposite of "higher number is more important", and it is deliberate: it
lets you add a more specific source later without renumbering the ones already there.
Think of it as a ranking, not a weight.

Merging is deep. Nested objects combine key by key rather than the higher-precedence
source replacing the whole subtree.

## Sources

| Source | Builder method | Notes |
| ------ | -------------- | ----- |
| JSON | `.json(path, priority)` | |
| YAML | `.yaml(path, priority)` | |
| TOML | `.toml(path, priority)` | |
| Env | `.env(prefix, separator, priority)` | `APP_DB__HOST` → `{"db": {"host": …}}` |
| DotEnv | `.dotenv(path, prefix, separator, priority)` | loads the file, then reads matching vars |
| Azure App Configuration | `.azure(endpoint, credential, priority)` | requires the `azure` feature |

Implement [`Source`] for anything else — a database, a secret store, an HTTP endpoint.

## Azure App Configuration

```toml
[dependencies]
stratify = { version = "0.3", features = ["azure"] }
azure_identity = "1"
```

```rust
use std::sync::Arc;
use azure_identity::ManagedIdentityCredential;
use stratify::ConfigBuilder;

let credential = Arc::new(ManagedIdentityCredential::new(None)?);

let store = ConfigBuilder::default()
    .json("config/base.json", 100)
    .azure("https://my-store.azconfig.io", credential, 10)
    .build()
    .await?;
```

The credential is yours to choose rather than something this crate decides. Use
`ManagedIdentityCredential` in Azure and `DeveloperToolsCredential` on a workstation,
and no secret has to be distributed either way. Any `TokenCredential` works, which also
means the source can be tested against a fake.

Keys follow the .NET convention: `Database:Host` becomes `{"database": {"host": …}}`.
Filter to one label with `AzureAppConfigSource::with_label`, which you will usually
want — without it, every label in the store is fetched and a key present under several
resolves unpredictably.

## Typed access

```rust
#[derive(serde::Deserialize)]
struct Database {
    host: String,
    port: u16,
}

let db: Database = store.get("database")?;
```

## Refresh

`store.refresh().await?` re-reads every source and re-merges, without recreating the
store or restarting the process. The cache is left untouched if any source fails, so a
transient outage does not blank your configuration.

Reads (`get_str`, `get`, …) are synchronous and lock-free on the happy path; only
loading and refreshing are async.

## Feature flags

| Feature | Default | Effect |
| ------- | :-----: | ------ |
| `azure` | no | Azure App Configuration source; pulls `azure_core` and `reqwest` |

## Versioning

`0.3` made `Source::load` async, which is a breaking change from `0.2`. Sources now
declare `#[async_trait]` and `build`/`refresh` are awaited. The change exists so that
network-backed sources do not have to block a runtime thread.

## Examples and design notes

Runnable examples live in [`examples/`](./examples/) and are compiled and run by
CI, so they cannot drift from the API:

```bash
cargo run --example basic
cargo run --example precedence
```

[`docs/design.md`](./docs/design.md) covers why the priority ordering is
inverted, why merging is deep, why `Source::load` is async, and why the Azure
credential is supplied by the caller.

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md). Pull requests are welcome; the review
focus and the local commands CI mirrors are both documented there.

Security issues go through [private vulnerability reporting](./SECURITY.md)
rather than a public issue.

## Supply chain

Actions are pinned to commit SHAs, `cargo-deny` gates advisories, licences,
wildcard versions and dependency sources, git and unknown-registry dependencies
are denied, and the crate is `#![forbid(unsafe_code)]`. See [SECURITY.md](./SECURITY.md).

A consumer takes 33 crates by default, or 145 with `azure` enabled — which is
why the Azure source is behind a feature flag rather than always on.

## License

MIT — see [LICENSE](./LICENSE).
