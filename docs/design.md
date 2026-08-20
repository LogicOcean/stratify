# Design notes

Decisions that shaped the crate, and the reasoning behind them. API reference
lives in [the rustdoc](https://docs.rs/stratify); this is the material that does
not belong next to a function signature.

## Lower priority number wins

The number is a rank, not a weight.

The alternative — higher number is more important — reads more naturally the
first time and then costs you every time afterwards. Adding a source that should
outrank everything means finding the current maximum and exceeding it, and
inserting one between two existing sources means renumbering. With the ordering
inverted, a more specific source added later simply gets a smaller number, and
nothing that already exists has to change.

The cost is real: it surprises people once. That is why it is the first thing in
the README and why `examples/precedence.rs` demonstrates it rather than
asserting it.

## Merging is deep, not replacing

A higher-precedence source that sets `database.host` does not discard
`database.port` from a lower one. Anything else would force every override file
to restate the whole section it touches, which is how override files drift out
of sync with their base.

The consequence to know about: there is no way to express "remove this key".
A source can override a value but not delete one contributed by a lower layer.

## Conflicting shapes are an error

If one key requires `database` to be a string and another requires it to be an
object, no merge satisfies both. Rather than resolve it by load order — which
would make the result depend on something the caller cannot see — this is a
`ConfigError::MergeConflict` naming the key.

Keys are processed shallowest-first specifically so this is detected rather than
silently resolved.

## `Source::load` is async

Version 0.3 made this a breaking change, and the reason is narrow: a synchronous
trait leaves a network-backed source no good option. Blocking on a runtime
inside `load` panics when called from within an existing runtime, which is
exactly how a service using this crate would call it. The Azure source is not
implementable on a sync trait without that trap.

File and environment sources simply do not await anything, and reads
(`get_str`, `get`, …) stay synchronous — only loading and refreshing are async.

## The caller supplies the Azure credential

`AzureAppConfigSource` takes an `Arc<dyn TokenCredential>` rather than
constructing one. Three reasons, in order of importance:

1. There is no sensible default any more. `azure_identity` 1.0 removed
   `DefaultAzureCredential`; the replacements are situational
   (`ManagedIdentityCredential` in Azure, `DeveloperToolsCredential` on a
   workstation), and picking one here would be wrong half the time.
2. It keeps `azure_identity` out of this crate's dependency tree. The feature
   already costs a consumer 112 extra crates; adding the credential stack on top
   would be worse, and it is the caller's choice anyway.
3. It makes the source testable. The unit tests inject a fake credential and
   never contact Entra ID.

## Configuration values are not secrets

This crate reads and merges values. It does not encrypt them at rest, redact
them on `Debug`, or treat one source as more trusted than another. If a value
must not be printed, wrap it in your own secret type before it leaves the
`ConfigStore`.

This is stated in [SECURITY.md](../SECURITY.md) too, because it is the kind of
design property that gets reported as a vulnerability otherwise.
