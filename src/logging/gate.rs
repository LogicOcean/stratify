//! Event gating — the seam where sampling and rate limiting veto an event
//! before any format or write layer sees it.
//!
//! [`Sampler`](crate::logging::sampling::Sampler) and
//! [`RateLimiter`](crate::logging::rate_limit::RateLimiter) answer the same question
//! ("should this event survive?") from different angles. [`EventGate`](crate::logging::gate::EventGate) is that
//! question as a trait, and [`GateLayer`](crate::logging::gate::GateLayer) is the single adapter that turns any
//! chain of gates into a `tracing` layer. Adding a third axis of control means
//! implementing `EventGate` — nothing in the layer plumbing has to change.
//!
//! ```rust
//! use stratify::logging::gate::{EventGate, GateLayer};
//! use stratify::logging::rate_limit::{RateLimit, RateLimiter};
//! use stratify::logging::sampling::{SampleConfig, Sampler};
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::Registry;
//!
//! let gates: Vec<Box<dyn EventGate>> = vec![
//!     Box::new(Sampler::new(SampleConfig::new(0.1))),
//!     Box::new(RateLimiter::new(RateLimit::per_second(100))),
//! ];
//!
//! let subscriber = Registry::default().with(GateLayer::new(gates));
//! let _guard = tracing::subscriber::set_default(subscriber);
//! ```

use tracing::subscriber::Interest;
use tracing::{Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// A per-event predicate that can veto an event before it is formatted.
///
/// Implementations are consulted on every event, from every thread, so they
/// must be cheap and interior-mutable rather than `&mut self`.
pub trait EventGate: Send + Sync + 'static {
    /// Return `false` to drop the event.
    fn allows(&self, meta: &Metadata<'_>) -> bool;
}

/// Applies a chain of [`EventGate`]s to every event the subscriber sees.
///
/// Gates are consulted in order and the chain short-circuits on the first
/// veto, so an event dropped by an earlier gate never reaches a later one.
/// [`Builder::build`](super::Builder::build) relies on that: it puts sampling
/// ahead of rate limiting so a sampled-out event does not spend a token.
///
/// Spans are never gated — see [`GateLayer::enabled`].
pub struct GateLayer {
    gates: Vec<Box<dyn EventGate>>,
}

impl GateLayer {
    /// Build a layer from `gates`, or `None` when there is nothing to gate.
    ///
    /// Returning `Option<Self>` lets `build()` keep its `Option<Layer>`
    /// composition — a `None` layer is transparent, so an ungated subscriber
    /// pays nothing at all, not even the per-event `enabled()` call that
    /// [`Interest::sometimes`] forces.
    pub fn new(gates: Vec<Box<dyn EventGate>>) -> Option<Self> {
        if gates.is_empty() {
            return None;
        }
        Some(Self { gates })
    }
}

impl std::fmt::Debug for GateLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateLayer")
            .field("gates", &self.gates.len())
            .finish()
    }
}

impl<S: Subscriber> Layer<S> for GateLayer {
    /// Always [`Interest::sometimes`].
    ///
    /// Gates are stateful: the token bucket is spent per event and the sampler
    /// draws a fresh number each time. `tracing` caches an `Interest::always`
    /// or `never` verdict for the lifetime of a callsite and stops calling
    /// `enabled()`, which would freeze the very first verdict forever — the
    /// reason this layer exists instead of a
    /// [`FilterFn`](tracing_subscriber::filter::FilterFn), whose
    /// `register_callsite` does exactly that caching.
    fn register_callsite(&self, _meta: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn enabled(&self, meta: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        // Gates drop *events*. Vetoing a span would delete the context that
        // surviving events are recorded under — a rate-limited service would
        // lose its request spans, not just some log lines — so spans always
        // pass and only the events inside them are gated.
        if !meta.is_event() {
            return true;
        }
        self.gates.iter().all(|gate| gate.allows(meta))
    }
}

/// Assemble the gate layer from whichever gates were configured.
///
/// Order matters and is fixed here: sampling is consulted before rate
/// limiting, and the first veto short-circuits. An event the sampler discards
/// must not also spend a rate-limit token, or a 1% sample would drain the
/// bucket a hundred times faster than the events it actually keeps.
pub(super) fn layer_for(
    sampling: Option<super::sampling::SampleConfig>,
    rate_limit: Option<super::rate_limit::RateLimit>,
) -> Option<GateLayer> {
    let mut gates: Vec<Box<dyn EventGate>> = Vec::new();
    if let Some(config) = sampling {
        gates.push(Box::new(super::sampling::Sampler::new(config)));
    }
    if let Some(config) = rate_limit {
        gates.push(Box::new(super::rate_limit::RateLimiter::new(config)));
    }
    GateLayer::new(gates)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// A gate that records how often it was asked and answers from a script.
    struct ScriptedGate {
        verdict: bool,
        calls: Arc<AtomicUsize>,
    }

    impl EventGate for ScriptedGate {
        fn allows(&self, _meta: &Metadata<'_>) -> bool {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.verdict
        }
    }

    fn scripted(verdict: bool) -> (Box<dyn EventGate>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let gate = ScriptedGate {
            verdict,
            calls: Arc::clone(&calls),
        };
        (Box::new(gate), calls)
    }

    #[test]
    fn new_returns_none_without_gates() {
        // Arrange / Act
        let layer = GateLayer::new(Vec::new());

        // Assert
        assert!(layer.is_none());
    }

    #[test]
    fn new_returns_a_layer_when_gates_are_present() {
        // Arrange
        let (gate, _) = scripted(true);

        // Act
        let layer = GateLayer::new(vec![gate]);

        // Assert
        assert!(layer.is_some());
    }

    #[test]
    fn a_vetoing_gate_short_circuits_the_rest_of_the_chain() {
        // Arrange
        let (deny, deny_calls) = scripted(false);
        let (allow, allow_calls) = scripted(true);
        let layer = GateLayer::new(vec![deny, allow]).expect("two gates");
        let subscriber = tracing_subscriber::registry().with(layer);

        // Act
        tracing::subscriber::with_default(subscriber, || tracing::error!("gated"));

        // Assert
        assert_eq!(deny_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            allow_calls.load(Ordering::Relaxed),
            0,
            "a gate after a veto must not be consulted"
        );
    }

    #[test]
    fn every_event_re_consults_the_gates() {
        // Arrange — regression guard for callsite interest caching.
        let (gate, calls) = scripted(true);
        let layer = GateLayer::new(vec![gate]).expect("one gate");
        let subscriber = tracing_subscriber::registry().with(layer);

        // Act
        tracing::subscriber::with_default(subscriber, || {
            for _ in 0..5 {
                tracing::error!("repeated callsite");
            }
        });

        // Assert
        assert_eq!(
            calls.load(Ordering::Relaxed),
            5,
            "the gate's verdict was cached instead of re-evaluated"
        );
    }

    #[test]
    fn spans_bypass_the_gates() {
        // Arrange
        let (deny, calls) = scripted(false);
        let layer = GateLayer::new(vec![deny]).expect("one gate");
        let subscriber = tracing_subscriber::registry().with(layer);

        // Act
        let is_disabled = tracing::subscriber::with_default(subscriber, || {
            tracing::info_span!("survives").is_disabled()
        });

        // Assert
        assert!(!is_disabled, "a vetoing gate must not disable spans");
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
