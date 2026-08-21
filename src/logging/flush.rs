//! Non-blocking writers that can actually be drained on demand.
//!
//! # Why this exists
//!
//! `tracing_appender`'s [`NonBlocking`](tracing_appender::non_blocking::NonBlocking) writer hands lines to a background
//! worker thread. Its [`std::io::Write::flush`] is a no-op, and the only
//! synchronisation point it offers is dropping the [`WorkerGuard`](tracing_appender::non_blocking::WorkerGuard): that sends
//! the worker a shutdown message and blocks until every line queued ahead of it
//! has been written and flushed. There is no way to wait for the queue without
//! also ending the worker.
//!
//! [`FlushableWriter`] turns that one-shot into a repeatable operation. It owns
//! the writer/guard pair behind an [`RwLock`]; [`drain`](FlushableWriter::drain)
//! swaps in a fresh pair and then drops the retired one, so the call returns
//! only once the retired worker has written everything — and logging continues
//! afterwards through the replacement.
//!
//! # Cost
//!
//! `drain()` retires a worker thread and starts another, and it holds the write
//! lock while doing so, which briefly blocks threads trying to log. It is a
//! checkpoint and shutdown operation — call it before exiting or after a
//! critical failure, not on the hot path. Writing a line only takes the read
//! lock, so ordinary logging stays uncontended.

use std::io;
use std::sync::{Arc, PoisonError, RwLock, RwLockReadGuard};

use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::fmt::MakeWriter;

/// Builds a fresh worker for the writer to hand lines to.
type SpawnWorker = dyn Fn() -> Generation + Send + Sync;

/// One writer/worker pair. Dropping it drains and retires that worker.
struct Generation {
    writer: NonBlocking,
    _guard: WorkerGuard,
}

/// A [`MakeWriter`] whose background worker can be drained without losing the
/// ability to write afterwards.
///
/// Cloning is cheap and shares the underlying worker, so a clone can drain the
/// same queue as the original and keeps it alive for as long as it lives.
#[derive(Clone)]
pub struct FlushableWriter {
    current: Arc<RwLock<Generation>>,
    spawn: Arc<SpawnWorker>,
    /// Lines handed to the queue, cumulative.
    submitted: Arc<std::sync::atomic::AtomicUsize>,
    /// Lines the background worker has written out, cumulative.
    written: Arc<std::sync::atomic::AtomicUsize>,
    /// Lines dropped by generations that have already been retired.
    ///
    /// Each drain replaces the worker, and the replacement starts its counter
    /// at zero. Without carrying the total forward, a drop count would silently
    /// reset on every flush and always read low.
    retired_drops: Arc<std::sync::atomic::AtomicUsize>,
}

impl FlushableWriter {
    /// Wrap `make_sink` in a non-blocking writer.
    ///
    /// `make_sink` is called once now and once per [`drain`](Self::drain), so
    /// it must be able to produce an equivalent sink repeatedly — reopening the
    /// same file in append mode, or handing back `io::stderr()` again.
    ///
    /// `lossy` selects what happens when the queue is full: `true` drops lines,
    /// `false` blocks the calling thread until there is room.
    pub fn new<W, F>(make_sink: F, queue_size: usize, lossy: bool) -> Self
    where
        W: io::Write + Send + 'static,
        F: Fn() -> W + Send + Sync + 'static,
    {
        // Counting both ends is the only way to see the queue: the underlying
        // non-blocking writer reports what it dropped but never how much is
        // still in flight, so saturation is invisible until it is too late.
        let written = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let submitted_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let written_for_sink = written.clone();

        let spawn: Arc<SpawnWorker> = Arc::new(move || {
            let (writer, guard) = NonBlockingBuilder::default()
                .lossy(lossy)
                .buffered_lines_limit(queue_size)
                .finish(CountingSink {
                    inner: make_sink(),
                    written: written_for_sink.clone(),
                });
            Generation {
                writer,
                _guard: guard,
            }
        });

        let current = Arc::new(RwLock::new(spawn()));
        Self {
            current,
            spawn,
            submitted: submitted_counter,
            written,
            retired_drops: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Block until every line queued so far has been written to the sink.
    ///
    /// Named `drain` rather than `flush` because [`std::io::Write::flush`] on a
    /// non-blocking writer promises nothing and delivers nothing — this call
    /// really does wait.
    ///
    /// Threads trying to log are blocked for the duration. Returns as soon as
    /// the retired worker has finished; a replacement is already in place.
    pub fn drain(&self) {
        let mut current = self.current.write().unwrap_or_else(PoisonError::into_inner);

        // Install the replacement first so no line can be written to a worker
        // that is already shutting down — the write lock guarantees no writer
        // is in flight while the swap happens.
        let retired = std::mem::replace(&mut *current, (self.spawn)());
        self.retired_drops.fetch_add(
            retired.writer.error_counter().dropped_lines(),
            std::sync::atomic::Ordering::Relaxed,
        );

        // Dropping the retired generation's guard is the drain: it blocks until
        // the worker has written every line queued ahead of the shutdown.
        drop(retired);
    }
}

impl std::fmt::Debug for FlushableWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlushableWriter")
            .field("shared_by", &Arc::strong_count(&self.current))
            .finish()
    }
}

/// Wraps a sink to count the lines that reach it.
struct CountingSink<W> {
    inner: W,
    written: Arc<std::sync::atomic::AtomicUsize>,
}

impl<W: io::Write> io::Write for CountingSink<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buf)?;
        // One buffer is one formatted event, so counting calls counts lines.
        self.written
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<'a> MakeWriter<'a> for FlushableWriter {
    type Writer = QueuedWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let current = self.current.read().unwrap_or_else(PoisonError::into_inner);
        let writer = current.writer.clone();
        self.submitted
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        QueuedWriter {
            _current: current,
            writer,
        }
    }
}

/// A borrowed handle on the current worker.
///
/// It holds the read lock for as long as the write is in flight, so a
/// concurrent [`FlushableWriter::drain`] cannot retire the worker out from
/// under a line that is already on its way.
pub struct QueuedWriter<'a> {
    _current: RwLockReadGuard<'a, Generation>,
    writer: NonBlocking,
}

impl io::Write for QueuedWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.writer.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl FlushableWriter {
    /// Lines this writer discarded because its queue was full.
    ///
    /// Only ever non-zero when the sink was configured `lossy`. A silent zero
    /// and a silent thousand look identical from the outside, which is the
    /// problem this exists to solve.
    ///
    /// Cumulative across drains: a flush retires the worker, and the count
    /// carries forward rather than restarting.
    /// Lines handed to the queue but not yet written out.
    ///
    /// Derived from what went in against what came out, because the underlying
    /// non-blocking writer exposes no depth of its own. Approximate by nature:
    /// it is sampled without a lock, so a value read while the worker is
    /// draining may be slightly stale. It is meant for spotting saturation, not
    /// for exact accounting.
    pub fn queue_depth(&self) -> usize {
        let submitted = self.submitted.load(std::sync::atomic::Ordering::Relaxed);
        let written = self.written.load(std::sync::atomic::Ordering::Relaxed);
        submitted
            .saturating_sub(written)
            .saturating_sub(self.dropped_lines())
    }

    /// Total events this writer discarded on queue overflow, cumulative
    /// across drains: a flush retires the worker and its replacement starts
    /// at zero, so the count is carried forward rather than silently reset.
    pub fn dropped_lines(&self) -> usize {
        let live = self
            .current
            .read()
            .map(|generation| generation.writer.error_counter().dropped_lines())
            .unwrap_or(0);
        live + self
            .retired_drops
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    /// Lines the sink has actually accepted, shared across every generation.
    type Recorder = Arc<Mutex<Vec<String>>>;

    /// A sink that takes `delay` to accept each line.
    ///
    /// The delay is what makes the drain tests deterministic: a `drain()` that
    /// does not really wait returns long before a slow worker has caught up.
    struct SlowSink {
        delay: Duration,
        recorded: Recorder,
    }

    impl io::Write for SlowSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            thread::sleep(self.delay);
            self.recorded
                .lock()
                .expect("recorder poisoned")
                .push(String::from_utf8_lossy(buf).into_owned());
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn slow_writer(delay_ms: u64) -> (FlushableWriter, Recorder) {
        let recorded: Recorder = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&recorded);
        let writer = FlushableWriter::new(
            move || SlowSink {
                delay: Duration::from_millis(delay_ms),
                recorded: Arc::clone(&sink),
            },
            8_192,
            false,
        );
        (writer, recorded)
    }

    fn emit(writer: &FlushableWriter, count: usize) {
        for i in 0..count {
            writer
                .make_writer()
                .write_all(format!("line {i}\n").as_bytes())
                .expect("enqueue failed");
        }
    }

    fn recorded_len(recorder: &Recorder) -> usize {
        recorder.lock().expect("recorder poisoned").len()
    }

    #[test]
    fn drain_blocks_until_every_queued_line_reaches_the_sink() {
        // Arrange — 64 lines at 1ms each is ~64ms of writer work, far longer
        // than the enqueue loop takes.
        let (writer, recorded) = slow_writer(1);
        emit(&writer, 64);

        // Act
        writer.drain();

        // Assert — no sleep: if drain returned early, lines would be missing.
        assert_eq!(recorded_len(&recorded), 64);
    }

    #[test]
    fn the_writer_still_works_after_a_drain() {
        // Arrange
        let (writer, recorded) = slow_writer(1);

        // Act
        emit(&writer, 16);
        writer.drain();
        let after_first = recorded_len(&recorded);

        emit(&writer, 16);
        writer.drain();

        // Assert — drain is a checkpoint, not a shutdown.
        assert_eq!(after_first, 16);
        assert_eq!(recorded_len(&recorded), 32);
    }

    #[test]
    fn draining_an_empty_queue_is_harmless() {
        // Arrange
        let (writer, recorded) = slow_writer(0);

        // Act
        writer.drain();
        writer.drain();
        emit(&writer, 1);
        writer.drain();

        // Assert
        assert_eq!(recorded_len(&recorded), 1);
    }

    #[test]
    fn a_clone_drains_the_same_queue() {
        // Arrange
        let (writer, recorded) = slow_writer(1);
        let clone = writer.clone();
        emit(&writer, 32);

        // Act
        clone.drain();

        // Assert
        assert_eq!(recorded_len(&recorded), 32);
    }

    #[test]
    fn a_clone_keeps_the_worker_alive_after_the_original_is_dropped() {
        // Arrange
        let (writer, recorded) = slow_writer(1);
        let clone = writer.clone();

        // Act
        drop(writer);
        emit(&clone, 16);
        clone.drain();

        // Assert
        assert_eq!(recorded_len(&recorded), 16);
    }

    #[test]
    fn lines_written_during_a_concurrent_drain_are_not_lost() {
        // Arrange — writers on other threads must either finish before the
        // swap or land on the replacement, never on a retired worker.
        let (writer, recorded) = slow_writer(0);
        let total = 200;

        // Act
        let producer = {
            let writer = writer.clone();
            thread::spawn(move || emit(&writer, total))
        };
        for _ in 0..10 {
            writer.drain();
        }
        producer.join().expect("producer panicked");
        writer.drain();

        // Assert
        assert_eq!(recorded_len(&recorded), total);
    }
}
