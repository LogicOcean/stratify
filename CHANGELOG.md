# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is below 1.0, a minor bump may contain breaking changes.

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
