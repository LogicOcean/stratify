//! Sink construction, extracted from `Builder::build`.
//!
//! Each function returns an `Option<impl Layer<S>>` so the concrete
//! `fmt::Layer` types never have to be named. That is why this logic lived
//! inline in `build` in the first place, and it is why `build` grew past two
//! hundred lines as sinks were added.
//!
//! `None` rather than an empty layer throughout: a `None` layer passes events
//! through untouched, whereas an empty `Vec<L>` registers `Interest::never()`
//! and silences the entire stack.

use std::io;

use tracing::Subscriber;
use tracing_subscriber::fmt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer};

use super::flush::FlushableWriter;
use super::{BoxedFormat, Timestamp, WithGlobals};
use super::{ConsoleConfig, ConsoleTarget, EventFormatter};

/// What a sink builder hands back: the layer, plus the writer to flush it with.
pub(super) struct Sink<L> {
    pub(super) layer: Option<L>,
    pub(super) writer: Option<FlushableWriter>,
}

impl<L> Sink<L> {
    /// No layer and no writer, for a shape this sink was not asked to take.
    fn none() -> Self {
        Self {
            layer: None,
            writer: None,
        }
    }
}

/// `DefaultFields` under a different type, so file sinks get their own span
/// cache.
///
/// `tracing-subscriber` formats a span's fields once per *field-formatter
/// type* and caches the result in the span's extensions. With every text sink
/// on `DefaultFields`, they all share one cache, so whichever layer touches a
/// span first decides — with its own ANSI setting — what every other layer
/// prints for that span. That is how a coloured console used to write escape
/// codes into the log file. A distinct type is a distinct cache slot: file
/// sinks always cache plain, and the console is free to colour its own.
#[derive(Default)]
pub(super) struct PlainFields(fmt::format::DefaultFields);

impl<'writer> fmt::FormatFields<'writer> for PlainFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        writer: fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        self.0.format_fields(writer, fields)
    }
}

/// The console sink, using the built-in layout.
///
/// Returns nothing when a caller-supplied formatter is in play; that case is
/// [`console_custom`] instead, because the two produce different layer types.
pub(super) fn console_builtin<S>(
    config: &ConsoleConfig,
    queue: usize,
    globals: &str,
    redact: &[String],
    has_custom_format: bool,
) -> Sink<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if has_custom_format {
        return Sink::none();
    }

    let writer = make_console_writer(config, queue);
    let built_in = fmt::format::Format::default()
        .with_ansi(config.use_color)
        .with_thread_ids(config.thread_ids)
        .with_target(config.target)
        .with_level(true)
        .with_timer(Timestamp(config.timestamp));

    Sink {
        layer: Some(
            fmt::layer()
                .with_writer(writer.clone())
                // On the *layer*, not only the format: the layer's flag is
                // what controls how span fields are cached into the span's
                // extensions, and the cached string is printed verbatim by
                // every event that closes over the span.
                .with_ansi(config.use_color)
                .event_format(WithGlobals {
                    inner: built_in,
                    prefix: globals.to_string(),
                    redact: redact.to_vec(),
                }),
        ),
        writer: Some(writer),
    }
}

/// The console sink, using a caller-supplied formatter.
pub(super) fn console_custom<S>(
    config: &ConsoleConfig,
    queue: usize,
    globals: &str,
    redact: &[String],
    format: Option<EventFormatter>,
) -> Sink<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let Some(format) = format else {
        return Sink::none();
    };

    let writer = make_console_writer(config, queue);
    Sink {
        layer: Some(
            fmt::layer()
                .with_writer(writer.clone())
                // Plain span cache: a custom formatter styles its own line,
                // and the `SpanScope` it receives is rendered from this cache.
                .with_ansi(false)
                .event_format(WithGlobals {
                    inner: BoxedFormat(format),
                    prefix: globals.to_string(),
                    redact: redact.to_vec(),
                }),
        ),
        writer: Some(writer),
    }
}

/// Stdout or stderr, per configuration.
fn make_console_writer(config: &ConsoleConfig, queue: usize) -> FlushableWriter {
    match config.target_stream {
        ConsoleTarget::Stderr => FlushableWriter::new(io::stderr, queue, config.lossy),
        ConsoleTarget::Stdout => FlushableWriter::new(io::stdout, queue, config.lossy),
    }
}

/// The JSON sink, which has no formatter or global-field variants.
///
/// Both are text concerns: a custom layout or a prefix would stop the output
/// being JSON, which is the only reason to choose this sink.
pub(super) fn json<S>(config: &super::JsonConfig, queue: usize) -> Sink<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let writer = FlushableWriter::new(io::stdout, queue, config.lossy);
    Sink {
        layer: Some(
            fmt::layer()
                .json()
                .with_writer(writer.clone())
                .with_current_span(true)
                .with_span_list(config.span_list)
                .with_timer(fmt::time::UtcTime::rfc_3339()),
        ),
        writer: Some(writer),
    }
}

/// The syslog sink.
///
/// A writer is built per event so each message carries its own severity, via
/// `MakeWriter::make_writer_for`. No timestamp or level in the payload: syslog
/// derives both from the priority, and repeating them only adds noise.
pub(super) fn syslog<S>(config: super::syslog::SyslogConfig) -> impl Layer<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    struct MakeSyslog(super::syslog::SyslogConfig);

    impl<'a> fmt::MakeWriter<'a> for MakeSyslog {
        type Writer = super::syslog::SyslogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            super::syslog::SyslogWriter::connect(&self.0, tracing::Level::INFO)
        }

        fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
            super::syslog::SyslogWriter::connect(&self.0, *meta.level())
        }
    }

    fmt::layer()
        .with_writer(MakeSyslog(config))
        .with_ansi(false)
        .with_level(false)
        .without_time()
}

/// The file sink in JSON form.
pub(super) fn file_json<S>(
    config: &super::FileConfig,
    writer: FlushableWriter,
) -> Option<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if config.format != super::FileFormat::Json {
        return None;
    }
    Some(
        fmt::layer()
            .json()
            .with_writer(writer)
            .with_current_span(true)
            .with_span_list(config.json_config.span_list)
            .with_timer(Timestamp(config.timestamp)),
    )
}

/// The file sink in plain text, using the built-in layout.
pub(super) fn file_text<S>(
    config: &super::FileConfig,
    writer: FlushableWriter,
    globals: &str,
    redact: &[String],
    has_custom_format: bool,
) -> Option<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if config.format != super::FileFormat::Text || has_custom_format {
        return None;
    }
    // No colour: escape codes in a file are noise, and a log shipper reading it
    // later has no terminal to render them.
    let built_in = fmt::format::Format::default()
        .with_ansi(false)
        .with_level(true)
        .with_target(true)
        .with_timer(Timestamp(config.timestamp));

    Some(
        fmt::layer()
            .with_writer(writer)
            // `PlainFields` gives this sink its own span cache, and the layer
            // flag keeps that cache free of escape codes, so a coloured
            // console beside this file cannot bleed ANSI into it.
            .fmt_fields(PlainFields::default())
            .with_ansi(false)
            .event_format(WithGlobals {
                inner: built_in,
                prefix: globals.to_string(),
                redact: redact.to_vec(),
            }),
    )
}

/// The file sink in plain text, using a caller-supplied formatter.
///
/// Wrapped in `WithGlobals` exactly as the console equivalent is. Without that
/// wrapper a custom formatter would silently opt out of global fields *and* of
/// redaction, which is the more serious half: a caller who configured
/// `redact` would reasonably assume it applied everywhere.
pub(super) fn file_custom<S>(
    config: &super::FileConfig,
    writer: FlushableWriter,
    globals: &str,
    redact: &[String],
    format: Option<EventFormatter>,
) -> Option<impl Layer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    if config.format != super::FileFormat::Text {
        return None;
    }
    let format = format?;
    Some(
        fmt::layer()
            .with_writer(writer)
            // Same reasoning as `file_text`: own cache, kept plain.
            .fmt_fields(PlainFields::default())
            .with_ansi(false)
            .event_format(WithGlobals {
                inner: BoxedFormat(format),
                prefix: globals.to_string(),
                redact: redact.to_vec(),
            }),
    )
}

/// Reproduce a per-sink filter, or one that admits everything.
///
/// `EnvFilter` is not `Clone` and each sink needs its own, so this rebuilds one
/// from the rendered directives. Absent becomes `trace` rather than anything
/// narrower: the global filter has already had its say, and a per-sink filter
/// exists to widen or narrow *that sink*, not to re-filter what already passed.
pub(super) fn clone_filter(filter: &Option<EnvFilter>) -> EnvFilter {
    match filter {
        Some(f) => EnvFilter::try_new(f.to_string()).unwrap_or_else(|_| EnvFilter::new("trace")),
        None => EnvFilter::new("trace"),
    }
}

/// Render process-wide fields into the prefix the text sinks prepend.
///
/// Rendered once here rather than per event. An empty result installs no
/// wrapper at all, so the default path stays untouched.
pub(super) fn render_globals(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A sink layer with its concrete type erased.
///
/// Erasure is what lets a group hand back one layer standing for several
/// mutually exclusive shapes. It also breaks an inference cycle: two
/// `impl Layer` values returned from a single call cannot both be attached at
/// different depths of the stack, because each depth would be defined in terms
/// of the other's opaque type. One dynamic dispatch per event is not
/// measurable against the formatting and channel send that follow it.
pub(super) type ErasedLayer<S> = Box<dyn Layer<S> + Send + Sync>;

/// A built sink: the layer to attach, and the writer that flushes it.
pub(super) struct Group<S> {
    pub(super) layer: Option<ErasedLayer<S>>,
    pub(super) writer: Option<FlushableWriter>,
}

impl<S> Group<S> {
    /// Nothing to attach, for a sink that was not configured.
    fn empty() -> Self {
        Self {
            layer: None,
            writer: None,
        }
    }
}

/// Build the console sink, or nothing when no console was configured.
///
/// Absence of a config is what decides, not the presence of a formatter: a
/// formatter set without `.console(...)` must not conjure a sink the caller
/// never asked for. The built-in layout and a caller-supplied formatter produce
/// different layer types, and at most one of them is ever built.
pub(super) fn console_group<S>(
    config: Option<&ConsoleConfig>,
    queue: usize,
    globals: &str,
    redact: &[String],
    format: Option<EventFormatter>,
    filter: &Option<EnvFilter>,
) -> Group<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let Some(config) = config else {
        return Group::empty();
    };

    let custom = console_custom::<S>(config, queue, globals, redact, format);
    let builtin = console_builtin::<S>(config, queue, globals, redact, custom.layer.is_some());

    let layer: Option<ErasedLayer<S>> = match (builtin.layer, custom.layer) {
        (Some(l), _) => Some(Box::new(l.with_filter(clone_filter(filter)))),
        (None, Some(l)) => Some(Box::new(l.with_filter(clone_filter(filter)))),
        (None, None) => None,
    };

    Group {
        layer,
        writer: builtin.writer.or(custom.writer),
    }
}

/// Build the JSON sink, or nothing when no JSON sink was configured.
///
/// Takes neither a formatter nor global fields: both are text concerns, and
/// either would stop the output being JSON.
pub(super) fn json_group<S>(
    config: Option<&super::JsonConfig>,
    queue: usize,
    filter: &Option<EnvFilter>,
) -> Group<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let Some(config) = config else {
        return Group::empty();
    };

    let sink = json::<S>(config, queue);
    Group {
        layer: sink
            .layer
            .map(|l| Box::new(l.with_filter(clone_filter(filter))) as ErasedLayer<S>),
        writer: sink.writer,
    }
}

/// Build the file sink around an already-opened writer.
///
/// The writer is opened by the caller because doing so touches the filesystem
/// and can fail; keeping that `?` outside leaves this function total. JSON,
/// built-in text and custom text produce different layer types, and at most one
/// of them is ever built.
pub(super) fn file_group<S>(
    config: Option<&super::FileConfig>,
    writer: Option<FlushableWriter>,
    globals: &str,
    redact: &[String],
    format: Option<EventFormatter>,
    filter: &Option<EnvFilter>,
) -> Group<S>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let (Some(config), Some(writer)) = (config, writer) else {
        return Group::empty();
    };

    let has_format = format.is_some();
    let json = file_json::<S>(config, writer.clone());
    let text = file_text::<S>(config, writer.clone(), globals, redact, has_format);
    let custom = file_custom::<S>(config, writer.clone(), globals, redact, format);

    let layer: Option<ErasedLayer<S>> = match (json, text, custom) {
        (Some(l), _, _) => Some(Box::new(l.with_filter(clone_filter(filter)))),
        (None, Some(l), _) => Some(Box::new(l.with_filter(clone_filter(filter)))),
        (None, None, Some(l)) => Some(Box::new(l.with_filter(clone_filter(filter)))),
        (None, None, None) => None,
    };

    Group {
        layer,
        writer: Some(writer),
    }
}

/// Build the syslog sink, or nothing when it was not configured.
pub(super) fn syslog_group<S>(
    config: Option<super::syslog::SyslogConfig>,
    filter: &Option<EnvFilter>,
) -> Option<ErasedLayer<S>>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let config = config?;
    Some(Box::new(
        syslog::<S>(config).with_filter(clone_filter(filter)),
    ))
}
