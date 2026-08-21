# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
## [1.1.0] — 2026-08-21

### Added

- `stratify::logging::tracing`, re-exporting the instrumentation API whole.
  `tracing` is how code emits — spans, events, `info!` — and this crate is how
  those records leave the process; re-exporting the former from the latter
  lets a service depend on stratify alone, with both halves guaranteed to
  agree on a version. `use stratify::logging::tracing;` at the top of a module
  keeps every existing `tracing::info!(...)` callsite compiling unchanged.
  The convention of depending on `tracing` directly remains available; this is
  for consumers who want one logging dependency, in the same spirit as the
  existing `EnvFilter` re-export.

## [1.0.1] — 2026-08-21

### Fixed

- Span fields no longer carry ANSI escape codes into files or colourless
  consoles. The escape codes were coming from span-field *caching*: the
  layer's own ANSI flag — which defaulted on — decides how a span's fields are
  rendered into the cache every event then prints verbatim, and the sinks only
  ever set the flag on the event format. Found live: a web service's
  per-request span (`http_request{method=…}`) arrived in its log file wrapped
  in italic/dim codes with colour configured off everywhere.

  The file sinks also get their own cache now (a distinct field-formatter
  type), so a *coloured* console beside a text file cannot bleed escape codes
  into it either — previously the two shared one cache and whichever layer
  formatted a span first decided what the other printed, which is why services
  had to switch console colour off to protect their files. Both regressions
  are pinned by tests that fail against the previous release.

## [1.0.0] — 2026-08-21

The stability release. From here, a breaking change to anything public is a
major version, so the 0.4-era caveat about minor bumps is retired along with
the number.

stratify absorbs the unpublished `loggingkit` crate. One crate, two namespaces:
`stratify::config` is the configuration half, `stratify::logging` is a
non-blocking `tracing` facade, and `stratify::init` stands both up in one call.
The logging half is behind the `logging` feature and a config-only build
compiles none of it — CI proves that on every push by failing if the default
dependency tree contains `tracing-subscriber`, `tracing-appender` or any
`opentelemetry` crate.

### Changed

- **Breaking:** the config API moved from the crate root into
  `stratify::config`, and the types shed their prefixes now that the module
  carries the name: `ConfigBuilder` → `config::Builder`, `ConfigError` →
  `config::Error`, `ConfigStore` → `config::Store`. `Source` and the source
  types live under `config::source`.
- The minimum supported Rust version is now declared: 1.88.0.

### Added

- `stratify::logging` (feature `logging`): console, JSON, file and syslog
  sinks, all non-blocking; per-sink filters; runtime filter reload; sampling
  and rate-limit gates; size- and time-based file rotation with retention;
  custom line formatters; redaction; panic capture; queue-depth and
  dropped-line accounting. Formerly the `loggingkit` facade, imported here
  without its history and with its legacy pre-facade API (`LogBuilder`,
  `LogStore`, the `sink` module) left behind.
- `logging::settings::Settings` (and per-sink `*Settings` blocks), a serde
  schema read from a config [`Store`] with `Settings::from_store`. The logging
  half does no parsing of its own — TOML, YAML, JSON and environment layering
  are the config half's job, in one place. The old `from_file`, and the `toml`
  dependency it carried, are gone.
- `stratify::init` and `init_with` (feature `logging`): read configuration
  (file < environment < `.env`), build logging from its `[logging]` block,
  install the subscriber, and return the `Bootstrap` pair. The first record
  the subscriber carries names the sources that resolved, so a wrong
  precedence stack is visible instead of silent.
- Features `appinsights` (Azure Application Insights export with trace
  correlation) and `compression` (gzip retired log files), both implying
  `logging`.
- Full settings parity: `[logging.filters]` (per-sink directives), `redact`,
  `capture_panics`, `[logging.global_fields]`, `[logging.syslog]` and
  `[logging.app_insights]` are all expressible in configuration, so "described
  rather than coded" covers the whole builder surface. The `app_insights`
  block names the *key* the connection string is found under rather than
  holding the value, so the file stays safe to commit; an explicit block whose
  secret is missing is a startup error, not a silently absent exporter. A bad
  `level`, an unknown syslog facility, or a block needing a feature that is
  not compiled in all fail loudly at startup naming the offending key —
  `Settings::to_builder` is fallible now for exactly that reason.
- `AppInsightsConfig::with_sample_rate`, bounding the fraction of traces
  exported (`0.0..=1.0`, parent-based so a trace is kept or dropped whole).
  Exporting every span is an Application Insights bill that grows linearly
  with traffic; this is the knob. Log records are not sampled — per-sink
  filters are the tool for those. Also settable as `sample_rate` in the
  `[logging.app_insights]` block.
- `init` reads `rust_log` from the *store* when no explicit level is set, so a
  `RUST_LOG` written in `.env` obeys the store's precedence instead of
  silently losing to a shell export — the builder's own fallback reads the
  process environment, which a `.env` loaded as a source never touches.
  Sourced from configuration it is validated strictly: a typo is a startup
  error naming the key.
- `AzureAppConfigSource::with_key_vault_resolution`, resolving Key Vault
  references into the secrets they point at, reusing the source's credential
  against the vault data plane. Off by default because resolution widens the
  source's reach from one store to every vault the references name; with it
  off, *encountering* a reference is an error naming the key — loud, rather
  than a JSON envelope masquerading as a configuration value.
- `stratify::logging::EnvFilter`, re-exported so `with_filter` and
  `reload_filter` can be called without taking `tracing-subscriber` as a
  direct dependency.

## [0.3.1] — 2026-08-20

### Added

- `EnvSource::with_keys` and `ConfigBuilder::env_keys`, capturing exactly the
  environment variables you name rather than everything matching a prefix.

  Some settings are named by convention rather than by application: `RUST_LOG`
  is read by `tracing-subscriber`, `AZURE_STORAGE_ACCOUNT` is what Azure
  injects. No prefix selects those and nothing else, and an empty prefix
  captures the whole environment — `PATH` and every other process's secrets
  along with it, which then sits in the merged configuration waiting to be
  logged.

  Names match case-insensitively and appear lowercased, so `RUST_LOG` is read as
  `rust_log`. The separator still applies.

## [0.3.0] — 2026-08-20

First public release. Continues the unpublished `configkit` under a name that is
available on crates.io and describes the crate rather than padding it.

### Added

- Azure App Configuration source, behind the `azure` feature. The caller supplies
  the credential rather than the crate choosing one, so a service can use a managed
  identity in Azure and a developer credential locally. That keeps `azure_identity`
  out of this crate's dependency tree and lets the source be tested against a fake.
  Supports `@nextLink` pagination, an optional label filter, and the `Database:Host`
  key convention.
- `ConfigBuilder::azure` for the same, in the fluent style.
- `#![forbid(unsafe_code)]`.

### Changed

- **Breaking:** `Source::load` is now `async`. Implementations declare
  `#[async_trait]`, and `ConfigBuilder::build` and `ConfigStore::refresh` are
  awaited. Network-backed sources should not block a runtime thread, and the
  synchronous trait left no way to use an async SDK without blocking inside a
  runtime, which panics.
- Replaced `serde_yaml` with `serde_norway`. `serde_yaml` is officially deprecated
  and should not be a dependency of a published crate.
- `toml` 0.8 to 1.
- Key nesting is shared between the environment and Azure sources rather than
  duplicated in each.

### Migration from `configkit` 0.2

```diff
-let store = ConfigBuilder::default().json("base.json", 100).build()?;
+let store = ConfigBuilder::default().json("base.json", 100).build().await?;
```

For a custom source, add the attribute and the keyword:

```diff
+#[async_trait::async_trait]
 impl Source for MySource {
-    fn load(&self) -> Result<Value, ConfigError> { ... }
+    async fn load(&self) -> Result<Value, ConfigError> { ... }
 }
```

[0.3.1]: https://github.com/LogicOcean/stratify/releases/tag/v0.3.1
[0.3.0]: https://github.com/LogicOcean/stratify/releases/tag/v0.3.0
