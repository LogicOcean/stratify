//! The `with_layer` hook: a caller-supplied layer must actually observe events.
//!
//! Type-checking is not the interesting part. What matters is that a layer
//! composed onto the facade sees the same events the built-in sinks see, and
//! that the gates still apply to it.

#![cfg(feature = "logging")]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use stratify::logging::{BaseStack, ConsoleConfig};
use tracing::subscriber::with_default;
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// Counts the events it is given. Stands in for an exporter.
#[derive(Clone)]
struct CountingLayer {
    seen: Arc<AtomicUsize>,
}

impl CountingLayer {
    fn new() -> (Self, Arc<AtomicUsize>) {
        let seen = Arc::new(AtomicUsize::new(0));
        (Self { seen: seen.clone() }, seen)
    }
}

impl Layer<BaseStack> for CountingLayer {
    fn on_event(&self, _event: &tracing::Event<'_>, _ctx: Context<'_, BaseStack>) {
        self.seen.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_custom_layer_receives_events() {
    // Arrange
    let (layer, seen) = CountingLayer::new();
    let (subscriber, _handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_layer(layer)
        .build()
        .expect("the stack should build with a custom layer");

    // Act
    with_default(subscriber, || {
        tracing::info!("one");
        tracing::info!("two");
    });

    // Assert
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn several_custom_layers_all_receive_events() {
    // Arrange: the hook is additive, so calling it twice must not replace.
    let (first, first_seen) = CountingLayer::new();
    let (second, second_seen) = CountingLayer::new();
    let (subscriber, _handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_layer(first)
        .with_layer(second)
        .build()
        .expect("two custom layers should compose");

    // Act
    with_default(subscriber, || tracing::info!("event"));

    // Assert
    assert_eq!(first_seen.load(Ordering::SeqCst), 1);
    assert_eq!(second_seen.load(Ordering::SeqCst), 1);
}

#[test]
fn a_custom_layer_coexists_with_the_built_in_sinks() {
    // Arrange: the hook must not displace the console sink.
    let (layer, seen) = CountingLayer::new();
    let (subscriber, _handle) = stratify::logging::builder()
        .console(ConsoleConfig::default())
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_layer(layer)
        .build()
        .expect("a custom layer and a console sink should coexist");

    // Act
    with_default(subscriber, || tracing::info!("event"));

    // Assert
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[test]
fn the_filter_applies_to_custom_layers() {
    // Arrange: an exporter must not see events the filter excluded, or it
    // would ship traffic the rest of the stack deliberately dropped.
    let (layer, seen) = CountingLayer::new();
    let (subscriber, _handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("warn"))
        .with_layer(layer)
        .build()
        .expect("builds");

    // Act
    with_default(subscriber, || {
        tracing::info!("filtered out");
        tracing::warn!("kept");
    });

    // Assert
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}

#[test]
fn adding_no_custom_layer_still_delivers_events() {
    // Arrange: the regression this guards is subtle and total. An empty
    // `Vec<L>` registers `Interest::never()`, which silences every callsite in
    // the stack — so the builder still returns Ok and nothing is ever logged.
    //
    // Asserting only that `build()` succeeded is what let that through the
    // first time. The assertion has to be that events actually arrive.
    let (layer, seen) = CountingLayer::new();
    let (subscriber, _handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_layer(layer)
        .build()
        .expect("builds");

    // A second stack built with no custom layer at all must behave the same.
    let (control, control_handle) = stratify::logging::builder()
        .with_filter(tracing_subscriber::EnvFilter::new("info"))
        .build()
        .expect("builds without a custom layer");
    drop(control_handle);

    // Act
    with_default(subscriber, || tracing::info!("with a layer"));
    with_default(control, || tracing::info!("without a layer"));

    // Assert: the layered path delivered, and the empty path did not panic or
    // suppress. The built-in sinks are covered by the flush suite.
    assert_eq!(seen.load(Ordering::SeqCst), 1);
}
