# stratify

[![crates.io](https://img.shields.io/crates/v/stratify.svg)](https://crates.io/crates/stratify)
[![docs.rs](https://docs.rs/stratify/badge.svg)](https://docs.rs/stratify)
[![CI](https://github.com/LogicOcean/stratify/actions/workflows/ci.yml/badge.svg)](https://github.com/LogicOcean/stratify/actions/workflows/ci.yml)
[![MIT licensed](https://img.shields.io/crates/l/stratify.svg)](./LICENSE)

Layered configuration and structured logging for Rust services.

Two halves behind one crate. `stratify::config` stacks configuration from files,
environment variables and Azure App Configuration, merges by declared
precedence, and reads the result as typed values. `stratify::logging` — behind
the `logging` feature, so a config-only build compiles none of it — is a
non-blocking `tracing` facade with console, JSON, file and syslog sinks.
`stratify::init` stands both up in one call.

```rust
use stratify::config::Builder;

let store = Builder::default()
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
| Env (named) | `.env_keys(["RUST_LOG", …], separator, priority)` | for variables with no shared prefix |
| DotEnv | `.dotenv(path, prefix, separator, priority)` | loads the file, then reads matching vars |
| Azure App Configuration | `.azure(endpoint, credential, priority)` | requires the `azure` feature |

Implement [`Source`] for anything else — a database, a secret store, an HTTP endpoint.

## Azure App Configuration

```toml
[dependencies]
stratify = { version = "1", features = ["azure"] }
azure_identity = "1"
```

```rust
use std::sync::Arc;
use azure_identity::ManagedIdentityCredential;
use stratify::config::Builder;

let credential = Arc::new(ManagedIdentityCredential::new(None)?);

let store = Builder::default()
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

A store can hold Key Vault *references* instead of values — the common
enterprise setup. `with_key_vault_resolution()` resolves them into the secrets
they point at, reusing the same credential (the identity needs `Key Vault
Secrets User` on each referenced vault). It is off by default, and encountering
a reference with it off is an error naming the key, never a JSON envelope
handed back as a value.
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

## Logging

```toml
[dependencies]
stratify = { version = "1", features = ["logging"] }
```

```rust
use stratify::logging::{self, ConsoleConfig, FileConfig};

let handle = logging::builder()
    .console(ConsoleConfig::default())
    .file(FileConfig::new("/var/log/myapp"))
    .console_filter("warn")           // per-sink filters
    .reloadable()                     // swap the global filter at runtime
    .init()?;

tracing::info!(request_id = "req-42", "server started");
handle.flush();
```

Every sink is non-blocking: writers flush on background threads, and the handle
reports queue depth and dropped lines so backpressure is visible before it is
fatal. Sinks: console (stderr or stdout), JSON, rotating file (daily, hourly,
or by size, with retention and optional gzip via the `compression` feature),
and syslog. Custom line formatters, field redaction and panic capture are
built in, and `appinsights` adds Azure Application Insights export with trace
correlation.

Logging can also be *described* rather than coded, in the same store as the
rest of your configuration:

```toml
[logging]
level = "info"
redact = ["password", "authorization"]
capture_panics = true

[logging.file]
directory = "/var/log/myapp"
rotation = "daily"

[logging.filters]
app_insights = "info"        # the sink that costs money per event

[logging.app_insights]
service_name = "my-service"
sample_rate = 0.25           # fraction of traces exported, kept-or-dropped whole
# The connection string is a secret: the block names the key it is found
# under (default: applicationinsights_connection_string), and the value
# arrives through the store — environment, .env, or a vault-backed source.
```

```rust
use stratify::logging::settings::Settings;

let builder = Settings::from_store(&store, "logging")?;
```

## One call to start a service

`init` reads configuration (`config.toml` < environment < `.env`), builds
logging from its `[logging]` block, installs the subscriber, and hands back
both halves:

```rust
let boot = stratify::init("my-service").await?;
let db_host = boot.config.get_str("database.host");
tracing::info!("up");
boot.logging.flush();
```

The first record the subscriber carries names the sources that resolved, so a
wrong precedence stack is visible instead of silent. With the `appinsights`
feature, a connection string reachable in the store (conventionally
`APPLICATIONINSIGHTS_CONNECTION_STRING`, injected by the platform) turns the
exporter on; its absence is a choice, not an error.

## Feature flags

| Feature | Default | Effect |
| ------- | :-----: | ------ |
| `azure` | no | Azure App Configuration source; pulls `azure_core` and `reqwest` |
| `logging` | no | `stratify::logging` and `stratify::init`; pulls `tracing-subscriber`, `tracing-appender`, `time` |
| `compression` | no | gzip retired log files (implies `logging`) |
| `appinsights` | no | Azure Application Insights export with trace correlation (implies `logging`) |

A config-only build stays a config library: CI fails if the default dependency
tree ever contains `tracing-subscriber`, `tracing-appender` or any
`opentelemetry` crate.

## Versioning

`1.0` moved the config API from the crate root into `stratify::config`
(`ConfigBuilder` → `config::Builder`, and so on), absorbed the logging half,
and declared the API stable: from here a breaking change is a major version.
`0.3` made `Source::load` async so that network-backed sources do not have to
block a runtime thread.

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
