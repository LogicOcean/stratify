## What and why

<!-- What changes, and what problem it solves. -->

## Supply-chain checklist

Tick anything that applies, so review knows where to look. All-unticked is the
common and expected case.

- [ ] Adds a dependency
- [ ] Adds a `build.rs` or a proc-macro dependency
- [ ] Moves an optional dependency into a default feature
- [ ] Changes a GitHub Actions pin

## Verification

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features -- -D warnings`
- [ ] `cargo test --all-features`
- [ ] `cargo deny --all-features check`

## Breaking changes

<!-- Public API changes, and what a caller has to do. "None" is a fine answer. -->
