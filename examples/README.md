# Examples

Every example here is compiled by CI, so none of them can quietly stop working.
Run them from the repository root, since they read the fixtures in
`examples/config/` by relative path.

| Example | Shows | Run |
| ------- | ----- | --- |
| `basic` | layering a base file, an override file and environment variables | `cargo run --example basic` |
| `precedence` | why the lowest priority number wins, and that merging is deep | `cargo run --example precedence` |
| `typed` | deserialising a section into your own struct | `cargo run --example typed` |
| `custom_source` | implementing `Source` for a backend this crate does not ship | `cargo run --example custom_source` |
| `azure` | reading from Azure App Configuration | `cargo run --example azure --features azure` |

`basic` is worth running twice to see the environment layer take effect:

```bash
cargo run --example basic
APP_DATABASE__HOST=from-env cargo run --example basic
```

`azure` needs the `azure` feature and a real store, so it is excluded from a
default `cargo build --examples` via `required-features`.

## Fixtures

`config/base.json`, `config/production.yaml` and `config/app.toml` deliberately
overlap. `production.yaml` sets only `database.host` and `database.tls`, which is
what makes deep merging visible: `database.port` survives from the JSON rather
than being wiped out along with the rest of the `database` object.
