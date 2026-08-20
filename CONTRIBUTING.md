# Contributing

Thanks for considering it. Bug reports and small focused pull requests are both
welcome.

## Before you open a pull request

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo deny --all-features check
```

CI runs exactly these, plus `cargo doc` with warnings denied, and runs the first
three against both the default feature set and `--all-features`. The `azure`
source is not compiled at all by a default-only build, so a change there needs
`--all-features` to be exercised.

## What review will focus on

Most of a pull request only runs when something calls it. Four things do not, so
they get read closely and are worth flagging in your description if you add one:

- **A new `build.rs`.** Build scripts execute during compilation, on CI and on
  every user's machine. This crate has none and is unlikely to need one.
- **A new proc-macro dependency.** These also run at compile time, and they
  generate code that never appears in the diff.
- **`unsafe`.** The crate sets `#![forbid(unsafe_code)]`, so this is a compile
  error rather than a discussion.
- **A new dependency.** One line in `Cargo.toml` is many thousands of lines in
  reality, plus that crate's own transitive tree. Expect to be asked what it
  buys and whether the standard library or an existing dependency covers it.

An optional dependency moving into the default feature set is a bigger change
than it looks: default is 33 crates, `--all-features` is 145.

## Adding a source

Implement [`Source`]: `name`, `priority`, and an async `load` returning a
`serde_json::Value`. Anything that produces flat delimited keys should reuse the
shared nesting helper rather than reimplementing the expansion, so that
behaviour on conflicting keys stays consistent.

Sources belong in `src/source/`, with unit tests in the same file.

## Style

- Tests are Arrange / Act / Assert, with the sections marked.
- A test name says the scenario and the expectation.
- Comments explain why, not what. If a line needs a comment to say what it does,
  the line is the problem.
