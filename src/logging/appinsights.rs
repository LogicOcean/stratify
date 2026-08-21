//! Azure Application Insights export, behind the `appinsights` feature.
//!
//! Wraps the setup that every service otherwise repeats: build the exporter,
//! build a logger provider, bridge `tracing` events onto it, and flush on the
//! way out.
//!
//! There is no official Azure Monitor SDK for Rust, so this goes through
//! OpenTelemetry, which is what the Azure Monitor exporters for other languages
//! do underneath as well.

use super::error::Error;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_sdk::logs::SdkLoggerProvider;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use opentelemetry_sdk::Resource;

/// Where events land in Application Insights, and under what identity.
#[derive(Debug, Clone)]
pub struct AppInsightsConfig {
    /// The full connection string, including `InstrumentationKey=` and
    /// `IngestionEndpoint=`.
    pub connection_string: String,
    /// Reported as the service name, so several services in one workspace can
    /// be told apart.
    pub service_name: String,
    /// Fraction of traces exported, `0.0..=1.0`. Defaults to `1.0`.
    ///
    /// At production traffic, exporting every span is an Application Insights
    /// bill that grows linearly with load; this is the knob that bounds it.
    /// Sampling is parent-based, so a trace is kept or dropped whole rather
    /// than arriving with holes in it. Log records are not sampled — the
    /// per-sink filter is the tool for those.
    pub sample_rate: f64,
}

/// The environment variable Azure Monitor's own SDKs read.
///
/// Named here rather than written out at each use so the string that has to
/// match Azure exactly exists in one place.
pub const CONNECTION_STRING_VAR: &str = "APPLICATIONINSIGHTS_CONNECTION_STRING";

impl AppInsightsConfig {
    /// Export to the resource named by `connection_string`.
    pub fn new(connection_string: impl Into<String>, service_name: impl Into<String>) -> Self {
        Self {
            connection_string: connection_string.into(),
            service_name: service_name.into(),
            sample_rate: 1.0,
        }
    }

    /// Export this fraction of traces. Clamped to `0.0..=1.0` when applied.
    #[must_use]
    pub fn with_sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = rate;
        self
    }

    /// Read the connection string from [`CONNECTION_STRING_VAR`].
    ///
    /// # Errors
    /// [`Error::AppInsights`] when the variable is unset. Absence is a
    /// configuration choice rather than a failure, so callers who treat it that
    /// way should check the variable themselves and skip the sink.
    pub fn from_env(service_name: impl Into<String>) -> Result<Self, Error> {
        Self::from_lookup(service_name, |name| std::env::var(name).ok())
    }

    /// Read the connection string from a caller-supplied lookup.
    ///
    /// [`from_env`](Self::from_env) is this with the process environment
    /// supplied. Take this one when the value comes from somewhere else -- a
    /// config file, a key vault, Azure App Configuration -- so that reaching
    /// the sink does not mean first writing a secret into the environment of
    /// the whole process.
    ///
    /// It is also what lets this be tested without `std::env::set_var`, which
    /// is `unsafe` because it races any concurrent read, and a test harness
    /// runs tests on many threads at once.
    ///
    /// # Errors
    /// [`Error::AppInsights`] when the lookup yields nothing.
    pub fn from_lookup(
        service_name: impl Into<String>,
        lookup: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Self, Error> {
        let connection_string = lookup(CONNECTION_STRING_VAR)
            .ok_or_else(|| Error::AppInsights(format!("{CONNECTION_STRING_VAR} is not set")))?;
        Ok(Self::new(connection_string, service_name))
    }
}

/// Everything the Application Insights sink needs, built together.
///
/// Both providers share one exporter and one resource, so logs and spans arrive
/// under the same service identity and can be correlated in the portal.
#[derive(Debug, Clone)]
pub struct Providers {
    /// Backs the log records.
    pub logger: SdkLoggerProvider,
    /// Backs the spans, and is what puts `operation_Id` on each record.
    pub tracer: SdkTracerProvider,
}

impl Providers {
    /// Flush both, logs first.
    pub fn force_flush(&self) {
        let _ = self.logger.force_flush();
        let _ = self.tracer.force_flush();
    }

    /// Shut both down, logs first.
    pub fn shutdown(&self) {
        let _ = self.logger.shutdown();
        let _ = self.tracer.shutdown();
    }
}

/// Build the logger provider that backs the Application Insights layer.
///
/// # Errors
/// [`Error::AppInsights`] if the connection string cannot be parsed or the
/// HTTP client cannot be built.
pub fn provider(config: &AppInsightsConfig) -> Result<SdkLoggerProvider, Error> {
    // A blocking client, deliberately, and built off any async runtime.
    //
    // The batch processor runs on its own plain OS thread with no reactor
    // installed, so an async client panics there the first time it resolves
    // DNS. Constructing the blocking client on a scratch thread keeps its
    // internal runtime from being created inside the caller's.
    let client = std::thread::spawn(reqwest::blocking::Client::new)
        .join()
        .map_err(|_| Error::AppInsights("could not build the HTTP client".to_string()))?;

    let exporter = opentelemetry_application_insights::Exporter::new_from_connection_string(
        config.connection_string.clone(),
        client,
    )
    .map_err(|e| Error::AppInsights(format!("unusable connection string: {e}")))?;

    let resource = Resource::builder_empty()
        .with_attributes(vec![opentelemetry::KeyValue::new(
            "service.name",
            config.service_name.clone(),
        )])
        .build();

    Ok(SdkLoggerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(resource)
        .build())
}

/// Build both providers, so log records carry trace correlation.
///
/// Without a tracer provider, records reach Application Insights with no
/// `operation_Id`. They are searchable but not correlated: you cannot click a
/// request and see the lines it produced, which is most of what the portal is
/// for. The tracer establishes the trace context that the log appender then
/// stamps onto each record.
///
/// # Errors
/// [`Error::AppInsights`] if the connection string cannot be parsed or the
/// HTTP client cannot be built.
pub fn providers(config: &AppInsightsConfig) -> Result<Providers, Error> {
    let client = std::thread::spawn(reqwest::blocking::Client::new)
        .join()
        .map_err(|_| Error::AppInsights("could not build the HTTP client".to_string()))?;

    let exporter = opentelemetry_application_insights::Exporter::new_from_connection_string(
        config.connection_string.clone(),
        client,
    )
    .map_err(|e| Error::AppInsights(format!("unusable connection string: {e}")))?;

    let resource = Resource::builder_empty()
        .with_attributes(vec![opentelemetry::KeyValue::new(
            "service.name",
            config.service_name.clone(),
        )])
        .build();

    Ok(Providers {
        logger: SdkLoggerProvider::builder()
            .with_batch_exporter(exporter.clone())
            .with_resource(resource.clone())
            .build(),
        tracer: SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            // Parent-based: a trace is kept or dropped whole. A ratio sampler
            // alone would decide per span and ship traces with holes in them.
            // The finite check matters because `clamp` propagates NaN, and a
            // NaN ratio is not a rate the sampler defines behaviour for.
            .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
                if config.sample_rate.is_finite() {
                    config.sample_rate.clamp(0.0, 1.0)
                } else {
                    1.0
                },
            ))))
            .build(),
    })
}

/// The layer that turns `tracing` spans into OpenTelemetry spans.
///
/// Pair it with [`layer`]: this one establishes the trace context, that one
/// stamps it onto log records.
pub fn trace_layer<S>(
    providers: &Providers,
) -> tracing_opentelemetry::OpenTelemetryLayer<S, opentelemetry_sdk::trace::SdkTracer>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    tracing_opentelemetry::layer().with_tracer(providers.tracer.tracer("stratify"))
}

/// The `tracing` layer that feeds `provider`.
pub fn layer(
    provider: &SdkLoggerProvider,
) -> OpenTelemetryTracingBridge<SdkLoggerProvider, opentelemetry_sdk::logs::SdkLogger> {
    OpenTelemetryTracingBridge::new(provider)
}

/// Everything the facade needs from a configured Application Insights export.
///
/// The two layers stay separate because they attach at different depths and the
/// order between them matters: the tracer opens the OpenTelemetry context, and
/// the log bridge stamps it onto each record. Their types are erased for the
/// same reason as the other sinks, in `sinks::ErasedLayer`.
pub(super) struct Export<L, T> {
    pub(super) providers: Option<Providers>,
    pub(super) logs: Option<super::sinks::ErasedLayer<L>>,
    pub(super) traces: Option<super::sinks::ErasedLayer<T>>,
}

/// Build the export, or nothing when Application Insights was not configured.
pub(super) fn export<L, T>(
    config: Option<AppInsightsConfig>,
    filter: &Option<tracing_subscriber::EnvFilter>,
) -> Result<Export<L, T>, Error>
where
    L: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    // `OpenTelemetryLayer` holds the subscriber type, so it is only `Send +
    // Sync` when that type is, and an erased layer has to be both.
    T: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a> + Send + Sync,
{
    use tracing_subscriber::Layer as _;

    let Some(config) = config else {
        return Ok(Export {
            providers: None,
            logs: None,
            traces: None,
        });
    };

    let providers = providers(&config)?;
    let logs = layer(&providers.logger).with_filter(super::sinks::clone_filter(filter));
    let traces = trace_layer::<T>(&providers);

    Ok(Export {
        providers: Some(providers),
        logs: Some(Box::new(logs)),
        traces: Some(Box::new(traces)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_malformed_connection_string_is_an_error_not_a_panic() {
        // Arrange
        let config = AppInsightsConfig::new("not-a-connection-string", "svc");

        // Act
        let result = provider(&config);

        // Assert
        assert!(matches!(result, Err(Error::AppInsights(_))));
    }

    #[test]
    fn a_well_formed_connection_string_builds_a_provider() {
        // Arrange: unroutable on purpose. Construction is lazy, so nothing is
        // sent and no network is needed.
        let config = AppInsightsConfig::new(
            "InstrumentationKey=00000000-1111-2222-3333-444444444444;\
             IngestionEndpoint=https://example.invalid/",
            "svc",
        );

        // Act
        let built = provider(&config);

        // Assert
        assert!(
            built.is_ok(),
            "a valid connection string must yield a provider"
        );
        if let Ok(p) = built {
            let _ = p.shutdown();
        }
    }

    #[test]
    fn from_lookup_reports_an_absent_variable() {
        // Arrange: an empty environment, expressed as a lookup rather than by
        // clearing the real one. `std::env::remove_var` is `unsafe` because it
        // races any concurrent read, and tests run on many threads at once.
        let empty = |_: &str| None;

        // Act
        let result = AppInsightsConfig::from_lookup("svc", empty);

        // Assert
        assert!(matches!(result, Err(Error::AppInsights(_))));
    }

    #[test]
    fn from_lookup_names_the_variable_in_the_error() {
        // Arrange
        let empty = |_: &str| None;

        // Act
        let result = AppInsightsConfig::from_lookup("svc", empty);

        // Assert: the message has to say which variable, or an operator has
        // nothing to act on.
        match result {
            Err(Error::AppInsights(message)) => {
                assert!(message.contains(CONNECTION_STRING_VAR), "got: {message}")
            }
            other => panic!("expected an app insights error, got {other:?}"),
        }
    }

    #[test]
    fn from_lookup_reads_the_variable_azure_defines() {
        // Arrange: the name is fixed by Azure, so assert on it rather than
        // trusting whatever the lookup is handed.
        let seen = std::cell::Cell::new(None);
        let lookup = |name: &str| {
            seen.set(Some(name.to_string()));
            Some("InstrumentationKey=abc".to_string())
        };

        // Act
        let config = AppInsightsConfig::from_lookup("svc", lookup).expect("a value is present");

        // Assert
        assert_eq!(seen.take().as_deref(), Some(CONNECTION_STRING_VAR));
        assert_eq!(config.connection_string, "InstrumentationKey=abc");
        assert_eq!(config.service_name, "svc");
    }

    #[test]
    fn the_sample_rate_defaults_to_everything() {
        // Arrange / Act
        let config = AppInsightsConfig::new("cs", "svc");

        // Assert: 1.0 keeps today's behaviour for anyone not setting it.
        assert_eq!(config.sample_rate, 1.0);
    }

    #[test]
    fn an_out_of_range_sample_rate_still_builds_providers() {
        // Arrange: the clamp lives at the point of use, so a wild value is
        // corrected rather than panicking inside the exporter.
        let config = AppInsightsConfig::new(
            "InstrumentationKey=00000000-0000-0000-0000-000000000000;\
             IngestionEndpoint=https://unroutable.invalid/",
            "svc",
        )
        .with_sample_rate(3.0);

        // Act
        let built = providers(&config);

        // Assert: construction is lazy, so this exercises the sampler wiring
        // without contacting the network.
        assert!(built.is_ok());
        if let Ok(p) = built {
            p.shutdown();
        }
    }

    #[test]
    fn the_service_name_is_carried_on_the_config() {
        // Arrange / Act
        let config = AppInsightsConfig::new("cs", "nse-api");

        // Assert
        assert_eq!(config.service_name, "nse-api");
    }
}
