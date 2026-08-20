# Security policy

## Reporting a vulnerability

Please report security issues privately through
[GitHub's private vulnerability reporting](https://github.com/LogicOcean/stratify/security/advisories/new)
rather than opening a public issue.

Include what you can: the affected version, what an attacker gains, and a
reproduction if you have one. You will get an acknowledgement within a few days.

## Supported versions

Only the latest published version receives fixes. While the crate is below 1.0,
a fix may arrive as a minor version bump rather than a patch.

## Scope

This crate reads configuration from files, environment variables and Azure App
Configuration, and merges it. Things in scope include leaking secret values into
logs or error messages, an unintended source being able to override a
higher-precedence one, and anything that lets configuration input execute code.

Two things worth being explicit about, since they are properties of the design
rather than bugs:

- **Configuration values are not secrets to this crate.** It reads and merges
  them; it does not encrypt them at rest or redact them on `Debug`. Wrap a value
  in your own secret type if it must not be printed.
- **Source precedence is whatever the caller declares.** A source given a lower
  priority number overrides one with a higher number, and the crate does not
  judge whether that ordering is sensible for your threat model. Placing a
  user-writable file above a platform-injected source is a decision made at the
  call site.

## How this crate reduces its own supply-chain risk

- Every GitHub Actions dependency is pinned to a commit SHA, not a tag.
- `cargo-deny` runs in CI over advisories, licences, wildcard versions and
  dependency sources; git and unknown-registry dependencies are denied outright.
- `#![forbid(unsafe_code)]`.
- The `azure` feature is off by default, so a user who does not need it does not
  take on its dependency tree.
