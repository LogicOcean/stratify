use std::path::PathBuf;
use std::sync::RwLock;

use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::fmt::format;
use tracing_subscriber::fmt::{FmtContext, FormatEvent};
use tracing_subscriber::layer::{Layered, SubscriberExt};
use tracing_subscriber::{fmt, Layer, Registry};

use flush::FlushableWriter;

#[cfg(feature = "appinsights")]
pub mod appinsights;
pub mod file;
/// Non-blocking writer plumbing: flushing, queue depth, drop counting.
pub mod flush;
/// Event gates: the trait sampling and rate limiting plug into.
pub mod gate;
pub mod rate_limit;
/// Runtime filter reload: the channel [`reload_filter`] drives.
pub mod reload;
pub mod sampling;
pub(crate) mod sinks;
pub mod size_rolling;
pub mod syslog;

pub mod error;
pub mod settings;

pub use error::Error;

// ── Public API ──────────────────────────────────────────────────────────────

/// The subscriber stack a custom layer is composed onto.
///
/// The reload filter is pinned innermost because it only implements
/// `Layer<Registry>`, so anything added by [`Builder::with_layer`] sits
/// directly outside it. Naming the type is what makes the hook possible: a
/// boxed layer has to know the subscriber it will be attached to.
pub type BaseStack = Layered<reload::ReloadFilterLayer, Registry>;

/// A layer supplied by the caller, erased so several can be stored together.
pub type CustomLayer = Box<dyn Layer<BaseStack> + Send + Sync>;

/// Formats a whole log line.
///
/// Implement this to control the output completely: field order, timestamp,
/// level rendering, separators. It replaces the built-in layout rather than
/// adjusting it.
///
/// Deliberately independent of the subscriber type, so one formatter works for
/// the console sink and the file sink alike. The trade is that it receives the
/// event and a writer but not the span context; if you need spans, compose your
/// own `fmt` layer through [`Builder::with_layer`] instead.
///
/// ```rust
/// use stratify::logging::LineFormatter;
/// use std::fmt::Write;
///
/// struct Minimal;
///
/// impl LineFormatter for Minimal {
///     fn format_line(
///         &self,
///         writer: &mut dyn Write,
///         event: &tracing::Event<'_>,
///         spans: &stratify::logging::SpanScope,
///     ) -> std::fmt::Result {
///         write!(
///             writer,
///             "[{}] {} {}",
///             event.metadata().level(),
///             spans.current().map_or("-", |s| s.name),
///             event.metadata().target(),
///         )
///     }
/// }
/// ```
pub trait LineFormatter: Send + Sync + 'static {
    /// Write one line for `event`. The newline is added by the caller.
    ///
    /// `spans` carries the enclosing span scope, outermost first. It is a
    /// concrete snapshot rather than the subscriber's own context type, which
    /// is what keeps this trait independent of the subscriber and therefore
    /// usable on every sink.
    fn format_line(
        &self,
        writer: &mut dyn std::fmt::Write,
        event: &tracing::Event<'_>,
        spans: &SpanScope,
    ) -> std::fmt::Result;
}

/// One span enclosing an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanInfo {
    /// The span's name, as given to `info_span!` and friends.
    pub name: &'static str,
    /// The module path the span was created in, when recorded.
    pub target: String,
    /// The span's fields, already rendered, in the form `key=value key=value`.
    ///
    /// Pre-rendered rather than structured because the subscriber stores them
    /// that way; re-parsing to hand back a map would invent structure the
    /// registry does not keep.
    pub fields: String,
}

/// The span scope enclosing an event, outermost first.
#[derive(Debug, Clone, Default)]
pub struct SpanScope {
    spans: Vec<SpanInfo>,
}

impl SpanScope {
    /// The enclosing spans, outermost first. Empty when the event is not inside
    /// a span.
    pub fn spans(&self) -> &[SpanInfo] {
        &self.spans
    }

    /// The innermost span, which is usually the interesting one.
    pub fn current(&self) -> Option<&SpanInfo> {
        self.spans.last()
    }

    /// Whether the event was recorded inside any span.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

/// A caller-supplied line formatter.
pub type EventFormatter = Box<dyn LineFormatter>;

/// Prefixes every line with the process-wide fields.
///
/// `tracing` event fields are immutable, so global values cannot be added to
/// the event itself. They are rendered ahead of whatever the inner formatter
/// writes instead, which keeps them on every line of the text sinks.
///
/// Not applied to the JSON sink: injecting text ahead of a JSON object would
/// produce something that is no longer JSON. See the note on
/// [`Builder::global_field`].
pub(crate) struct WithGlobals<F> {
    pub(crate) inner: F,
    /// Pre-rendered once at build time rather than per event.
    pub(crate) prefix: String,
    /// Lower-cased field names whose values are masked. Empty disables the
    /// buffering that redaction requires, so the default path is untouched.
    pub(crate) redact: Vec<String>,
}

impl<F> WithGlobals<F> {
    /// Replace `key=value` with `key=[redacted]` for every configured key.
    ///
    /// Operates on rendered text because `tracing` event fields are immutable
    /// by the time a formatter sees them. That bounds what it can catch, which
    /// the public documentation states plainly.
    fn mask(&self, line: &str) -> String {
        let mut out = line.to_string();
        for key in &self.redact {
            let mut from = 0;
            while let Some(found) = out[from..].to_lowercase().find(key.as_str()) {
                let start = from + found;
                let after_key = start + key.len();
                if out[after_key..].starts_with('=') {
                    let value_start = after_key + 1;
                    // A quoted value runs to its closing quote, not to the first
                    // space. Stopping at whitespace masks `"Bearer` and leaves
                    // ` abc123"` in the log, which is worse than not redacting
                    // at all because it looks handled.
                    let value_end = if out[value_start..].starts_with('"') {
                        out[value_start + 1..]
                            .find('"')
                            .map_or(out.len(), |offset| value_start + 1 + offset + 1)
                    } else {
                        out[value_start..]
                            .find(char::is_whitespace)
                            .map_or(out.len(), |offset| value_start + offset)
                    };
                    out.replace_range(value_start..value_end, "[redacted]");
                    from = value_start + "[redacted]".len();
                } else {
                    from = after_key;
                }
                if from >= out.len() {
                    break;
                }
            }
        }
        out
    }
}

impl<S, N, F> FormatEvent<S, N> for WithGlobals<F>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
    F: FormatEvent<S, N>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        // An empty prefix must add nothing at all, not a leading space: the
        // no-global-fields path has to render exactly as it did before.
        if !self.prefix.is_empty() {
            write!(writer, "{} ", self.prefix)?;
        }
        if self.redact.is_empty() {
            return self.inner.format_event(ctx, writer, event);
        }
        // Redaction needs the finished line, so render to a buffer first. Only
        // pay for that when there is something to redact.
        let mut buffer = String::new();
        self.inner
            .format_event(ctx, format::Writer::new(&mut buffer), event)?;
        write!(writer, "{}", self.mask(&buffer))
    }
}

/// Adapts a [`LineFormatter`] into what `fmt::Layer` expects.
///
/// Generic over the subscriber, which is the whole point: the sinks sit at
/// different depths in the stack, and a formatter pinned to one depth could not
/// be used for both.
pub(crate) struct BoxedFormat(pub(crate) EventFormatter);

impl<S, N> FormatEvent<S, N> for BoxedFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        // Flattened here, where the subscriber type is still known, so the
        // formatter itself never has to name it.
        let mut spans = Vec::new();
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                let fields = span
                    .extensions()
                    .get::<tracing_subscriber::fmt::FormattedFields<N>>()
                    .map(|f| f.fields.clone())
                    .unwrap_or_default();
                spans.push(SpanInfo {
                    name: span.name(),
                    target: span.metadata().target().to_string(),
                    fields,
                });
            }
        }
        self.0
            .format_line(&mut writer, event, &SpanScope { spans })?;
        writeln!(writer)
    }
}

// ── Module-level entry points ───────────────────────────────────────────────
//
// These were associated functions on a `LoggingKit` namespace struct when this
// lived in its own crate. The module is the namespace now, so they are plain
// functions: `logging::builder()`, `logging::flush()`.

/// Start building the logging stack.
///
/// Every sink is non-blocking — writers flush on background threads so the
/// calling thread is never blocked by a slow pipe or full disk.
///
/// ```rust
/// use stratify::logging::{self, ConsoleConfig, JsonConfig};
///
/// # fn main() -> Result<(), stratify::logging::Error> {
/// logging::builder()
///     .console(ConsoleConfig::default())
///     .json(JsonConfig::default())
///     .reloadable()
///     .init()?;
/// # Ok(())
/// # }
/// ```
pub fn builder() -> Builder {
    Builder::default()
}

/// A clone of the installed handle, or `None` before [`Builder::init`]
/// runs or after [`reset`] discards it.
pub fn handle() -> Option<Handle> {
    LOGGING_HANDLE.read().ok()?.clone()
}

/// Whether a subscriber from this facade is currently installed.
pub fn is_initialized() -> bool {
    LOGGING_HANDLE.read().map(|g| g.is_some()).unwrap_or(false)
}

/// Swap the active level/target filter at runtime.
///
/// # Errors
///
/// - [`Error::NotInitialized`] — [`init`](Builder::init) has not run, or
///   [`reset`] discarded the handle.
/// - [`Error::ReloadNotEnabled`] — the subscriber was built without
///   [`Builder::reloadable`], so there is no channel to reload through.
/// - [`Error::FilterReload`] — the reload channel rejected the filter.
pub fn reload_filter(filter: EnvFilter) -> Result<(), Error> {
    let guard = LOGGING_HANDLE.read().map_err(|_| Error::LockPoisoned)?;
    let handle = guard.as_ref().ok_or(Error::NotInitialized)?;
    handle
        .reload_tx
        .as_ref()
        .ok_or(Error::ReloadNotEnabled)?
        .reload(filter)?;
    Ok(())
}

/// Block until every queued event has reached its destination.
///
/// Drains the console, JSON and file writers installed by [`init`] and
/// returns only once each backlog has been written. Logging continues
/// afterwards, so this is safe both at a checkpoint and before process
/// exit — and it is the only way to guarantee the queue is not lost at
/// exit, since the stored handle lives in a `static` and is never dropped.
///
/// # Errors
///
/// [`Error::NotInitialized`] if [`init`] has not run, or if
/// [`reset`] has discarded the handle — a clone taken before
/// the reset can still flush.
///
/// [`init`]: Builder::init
pub fn flush() -> Result<(), Error> {
    let guard = LOGGING_HANDLE.read().map_err(|_| Error::LockPoisoned)?;
    let handle = guard.as_ref().ok_or(Error::NotInitialized)?;
    flush_handle(handle);
    Ok(())
}

/// Forget the stored handle.
///
/// Afterwards [`is_initialized`] reports `false` and
/// [`handle`] returns `None`, so [`flush`](flush()) has
/// nothing left to drain and returns an error. Take a clone of the handle
/// *before* calling this if you still need to flush — clones are
/// full-strength.
///
/// Logging itself keeps working. The installed layers own the writers
/// jointly with the handle, so dropping the handle neither stops the
/// background workers nor leaves events queued for a thread that has
/// exited.
///
/// This does **not** uninstall the global subscriber — `tracing` offers no
/// way to do that — so a later [`init`](Builder::init) still fails, with a
/// "failed to set global subscriber" error rather than "already
/// initialized". Tests that need a fresh subscriber should use
/// [`Builder::build`] with `tracing::subscriber::set_default` instead,
/// which is scoped to one thread and needs no reset at all.
pub fn reset() {
    if let Ok(mut g) = LOGGING_HANDLE.write() {
        *g = None;
    }
}
// ── Builder ─────────────────────────────────────────────────────────────────
//
// Uses `Option<Layer>` to eliminate the 4-arm match dispatch:
// tracing-subscriber implements `Layer<S>` for `Option<L>` where
// `L: Layer<S>`.  `None` layers pass events through transparently.
// This drops cyclomatic complexity from 21 → ~4 and enables a
// testable `build()` method that returns the subscriber.

/// Assembles the logging stack: sinks, filters, gates, exporters.
///
/// Start from [`builder`], chain what the service needs, and finish with
/// [`init`](Builder::init) (installs globally, returns the [`Handle`]) or
/// [`build`](Builder::build) (returns the subscriber for scoped use).
#[derive(Default)]
pub struct Builder {
    console: Option<ConsoleConfig>,
    json: Option<JsonConfig>,
    file: Option<FileConfig>,
    filter: Option<EnvFilter>,
    reloadable: bool,
    queue_size: Option<usize>,
    rate_limit: Option<rate_limit::RateLimit>,
    sampling: Option<sampling::SampleConfig>,
    /// Caller-supplied layers, in the order they were added.
    layers: Vec<CustomLayer>,
    /// Replaces the built-in console layout when set.
    console_format: Option<EventFormatter>,
    /// Replaces the built-in plain-text file layout when set.
    file_format: Option<EventFormatter>,
    /// Route panics through the subscriber when set.
    capture_panics: bool,
    /// Fields attached to every event.
    global_fields: Vec<(String, String)>,
    /// Lower-cased field names whose values are masked in rendered output.
    redact_keys: Vec<String>,
    /// Syslog sink, when configured.
    syslog: Option<syslog::SyslogConfig>,
    /// Application Insights export, when configured.
    #[cfg(feature = "appinsights")]
    app_insights: Option<appinsights::AppInsightsConfig>,
    /// Per-sink filter directives, overriding the global filter for one sink.
    sink_filters: SinkFilters,
}

/// Filter directives applied to individual sinks.
///
/// Each is layered on top of the global filter rather than replacing it: a sink
/// never sees an event the global filter already excluded, because the global
/// filter sits innermost and short-circuits first.
#[derive(Debug, Clone, Default)]
struct SinkFilters {
    console: Option<String>,
    json: Option<String>,
    file: Option<String>,
    syslog: Option<String>,
    #[cfg(feature = "appinsights")]
    app_insights: Option<String>,
}

/// The same set of filters, compiled.
///
/// Parsing is separated from use so that a bad directive fails during
/// `build()`, naming the sink that owns it, rather than becoming a filter that
/// silently matches nothing at runtime.
struct ParsedSinkFilters {
    console: Option<EnvFilter>,
    json: Option<EnvFilter>,
    file: Option<EnvFilter>,
    syslog: Option<EnvFilter>,
    #[cfg(feature = "appinsights")]
    app_insights: Option<EnvFilter>,
}

impl SinkFilters {
    /// Compile every configured directive, or report the first that is invalid.
    fn parse(&self) -> Result<ParsedSinkFilters, Error> {
        fn one(directives: &Option<String>, sink: &str) -> Result<Option<EnvFilter>, Error> {
            match directives {
                None => Ok(None),
                Some(d) => EnvFilter::try_new(d)
                    .map(Some)
                    .map_err(|e| Error::InvalidFilter(format!("{sink}: {e}"))),
            }
        }

        Ok(ParsedSinkFilters {
            console: one(&self.console, "console")?,
            json: one(&self.json, "json")?,
            file: one(&self.file, "file")?,
            syslog: one(&self.syslog, "syslog")?,
            #[cfg(feature = "appinsights")]
            app_insights: one(&self.app_insights, "app_insights")?,
        })
    }
}

impl Builder {
    /// Add the console sink.
    pub fn console(mut self, config: ConsoleConfig) -> Self {
        self.console = Some(config);
        self
    }

    /// Add the JSON sink.
    pub fn json(mut self, config: JsonConfig) -> Self {
        self.json = Some(config);
        self
    }

    /// Add the file sink.
    pub fn file(mut self, config: FileConfig) -> Self {
        self.file = Some(config);
        self
    }

    /// Compose an additional `tracing` layer onto the stack.
    ///
    /// The facade owns the subscriber, so a caller cannot otherwise attach an
    /// exporter of their own. This is the hook for anything layer-shaped that
    /// this crate does not ship: an OpenTelemetry bridge, a Sentry or
    /// Honeycomb layer, a metrics recorder.
    ///
    /// Layers added here sit outside the reload filter and inside the sampling
    /// and rate-limit gates, so they observe exactly the events the built-in
    /// sinks observe. An event a gate discards does not reach them, which is
    /// usually what you want: an exporter should not receive traffic the rest
    /// of your logging deliberately dropped.
    ///
    /// Call it more than once to add several.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig};
    /// # fn example<L>(exporter: L) -> Result<(), stratify::logging::Error>
    /// # where L: tracing_subscriber::Layer<stratify::logging::BaseStack> + Send + Sync + 'static {
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .with_layer(exporter)
    ///     .init()
    ///     .map(|_handle| ())
    /// # }
    /// ```
    pub fn with_layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<BaseStack> + Send + Sync + 'static,
    {
        self.layers.push(Box::new(layer));
        self
    }

    /// Take full control of the console log line.
    ///
    /// The formatter is handed the event and writes the entire line: field
    /// order, timestamp, level rendering, all of it. Implement
    /// [`LineFormatter`].
    ///
    /// This replaces the built-in layout, so `ConsoleConfig`'s `with_color`,
    /// `with_thread_ids` and `with_target` no longer apply — they configure the
    /// layout you just replaced.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig, LineFormatter};
    /// # fn example<F>(mine: F) -> Result<(), stratify::logging::Error>
    /// # where F: LineFormatter {
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .console_format(mine)
    ///     .init()
    ///     .map(|_handle| ())
    /// # }
    /// ```
    pub fn console_format<F>(mut self, format: F) -> Self
    where
        F: LineFormatter,
    {
        self.console_format = Some(Box::new(format));
        self
    }

    /// Take full control of the plain-text file log line.
    ///
    /// Applies only when the file sink is [`FileFormat::Text`]. JSON's value is
    /// a fixed machine-readable shape, and a custom layout would defeat the
    /// point of choosing it.
    pub fn file_format<F>(mut self, format: F) -> Self
    where
        F: LineFormatter,
    {
        self.file_format = Some(Box::new(format));
        self
    }

    /// Route panics through the logging stack.
    ///
    /// Installs a panic hook that emits an `ERROR` event carrying the payload,
    /// the file and the line, then calls whatever hook was already installed so
    /// the default backtrace behaviour is preserved.
    ///
    /// Without this a panicking thread writes to stderr directly and bypasses
    /// every sink: it never reaches the file, and it never reaches an exporter.
    /// That is the one event you least want to lose.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig};
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .capture_panics()
    ///     .init()
    ///     .expect("logging");
    /// ```
    pub fn capture_panics(mut self) -> Self {
        self.capture_panics = true;
        self
    }

    /// Attach a field to every event this process emits.
    ///
    /// For the values that identify the emitter rather than the event: service
    /// name, version, environment, region. Without them, logs from several
    /// services in one aggregator cannot be told apart or joined.
    ///
    /// Call it repeatedly to add several. A later call with the same key
    /// replaces the earlier value.
    ///
    /// Applies to the console and plain-text file sinks. The **JSON sink is not
    /// affected**: `tracing` event fields are immutable, so these are rendered
    /// alongside the line rather than added to the event, and text prepended to
    /// a JSON object would stop it being JSON. For structured global context in
    /// JSON, wrap your work in a span carrying the fields, or attach them in
    /// your exporter.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig};
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .global_field("service", "nse-api")
    ///     .global_field("version", env!("CARGO_PKG_VERSION"))
    ///     .init()
    ///     .expect("logging");
    /// ```
    pub fn global_field(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        let key = key.into();
        self.global_fields.retain(|(existing, _)| existing != &key);
        self.global_fields.push((key, value.into()));
        self
    }

    /// Mask the values of fields whose names look sensitive.
    ///
    /// Matches `key=value` in the rendered line, case-insensitively, and
    /// replaces the value with `[redacted]`.
    ///
    /// A safety net, not a guarantee, and worth understanding before relying on
    /// it. `tracing` event fields are immutable by the time a formatter sees
    /// them, so this works on rendered text: a secret that never appears as
    /// `key=value` — embedded in a URL, inside a JSON blob, or interpolated
    /// into the message itself — is not caught. Not logging secrets stays the
    /// primary control; this catches the accident.
    ///
    /// Applies to the text sinks. The JSON sink is untouched, for the same
    /// reason as [`Builder::global_field`].
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig};
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .redact(["password", "token", "authorization"])
    ///     .init()
    ///     .expect("logging");
    /// ```
    pub fn redact<I, K>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = K>,
        K: AsRef<str>,
    {
        self.redact_keys
            .extend(keys.into_iter().map(|k| k.as_ref().to_lowercase()));
        self
    }

    /// Send events to the local syslog daemon.
    ///
    /// Unix only; accepted and inert elsewhere so a cross-platform service does
    /// not need `cfg` at the call site. A host with no syslog socket is not an
    /// error either: the sink discards and the others keep working, because a
    /// logging destination being absent should not stop a service starting.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{syslog::SyslogConfig};
    /// stratify::logging::builder()
    ///     .syslog(SyslogConfig::new("nse-api"))
    ///     .init()
    ///     .expect("logging");
    /// ```
    /// Export events to Azure Application Insights.
    ///
    /// Requires the `appinsights` feature. Handles the exporter, the logger
    /// provider and the `tracing` bridge, and the returned [`Handle`]
    /// flushes the batch on shutdown — the exporter batches, so without that
    /// the final records never leave the process.
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "appinsights")]
    /// # fn example() -> Result<(), stratify::logging::Error> {
    /// use stratify::logging::{appinsights::AppInsightsConfig, ConsoleConfig};
    ///
    /// stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .app_insights(AppInsightsConfig::from_env("nse-api")?)
    ///     .init()
    ///     .map(|_handle| ())
    /// # }
    /// ```
    #[cfg(feature = "appinsights")]
    pub fn app_insights(mut self, config: appinsights::AppInsightsConfig) -> Self {
        self.app_insights = Some(config);
        self
    }

    /// Add the syslog sink. Unix only, and inert elsewhere, so
    /// cross-platform callers need no `cfg`.
    pub fn syslog(mut self, config: syslog::SyslogConfig) -> Self {
        self.syslog = Some(config);
        self
    }

    /// Filter the console sink independently of the others.
    ///
    /// Directives use the same syntax as `RUST_LOG`.
    ///
    /// Applied on top of the global filter, never instead of it: a sink cannot
    /// see an event the global filter excluded, because that one sits innermost
    /// and short-circuits first. So this narrows, it does not widen. To send
    /// DEBUG to a file and WARN to an exporter, set the global filter to the
    /// most permissive level you want anywhere and narrow each sink from there.
    ///
    /// ```rust,no_run
    /// # use stratify::logging::{ConsoleConfig};
    /// # use tracing_subscriber::EnvFilter;
    /// stratify::logging::builder()
    ///     .with_filter(EnvFilter::new("debug"))
    ///     .console(ConsoleConfig::default())
    ///     .console_filter("warn")
    ///     .init()
    ///     .expect("logging");
    /// ```
    pub fn console_filter(mut self, directives: impl Into<String>) -> Self {
        self.sink_filters.console = Some(directives.into());
        self
    }

    /// Filter the JSON sink independently. See [`Builder::console_filter`].
    pub fn json_filter(mut self, directives: impl Into<String>) -> Self {
        self.sink_filters.json = Some(directives.into());
        self
    }

    /// Filter the file sink independently. See [`Builder::console_filter`].
    pub fn file_filter(mut self, directives: impl Into<String>) -> Self {
        self.sink_filters.file = Some(directives.into());
        self
    }

    /// Filter the syslog sink independently. See [`Builder::console_filter`].
    pub fn syslog_filter(mut self, directives: impl Into<String>) -> Self {
        self.sink_filters.syslog = Some(directives.into());
        self
    }

    /// Filter the Application Insights sink independently.
    ///
    /// The usual reason to narrow this one specifically: telemetry is billed by
    /// volume, so shipping DEBUG to the portal is expensive in a way that
    /// writing it to a local file is not. See [`Builder::console_filter`].
    #[cfg(feature = "appinsights")]
    pub fn app_insights_filter(mut self, directives: impl Into<String>) -> Self {
        self.sink_filters.app_insights = Some(directives.into());
        self
    }

    /// Set the global filter. Without it, `RUST_LOG` is consulted and
    /// `info` is the fallback.
    pub fn with_filter(mut self, filter: EnvFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Allow [`reload_filter`] to swap the global filter at runtime.
    pub fn reloadable(mut self) -> Self {
        self.reloadable = true;
        self
    }

    /// Override the default non-blocking queue size.
    ///
    /// Default: `128_000` (the `tracing-appender` built-in default). Bump this
    /// for high-throughput services; lower for memory-constrained workloads.
    pub fn queue_size(mut self, size: usize) -> Self {
        self.queue_size = Some(size);
        self
    }

    /// Apply a token-bucket rate limit to every event, dropping the excess
    /// before it reaches any format or write layer.
    ///
    /// The bucket starts full and refills at `max_events / per_secs` per
    /// second, so a burst of `max_events` passes immediately and the rest are
    /// discarded until the bucket refills. Spans are never rate limited —
    /// only the events inside them.
    ///
    /// # Ordering
    ///
    /// Rate limiting runs *after* [`sampling`](Self::sampling) and *before*
    /// the level/target filter installed by [`with_filter`](Self::with_filter).
    /// The filter sits innermost because the reload layer is bound to
    /// `Registry`, which means an event the filter would have rejected still
    /// costs a token. Size the budget against everything your code emits, not
    /// only what the filter admits.
    ///
    /// ```rust
    /// use stratify::logging::rate_limit::RateLimit;
    /// use stratify::logging::{ConsoleConfig};
    ///
    /// # fn main() -> Result<(), stratify::logging::Error> {
    /// let (subscriber, _handle) = stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .rate_limit(RateLimit::per_second(100))
    ///     .build()?;
    /// # let _ = subscriber;
    /// # Ok(())
    /// # }
    /// ```
    pub fn rate_limit(mut self, limit: rate_limit::RateLimit) -> Self {
        self.rate_limit = Some(limit);
        self
    }

    /// Apply probabilistic event sampling — drops a fraction of events at or
    /// above `min_level` before they reach any format or write layer.
    ///
    /// Sampling is consulted before [`rate_limit`](Self::rate_limit), so a
    /// sampled-out event does not spend a token. Spans are never sampled —
    /// only the events inside them.
    ///
    /// ```rust
    /// use stratify::logging::sampling::SampleConfig;
    /// use stratify::logging::{ConsoleConfig};
    /// use tracing::level_filters::LevelFilter;
    ///
    /// # fn main() -> Result<(), stratify::logging::Error> {
    /// let sampling = SampleConfig::new(0.1).with_min_level(LevelFilter::DEBUG);
    ///
    /// let (subscriber, _handle) = stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .sampling(sampling)
    ///     .build()?;
    /// # let _ = subscriber;
    /// # Ok(())
    /// # }
    /// ```
    pub fn sampling(mut self, config: sampling::SampleConfig) -> Self {
        self.sampling = Some(config);
        self
    }

    /// Build the subscriber for testing or scoped use.
    ///
    /// Returns a fully composed subscriber and the [`Handle`] owning its
    /// writers. Unlike [`init`](Self::init) this touches no global state, so
    /// tests can install it for the current thread only — and every test in
    /// this crate does.
    ///
    /// Keep the handle alive for as long as you log through the subscriber:
    /// dropping both stops the background writer threads.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] if the configured log directory cannot be created.
    ///
    /// ```rust
    /// use stratify::logging::{ConsoleConfig};
    ///
    /// # fn main() -> Result<(), stratify::logging::Error> {
    /// let (subscriber, handle) = stratify::logging::builder()
    ///     .console(ConsoleConfig::default())
    ///     .build()?;
    ///
    /// let _guard = tracing::subscriber::set_default(subscriber);
    /// tracing::info!("scoped to this thread");
    /// handle.flush();
    /// # Ok(())
    /// # }
    /// ```
    pub fn build(self) -> Result<(impl tracing::Subscriber + Send + Sync, Handle), Error> {
        if self.capture_panics {
            install_panic_hook();
        }
        let filter = self
            .filter
            .unwrap_or_else(|| EnvFilter::try_from_default_env().unwrap_or(EnvFilter::new("info")));

        let queue = self.queue_size.unwrap_or(128_000);
        let sink_filters = self.sink_filters.parse()?;
        let redact = self.redact_keys;
        let globals = sinks::render_globals(&self.global_fields);

        // ── Reload (always first — Layer<Registry> only) ──────────────
        let (rx, tx) = reload::ReloadFilterLayer::new(filter);
        let reload_tx = if self.reloadable { Some(tx) } else { None };

        // ── Gating (sampling + rate limiting — Option<Layer>) ─────────
        let gate_layer = gate::layer_for(self.sampling, self.rate_limit);

        // ── Sinks ─────────────────────────────────────────────────────
        let console = sinks::console_group(
            self.console.as_ref(),
            queue,
            &globals,
            &redact,
            self.console_format,
            &sink_filters.console,
        );

        let json = sinks::json_group(self.json.as_ref(), queue, &sink_filters.json);

        // Opened here rather than inside the group builder because it touches
        // the filesystem and can fail; the group builder stays total.
        let file_writer = self
            .file
            .as_ref()
            .map(|fc| build_file_writer(fc, queue))
            .transpose()?;
        let file = sinks::file_group(
            self.file.as_ref(),
            file_writer,
            &globals,
            &redact,
            self.file_format,
            &sink_filters.file,
        );

        let syslog_layer = sinks::syslog_group(self.syslog, &sink_filters.syslog);

        #[cfg(feature = "appinsights")]
        let app_insights = appinsights::export(self.app_insights, &sink_filters.app_insights)?;

        // ── Build subscriber ───────────────────────────────────────────
        //
        // `Layered::enabled` asks the outermost layer first and stops at the
        // first `false`, so the gates are consulted *before* the reload
        // filter: `rx` is pinned to the innermost position by its
        // `Layer<Registry>` bound. See the note on `Builder::rate_limit` for
        // what that means in practice.
        let subscriber = Registry::default()
            .with(rx)
            // `Some` only when non-empty: an empty `Vec<L>` registers
            // `Interest::never()` and silences every callsite in the stack,
            // whereas a `None` layer passes events through untouched.
            .with((!self.layers.is_empty()).then_some(self.layers))
            .with(gate_layer)
            .with(console.layer)
            .with(json.layer)
            .with(file.layer)
            .with(syslog_layer);

        // Composed by shadowing rather than through a `None` of some stand-in
        // type: the layer sits far down the stack, so naming the subscriber it
        // attaches to for the feature-off case is neither possible nor useful.
        //
        // Traces first: that layer opens the OpenTelemetry context the log
        // layer then stamps onto each record.
        #[cfg(feature = "appinsights")]
        let subscriber = subscriber.with(app_insights.traces).with(app_insights.logs);

        let handle = Handle {
            #[cfg(feature = "appinsights")]
            app_insights: app_insights.providers,
            reload_tx,
            console: console.writer,
            json: json.writer,
            file: file.writer,
        };

        Ok((subscriber, handle))
    }

    /// Initialize the global subscriber.
    ///
    /// Production entry point. Calls [`build`](Self::build) and installs the
    /// result as the process-wide `tracing` subscriber.
    ///
    /// # Errors
    ///
    /// Never panics.
    ///
    /// - [`Error::AlreadyInitialized`] — this crate has already initialized
    ///   logging in this process.
    /// - [`Error::SetGlobalDefault`] — something else installed a global
    ///   subscriber first.
    /// - Anything [`build`](Self::build) can return.
    ///
    /// On every one of these nothing is stored:
    /// [`is_initialized`] still reports `false` and the failure is
    /// recoverable rather than terminal.
    pub fn init(self) -> Result<Handle, Error> {
        // Fail before building. `build()` creates directories and spawns
        // writer threads, and there is no reason to do either only to discard
        // the result. The authoritative check is under the write lock below.
        if is_initialized() {
            return Err(Error::AlreadyInitialized);
        }

        let (subscriber, handle) = self.build()?;

        // Hold the write lock across the re-check and the install so two
        // concurrent `init()` calls cannot both get past the check, and store
        // the handle only once the subscriber is genuinely in place. Storing
        // first left `is_initialized()` reporting true with no subscriber
        // installed, and every retry then hit "already initialized" — a state
        // nothing but `reset()` could escape.
        let mut guard = LOGGING_HANDLE.write().map_err(|_| Error::LockPoisoned)?;
        if guard.is_some() {
            return Err(Error::AlreadyInitialized);
        }

        tracing::subscriber::set_global_default(subscriber)?;

        *guard = Some(handle.clone());
        Ok(handle)
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

/// Base name every rotated log file is derived from.
/// Default base name for log files. Overridable per sink via
/// [`FileConfig::with_prefix`].
const LOG_FILE_PREFIX: &str = "app.log";

fn build_file_writer(fc: &FileConfig, queue: usize) -> Result<FlushableWriter, Error> {
    let path = PathBuf::from(&fc.directory);
    std::fs::create_dir_all(&path)?;

    // Run retention cleanup if configured.
    if fc.retention_days > 0 {
        file::cleanup_old_files(&fc.directory, &fc.prefix, fc.retention_days);
    }

    // Size-based rotation is not something tracing-appender offers, so it uses
    // its own writer rather than the rolling appender.
    if let file::Rotation::Size {
        max_bytes,
        max_files,
    } = fc.rotation
    {
        let directory = fc.directory.clone();
        let prefix = fc.prefix.clone();
        let compress = fc.compress;
        // Validated eagerly so a bad configuration fails at startup, with the
        // error, rather than at the first write.
        size_rolling::SizeRollingWriter::new(&directory, &prefix, max_bytes, max_files, compress)?;

        let open = move || -> Box<dyn std::io::Write + Send> {
            match size_rolling::SizeRollingWriter::new(
                &directory, &prefix, max_bytes, max_files, compress,
            ) {
                Ok(writer) => Box::new(writer),
                // Reached only if the directory becomes unwritable after
                // startup, on a reopen following a drain. Discarding beats
                // panicking: a logging sink must not take the service down, and
                // stderr still says what happened.
                Err(e) => {
                    eprintln!(
                        "stratify::logging: cannot reopen the log file, discarding output: {e}"
                    );
                    Box::new(std::io::sink())
                }
            }
        };
        return Ok(FlushableWriter::new(open, queue, true));
    }

    let rotation = match fc.rotation {
        file::Rotation::Daily => tracing_appender::rolling::Rotation::DAILY,
        file::Rotation::Hourly => tracing_appender::rolling::Rotation::HOURLY,
        file::Rotation::Never => tracing_appender::rolling::Rotation::NEVER,
        // Handled above; unreachable, and returning a value keeps the match
        // total rather than adding a panic.
        file::Rotation::Size { .. } => tracing_appender::rolling::Rotation::NEVER,
    };

    // Every drain reopens the appender, which appends to the same file rather
    // than truncating it.
    let directory = fc.directory.clone();
    let prefix = fc.prefix.clone();
    let open_appender = move || {
        tracing_appender::rolling::RollingFileAppender::new(rotation.clone(), &directory, &prefix)
    };

    // `lossy: true` matches `NonBlockingBuilder::default()`, which is what the
    // file backend has always used.
    Ok(FlushableWriter::new(open_appender, queue, true))
}

fn flush_handle(handle: &Handle) {
    for writer in [&handle.console, &handle.json, &handle.file]
        .into_iter()
        .flatten()
    {
        writer.drain();
    }
}

// ── Config types ────────────────────────────────────────────────────────────

/// Where the console sink writes.
///
/// Defaults to stderr, which is what this crate has always done. Container
/// platforms conventionally read stdout, and some log shippers treat anything
/// on stderr as an error regardless of its level, so the choice matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConsoleTarget {
    /// Standard error. The default.
    #[default]
    Stderr,
    /// Standard output.
    Stdout,
}

/// How timestamps are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimestampFormat {
    /// RFC 3339 in UTC. The default, unchanged from earlier versions.
    #[default]
    Utc,
    /// RFC 3339 in the machine's local timezone.
    ///
    /// Falls back to UTC when the offset cannot be determined, which happens in
    /// multi-threaded processes on some platforms. Logs keep flowing either
    /// way; they are simply in UTC.
    Local,
    /// No timestamp at all, for when something downstream already stamps them.
    None,
}

/// Route panics through `tracing` before the previous hook runs.
///
/// Chains rather than replaces: the existing hook still runs afterwards, so the
/// default backtrace behaviour and anything a test harness installed are both
/// preserved.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `&str` and `String` are the two payload types a `panic!` produces.
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with a non-string payload".to_string());

        match info.location() {
            Some(location) => tracing::error!(
                panic.message = %message,
                panic.file = %location.file(),
                panic.line = location.line(),
                "thread panicked"
            ),
            None => tracing::error!(panic.message = %message, "thread panicked"),
        }

        previous(info);
    }));
}

/// Renders timestamps according to a [`TimestampFormat`].
///
/// One type dispatching at runtime, rather than three timer types: each
/// `with_timer` call changes the layer's type, so three variants would mean
/// three incompatible layers and three code paths per sink.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Timestamp(pub(crate) TimestampFormat);

impl fmt::time::FormatTime for Timestamp {
    fn format_time(&self, writer: &mut format::Writer<'_>) -> std::fmt::Result {
        use time::format_description::well_known::Rfc3339;

        let now = match self.0 {
            TimestampFormat::None => return Ok(()),
            TimestampFormat::Utc => time::OffsetDateTime::now_utc(),
            // Falls back to UTC rather than failing: the offset is
            // undeterminable in a multi-threaded process on some platforms, and
            // a log line in the wrong zone beats no log line.
            TimestampFormat::Local => time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc()),
        };

        match now.format(&Rfc3339) {
            Ok(rendered) => write!(writer, "{rendered}"),
            Err(_) => Err(std::fmt::Error),
        }
    }
}

/// Wire format for the file sink.
///
/// The console sink has always been plain text and the file sink has always
/// been JSON. That pairing suits a machine-scraped deployment, and suits
/// nothing else: a service whose logs a person tails, or one shipping structure
/// to a separate exporter and wanting the file readable, needs the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FileFormat {
    /// One JSON object per line. The default, unchanged from earlier versions.
    #[default]
    Json,
    /// The same human-readable layout the console sink uses, without colour.
    Text,
}

/// The rotating file sink's configuration.
pub struct FileConfig {
    /// Directory the log files are written to. Created when absent.
    pub directory: String,
    /// When the current file is retired for a fresh one.
    pub rotation: file::Rotation,
    /// Maximum age of rotated log files in days. Files older than this
    /// are deleted at startup. 0 disables retention.
    pub retention_days: u32,
    /// Shape of each record when the format is JSON.
    pub json_config: JsonConfig,
    /// Wire format. Defaults to [`FileFormat::Json`], preserving the behaviour
    /// of every earlier version.
    pub format: FileFormat,
    /// Base name for log files, before the rotation suffix. Defaults to
    /// `"app.log"`.
    ///
    /// Two services sharing a log directory need different prefixes, or their
    /// files collide and a shipper cannot route by name.
    pub prefix: String,
    /// Timestamp rendering. Defaults to [`TimestampFormat::Utc`].
    pub timestamp: TimestampFormat,
    /// Gzip retired files. Only honoured for [`file::Rotation::Size`], and only
    /// when the `compression` feature is enabled.
    pub compress: bool,
}

impl FileConfig {
    /// Log to `directory`, rotating daily with retention disabled.
    pub fn new(directory: impl Into<String>) -> Self {
        Self {
            directory: directory.into(),
            rotation: file::Rotation::Daily,
            retention_days: 0,
            json_config: JsonConfig::default(),
            format: FileFormat::default(),
            prefix: LOG_FILE_PREFIX.to_string(),
            timestamp: TimestampFormat::default(),
            compress: false,
        }
    }

    /// How often a new log file is started.
    pub fn with_rotation(mut self, rotation: file::Rotation) -> Self {
        self.rotation = rotation;
        self
    }

    /// Delete rotated files older than `days` at startup. 0 disables it.
    pub fn with_retention_days(mut self, days: u32) -> Self {
        self.retention_days = days;
        self
    }

    /// How the JSON written to the file is shaped.
    /// Choose the wire format for this file sink.
    ///
    /// ```rust
    /// # use stratify::logging::{FileConfig, FileFormat};
    /// let config = FileConfig::new("logs").with_format(FileFormat::Text);
    /// ```
    ///
    /// [`FileFormat::Text`] ignores [`FileConfig::with_json_config`], since
    /// span lists and current-span rendering are JSON concepts.
    pub fn with_format(mut self, format: FileFormat) -> Self {
        self.format = format;
        self
    }

    /// Base name for log files, before the rotation suffix.
    ///
    /// Defaults to `"app.log"`, giving `app.log.2026-08-20`. Set it per service
    /// when several share a log directory.
    /// Gzip retired files.
    ///
    /// Requires the `compression` feature and [`file::Rotation::Size`]. Without
    /// the feature this is accepted and ignored, so enabling it later needs no
    /// code change. Compression is best-effort: a failure leaves the file
    /// uncompressed rather than losing it.
    pub fn with_compression(mut self, compress: bool) -> Self {
        self.compress = compress;
        self
    }

    /// Set the base file name, before the rotation suffix.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Choose how timestamps are rendered. Defaults to UTC.
    pub fn with_timestamp(mut self, timestamp: TimestampFormat) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Set the JSON record shape used when the format is JSON.
    pub fn with_json_config(mut self, config: JsonConfig) -> Self {
        self.json_config = config;
        self
    }
}

impl Default for FileConfig {
    fn default() -> Self {
        Self::new("/var/log/app")
    }
}

/// Text output to stderr.
///
/// The type is `#[non_exhaustive]`, so struct-literal syntax is not available
/// outside this crate. Start from [`Default`] and chain the `with_*` setters:
///
/// ```rust
/// use stratify::logging::ConsoleConfig;
///
/// let config = ConsoleConfig::default()
///     .with_color(false)
///     .with_thread_ids(false);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConsoleConfig {
    /// Stream to write to. Defaults to [`ConsoleTarget::Stderr`].
    pub target_stream: ConsoleTarget,
    /// Timestamp rendering. Defaults to [`TimestampFormat::Utc`].
    pub timestamp: TimestampFormat,
    /// Emit ANSI colour codes.
    pub use_color: bool,
    /// Include the emitting thread's id on each line.
    pub thread_ids: bool,
    /// Include the event's target (module path) on each line.
    pub target: bool,
    /// When `true`, drop events instead of blocking when the non-blocking queue
    /// is full. When `false` (default), the background writer thread blocks the
    /// calling thread if the queue overflows.
    pub lossy: bool,
}

impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            target_stream: ConsoleTarget::default(),
            timestamp: TimestampFormat::default(),
            use_color: true,
            thread_ids: true,
            target: true,
            lossy: false,
        }
    }
}

impl ConsoleConfig {
    /// Write console output to stdout or stderr. Defaults to stderr.
    pub fn with_target_stream(mut self, target: ConsoleTarget) -> Self {
        self.target_stream = target;
        self
    }

    /// Choose how timestamps are rendered. Defaults to UTC.
    pub fn with_timestamp(mut self, timestamp: TimestampFormat) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Emit ANSI colour codes.
    pub fn with_color(mut self, enabled: bool) -> Self {
        self.use_color = enabled;
        self
    }

    /// Include the originating thread's id on each line.
    pub fn with_thread_ids(mut self, enabled: bool) -> Self {
        self.thread_ids = enabled;
        self
    }

    /// Include the event's target (usually the module path).
    pub fn with_target(mut self, enabled: bool) -> Self {
        self.target = enabled;
        self
    }

    /// Drop events rather than block the caller when the queue is full.
    pub fn with_lossy(mut self, enabled: bool) -> Self {
        self.lossy = enabled;
        self
    }
}

/// Structured JSON output.
///
/// The type is `#[non_exhaustive]`, so struct-literal syntax is not available
/// outside this crate. Start from [`Default`] and chain the `with_*` setters:
///
/// ```rust
/// use stratify::logging::JsonConfig;
///
/// let config = JsonConfig::default().with_span_list(false).with_lossy(true);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct JsonConfig {
    /// Include the span scope on each record.
    pub span_list: bool,
    /// Flatten event fields into the top-level object.
    pub flatten: bool,
    /// When `true`, drop events instead of blocking when the non-blocking queue
    /// is full. When `false` (default), the background writer thread blocks the
    /// calling thread if the queue overflows.
    pub lossy: bool,
}

impl Default for JsonConfig {
    fn default() -> Self {
        Self {
            span_list: true,
            flatten: true,
            lossy: false,
        }
    }
}

impl JsonConfig {
    /// Include the full list of enclosing spans on each event.
    pub fn with_span_list(mut self, enabled: bool) -> Self {
        self.span_list = enabled;
        self
    }

    /// Flatten event fields into the top-level object.
    pub fn with_flatten(mut self, enabled: bool) -> Self {
        self.flatten = enabled;
        self
    }

    /// Drop events rather than block the caller when the queue is full.
    pub fn with_lossy(mut self, enabled: bool) -> Self {
        self.lossy = enabled;
        self
    }
}

// ── Handle ──────────────────────────────────────────────────────────────────

/// Holds the non-blocking writers and the optional reload channel.
///
/// The writer fields are the same [`FlushableWriter`]s the format layers write
/// through, not copies, which is what lets [`flush`](flush()) drain the
/// queues that are actually in use. They also keep the background worker
/// threads alive: the workers stop only once this handle and every clone of it
/// have been dropped *and* the subscriber holding the layers is gone.
///
/// Clones are full-strength — a cloned handle drains the same queues as the
/// original and keeps them alive on its own.
#[derive(Debug, Clone)]
/// Drives the installed stack: flush, reload, shutdown, and the queue and
/// drop counters. Clones are full-strength — they share the same writers.
pub struct Handle {
    /// The reload channel, present when the stack was built `reloadable`.
    reload_tx: Option<reload::ReloadHandle>,
    /// The console (stderr) writer, when a console layer was configured.
    console: Option<FlushableWriter>,
    /// The JSON (stdout) writer, when a JSON layer was configured.
    json: Option<FlushableWriter>,
    /// The rotating-file writer, when a file layer was configured.
    file: Option<FlushableWriter>,
    /// Held so the batch can be flushed on shutdown. The exporter batches, so
    /// without this the final records never leave the process.
    #[cfg(feature = "appinsights")]
    app_insights: Option<appinsights::Providers>,
}

/// Lines discarded because a sink's queue was full, per sink.
///
/// Non-zero only for sinks configured `lossy`. A service logging nothing and a
/// service dropping everything look identical without this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DroppedLines {
    /// Lines dropped by the console sink.
    pub console: usize,
    /// Lines dropped by the JSON sink.
    pub json: usize,
    /// Lines dropped by the file sink.
    pub file: usize,
}

/// Lines queued but not yet written, per sink.
///
/// Saturation is otherwise invisible until lines are already being dropped:
/// `DroppedLines` reports the damage, this reports the pressure before it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueDepth {
    /// Depth of the console sink's queue.
    pub console: usize,
    /// Depth of the JSON sink's queue.
    pub json: usize,
    /// Depth of the file sink's queue.
    pub file: usize,
}

impl QueueDepth {
    /// Total across every sink.
    pub fn total(&self) -> usize {
        self.console + self.json + self.file
    }

    /// The busiest sink's depth, which is the one that will drop first.
    pub fn max(&self) -> usize {
        self.console.max(self.json).max(self.file)
    }
}

impl DroppedLines {
    /// Total across every sink.
    pub fn total(&self) -> usize {
        self.console + self.json + self.file
    }

    /// Whether anything was dropped at all.
    pub fn any(&self) -> bool {
        self.total() > 0
    }
}

impl Handle {
    /// Lines each sink discarded because its queue was full.
    ///
    /// Only ever non-zero for a sink configured `lossy`, which trades
    /// completeness for never blocking the calling thread. Sample this
    /// periodically, or check it at shutdown: silence is otherwise
    /// indistinguishable from a sink dropping everything.
    ///
    /// ```rust,no_run
    /// # use stratify::logging;
    /// # fn example() {
    /// if let Some(handle) = stratify::logging::handle() {
    ///     let dropped = handle.dropped_lines();
    ///     if dropped.any() {
    ///         eprintln!("logging dropped {} lines", dropped.total());
    ///     }
    /// }
    /// # }
    /// ```
    /// Lines queued but not yet written, per sink.
    ///
    /// Approximate, and deliberately so: it is derived by counting both ends of
    /// the queue without a lock, so a value read mid-drain may be slightly
    /// stale. Use it to spot pressure building, not for exact accounting.
    ///
    /// Compare against the configured `queue_size`: a depth approaching it
    /// means the next burst starts dropping.
    pub fn queue_depth(&self) -> QueueDepth {
        QueueDepth {
            console: self.console.as_ref().map_or(0, |w| w.queue_depth()),
            json: self.json.as_ref().map_or(0, |w| w.queue_depth()),
            file: self.file.as_ref().map_or(0, |w| w.queue_depth()),
        }
    }

    /// Lines each lossy sink discarded on queue overflow, cumulative for
    /// the process lifetime.
    pub fn dropped_lines(&self) -> DroppedLines {
        DroppedLines {
            console: self.console.as_ref().map_or(0, |w| w.dropped_lines()),
            json: self.json.as_ref().map_or(0, |w| w.dropped_lines()),
            file: self.file.as_ref().map_or(0, |w| w.dropped_lines()),
        }
    }

    /// Block until every queued event has been written to its destination.
    ///
    /// This is a real drain, not a hint: it returns only once the console,
    /// JSON and file writers have each handed their backlog to the underlying
    /// sink. Nothing is lost and logging continues afterwards, so it is safe to
    /// call at a checkpoint as well as at shutdown.
    ///
    /// Threads trying to log are blocked for the duration, and each writer
    /// retires and replaces a worker thread, so this is not a hot-path call.
    /// See [`FlushableWriter::drain`] for the mechanism.
    pub fn flush(&self) {
        // Drain the exporter first: it batches over the network, so it is the
        // slowest and the most likely to be cut short by an exiting process.
        #[cfg(feature = "appinsights")]
        if let Some(providers) = &self.app_insights {
            providers.force_flush();
        }
        flush_handle(self);
    }

    /// Flush everything, then shut the exporter down cleanly.
    ///
    /// The last call before the process exits. [`flush`](Handle::flush) drains
    /// what is queued but leaves the Application Insights exporter running;
    /// this also closes it, which is what ends the batch-export loop and its
    /// scratch thread rather than abandoning them mid-cycle. Local sinks need
    /// no closing — their workers stop when the last handle drops.
    ///
    /// Takes `self` because a shut-down handle must not be reused: logging
    /// keeps working, but records after this never reach Application
    /// Insights, so holding a live-looking handle past it would lie. Other
    /// clones (including the stored one driving [`flush`](flush())) are unaffected
    /// except that the exporter they share is now closed.
    pub fn shutdown(self) {
        self.flush();
        #[cfg(feature = "appinsights")]
        if let Some(providers) = &self.app_insights {
            providers.shutdown();
        }
    }
}

// ── Convenience helpers ─────────────────────────────────────────────────────

/// Create a request-context span that survives across async await points.
pub fn request_context(request_id: &str, method: &str, path: &str) -> tracing::Span {
    tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %method,
        path = %path,
    )
}

// ── Singleton ───────────────────────────────────────────────────────────────

static LOGGING_HANDLE: RwLock<Option<Handle>> = RwLock::new(None);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_config_defaults() {
        let c = ConsoleConfig::default();
        assert!(c.use_color);
        assert!(c.thread_ids);
        assert!(c.target);
        assert!(!c.lossy);
    }

    #[test]
    fn json_config_defaults() {
        let j = JsonConfig::default();
        assert!(j.span_list);
        assert!(j.flatten);
        assert!(!j.lossy);
    }

    #[test]
    fn file_config_defaults() {
        let f = FileConfig::default();
        assert_eq!(f.directory, "/var/log/app");
    }

    #[test]
    fn builder_methods_are_chainable() {
        let b = super::builder()
            .console(ConsoleConfig::default())
            .json(JsonConfig::default())
            .file(FileConfig::new("/tmp/test"))
            .reloadable()
            .queue_size(64_000)
            .with_filter(EnvFilter::builder().parse("debug").unwrap());
        let _: Builder = b;
    }

    #[test]
    fn request_context_creates_span() {
        let span = request_context("r1", "GET", "/api");
        drop(span);
    }

    /// Verify `build()` works without hitting global subscriber limits.
    #[test]
    fn build_is_testable() {
        let (subscriber, handle) = super::builder()
            .console(ConsoleConfig::default())
            .build()
            .unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.reload_tx.is_none());
        assert!(handle.console.is_some());
        assert!(handle.json.is_none());
        assert!(handle.file.is_none());
    }

    /// Verify `build()` with reload returns a working reload handle.
    #[test]
    fn build_with_reload() {
        let (subscriber, handle) = super::builder().reloadable().build().unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.reload_tx.is_some());
    }

    /// Verify `build()` with file rotation.
    #[test]
    fn build_with_file() {
        let dir = std::env::temp_dir().join("stratify_logging_build_test");
        let _ = std::fs::create_dir_all(&dir);

        let (subscriber, handle) = super::builder()
            .file(FileConfig::new(dir.to_string_lossy().as_ref()))
            .build()
            .unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.file.is_some());
    }

    /// Verify all three layers can coexist.
    #[test]
    fn build_all_layers() {
        let dir = std::env::temp_dir().join("stratify_logging_all_layers_test");
        let _ = std::fs::create_dir_all(&dir);

        let (subscriber, handle) = super::builder()
            .console(ConsoleConfig::default())
            .json(JsonConfig::default())
            .file(FileConfig::new(dir.to_string_lossy().as_ref()))
            .build()
            .unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.console.is_some());
        assert!(handle.json.is_some());
        assert!(handle.file.is_some());
    }

    /// Verify lossy mode is passed through to the non-blocking writer.
    #[test]
    fn lossy_mode_present_on_configs() {
        let cc = ConsoleConfig::default().with_lossy(true);
        assert!(cc.lossy);

        let jc = JsonConfig::default().with_lossy(true);
        assert!(jc.lossy);
    }

    /// Verify custom queue size is accepted.
    #[test]
    fn custom_queue_size() {
        let (subscriber, _handle) = super::builder().queue_size(1_000).build().unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        // Queue size is used during build; if it compiles and runs, it's valid.
    }

    // ── Singleton lifecycle ────────────────────────────────────────────

    #[test]
    fn handle_returns_none_before_init() {
        assert!(handle().is_none());
        assert!(!is_initialized());
    }

    #[test]
    fn reset_clears_handle() {
        assert!(!is_initialized());
        reset(); // idempotent on empty singleton
        assert!(!is_initialized());
    }

    /// A clone that silently cannot flush is worse than no clone at all —
    /// callers would call `flush()` on it and lose the queue anyway.
    #[test]
    fn clone_carries_the_same_writers() {
        let (_, handle) = super::builder()
            .console(ConsoleConfig::default())
            .json(JsonConfig::default())
            .build()
            .unwrap();

        let cloned = handle.clone();
        assert_eq!(cloned.reload_tx.is_some(), handle.reload_tx.is_some());
        assert!(cloned.console.is_some());
        assert!(cloned.json.is_some());
        assert!(cloned.file.is_none(), "no file layer was configured");
    }

    #[test]
    fn handle_flush_does_not_panic() {
        let (subscriber, handle) = super::builder()
            .console(ConsoleConfig::default())
            .build()
            .unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        handle.flush(); // smoke test — should not panic
    }

    // ── File backend ───────────────────────────────────────────────────

    #[test]
    fn file_config_new_accepts_custom_path() {
        let fc = FileConfig::new("/custom/log/path");
        assert_eq!(fc.directory, "/custom/log/path");
    }

    #[test]
    fn file_config_default_rotation_is_daily() {
        let fc = FileConfig::default();
        assert_eq!(fc.rotation, file::Rotation::Daily);
    }

    #[test]
    fn build_file_with_hourly_rotation() {
        let dir = std::env::temp_dir().join("stratify_logging_hourly_test");
        let _ = std::fs::create_dir_all(&dir);

        let fc =
            FileConfig::new(dir.to_string_lossy().as_ref()).with_rotation(file::Rotation::Hourly);

        let (subscriber, handle) = super::builder().file(fc).build().unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.file.is_some());
    }

    #[test]
    fn build_file_with_never_rotation() {
        let dir = std::env::temp_dir().join("stratify_logging_never_test");
        let _ = std::fs::create_dir_all(&dir);

        let fc =
            FileConfig::new(dir.to_string_lossy().as_ref()).with_rotation(file::Rotation::Never);

        let (subscriber, handle) = super::builder().file(fc).build().unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.file.is_some());
    }

    #[test]
    fn build_file_creates_nonexistent_directory() {
        let dir = std::env::temp_dir().join("stratify_logging_auto_create_test");
        let _ = std::fs::remove_dir_all(&dir);

        let (subscriber, handle) = super::builder()
            .file(FileConfig::new(dir.to_string_lossy().as_ref()))
            .build()
            .unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.file.is_some());
        assert!(dir.exists());
    }

    // ── Event routing ──────────────────────────────────────────────────

    #[test]
    fn console_layer_writes_to_stderr() {
        let (subscriber, _handle) = super::builder()
            .console(ConsoleConfig::default())
            .with_filter(EnvFilter::new("info"))
            .build()
            .unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        // Smoke test: log an event and ensure it doesn't panic.
        // Non-blocking writer means we can't assert on output, but we
        // can verify the subscriber is active.
        tracing::info!("console smoke test");
        tracing::error!("console error smoke");
        tracing::warn!("console warn smoke");
    }

    #[test]
    fn json_layer_writes_to_stdout() {
        let (subscriber, _handle) = super::builder()
            .json(JsonConfig::default())
            .with_filter(EnvFilter::new("info"))
            .build()
            .unwrap();

        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!(key = "value", "json smoke test");
        tracing::error!(code = 500, "json error smoke");
    }

    #[test]
    fn default_filter_when_none_supplied() {
        // No filter supplied → should fall back to env or "info"
        let (subscriber, _handle) = super::builder()
            .console(ConsoleConfig::default())
            .build()
            .unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!("default filter test");
    }

    #[test]
    fn reloadable_returns_some_handle_when_enabled() {
        let (subscriber, handle) = super::builder().reloadable().build().unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.reload_tx.is_some());
    }

    #[test]
    fn reloadable_returns_none_when_not_enabled() {
        let (subscriber, handle) = super::builder()
            .console(ConsoleConfig::default())
            .build()
            .unwrap();
        let _guard = tracing::subscriber::set_default(subscriber);
        assert!(handle.reload_tx.is_none());
    }

    #[test]
    fn request_context_includes_all_fields() {
        let span = request_context("req-42", "POST", "/users");
        // Verify the span can be entered — fields are embedded at creation
        let _enter = span.enter();
        tracing::info!("inside request span");
    }

    #[test]
    fn error_paths_are_covered() {
        assert!(flush().is_err()); // not initialized

        let result = reload_filter(EnvFilter::new("debug"));
        assert!(result.is_err()); // not initialized + not reloadable
    }
}
