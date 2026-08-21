//! A syslog sink, for services running outside a container.
//!
//! Writes RFC 3164 messages to a local datagram socket, trying `/dev/log` then
//! `/var/run/syslog`. Uses `std::os::unix::net::UnixDatagram` rather than a
//! syslog crate: the wire format is a single line and the socket is in the
//! standard library, so a dependency would buy very little.
//!
//! Unix only. On other platforms the sink is accepted and does nothing, so a
//! cross-platform service does not need `cfg` at its call site.

use std::io::{self, Write};

/// Well-known local syslog sockets, in the order they are tried.
#[cfg(unix)]
const SOCKET_PATHS: [&str; 2] = ["/dev/log", "/var/run/syslog"];

/// Syslog facility, as defined by RFC 3164.
///
/// Only the facilities a service is plausibly logging under are offered; the
/// rest belong to the kernel and system daemons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Facility {
    /// `user`, facility 1. The default and the right answer for most services.
    #[default]
    User,
    /// `daemon`, facility 3, for a background service.
    Daemon,
    /// `local0`, the first of the eight per-deployment facilities.
    Local0,
    /// `local1`.
    Local1,
    /// `local2`.
    Local2,
    /// `local3`.
    Local3,
    /// `local4`.
    Local4,
    /// `local5`.
    Local5,
    /// `local6`.
    Local6,
    /// `local7`.
    Local7,
}

impl Facility {
    fn code(self) -> u8 {
        match self {
            Facility::User => 1,
            Facility::Daemon => 3,
            Facility::Local0 => 16,
            Facility::Local1 => 17,
            Facility::Local2 => 18,
            Facility::Local3 => 19,
            Facility::Local4 => 20,
            Facility::Local5 => 21,
            Facility::Local6 => 22,
            Facility::Local7 => 23,
        }
    }
}

/// Maps a `tracing` level onto an RFC 3164 severity.
///
/// TRACE and DEBUG both become `debug`: syslog has no finer level, and
/// inventing one would misrepresent the message to anything reading it.
fn severity(level: &tracing::Level) -> u8 {
    match *level {
        tracing::Level::ERROR => 3,
        tracing::Level::WARN => 4,
        tracing::Level::INFO => 6,
        tracing::Level::DEBUG | tracing::Level::TRACE => 7,
    }
}

/// Configuration for the syslog sink.
#[derive(Debug, Clone)]
pub struct SyslogConfig {
    /// The `tag` identifying this process in each message.
    pub tag: String,
    /// Facility to log under.
    pub facility: Facility,
}

impl SyslogConfig {
    /// Log under `tag`, which appears at the start of every message.
    pub fn new(tag: impl Into<String>) -> Self {
        Self {
            tag: tag.into(),
            facility: Facility::default(),
        }
    }

    /// Log under a specific facility. Defaults to `user`.
    pub fn with_facility(mut self, facility: Facility) -> Self {
        self.facility = facility;
        self
    }
}

/// Writes lines to the local syslog socket.
///
/// The priority prefix is computed per line from the level the formatter
/// rendered, which is why the writer is constructed per event rather than once.
pub struct SyslogWriter {
    #[cfg(unix)]
    socket: Option<std::os::unix::net::UnixDatagram>,
    tag: String,
    facility: Facility,
    level: tracing::Level,
}

impl SyslogWriter {
    /// Connect to the local syslog socket, or fall back to discarding.
    ///
    /// A missing socket is not an error: plenty of hosts have no syslog daemon,
    /// and refusing to start over it would be the wrong trade for a logging
    /// sink. The other sinks keep working either way.
    pub fn connect(config: &SyslogConfig, level: tracing::Level) -> Self {
        #[cfg(unix)]
        {
            let socket = std::os::unix::net::UnixDatagram::unbound()
                .ok()
                .and_then(|s| {
                    SOCKET_PATHS
                        .iter()
                        .find_map(|path| s.connect(path).ok().map(|_| ()))
                        .map(|_| s)
                });
            Self {
                socket,
                tag: config.tag.clone(),
                facility: config.facility,
                level,
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                tag: config.tag.clone(),
                facility: config.facility,
                level,
            }
        }
    }

    /// `<priority>tag[pid]: message`, the RFC 3164 shape.
    fn frame(&self, message: &str) -> String {
        let priority = self.facility.code() * 8 + severity(&self.level);
        format!(
            "<{priority}>{}[{}]: {}",
            self.tag,
            std::process::id(),
            message.trim_end()
        )
    }
}

impl Write for SyslogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let message = String::from_utf8_lossy(buf);
        if message.trim().is_empty() {
            return Ok(buf.len());
        }

        #[cfg(unix)]
        if let Some(socket) = &self.socket {
            // A failed send is swallowed on purpose: the daemon restarting must
            // not propagate an error into the calling thread's logging call.
            let _ = socket.send(self.frame(&message).as_bytes());
        }

        // Always reports the whole buffer consumed. A short count would make
        // the caller retry a line syslog has already been offered.
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Datagrams leave immediately; there is nothing buffered to drain.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_combines_facility_and_severity() {
        // Arrange: RFC 3164 priority is facility * 8 + severity.
        let config = SyslogConfig::new("svc").with_facility(Facility::Local3);
        let writer = SyslogWriter::connect(&config, tracing::Level::ERROR);

        // Act
        let framed = writer.frame("boom");

        // Assert: local3 is 19, error is 3, so 19 * 8 + 3 = 155.
        assert!(framed.starts_with("<155>"), "got: {framed}");
    }

    #[test]
    fn the_frame_carries_the_tag_and_pid() {
        // Arrange
        let config = SyslogConfig::new("nse-api");
        let writer = SyslogWriter::connect(&config, tracing::Level::INFO);

        // Act
        let framed = writer.frame("hello");

        // Assert
        assert!(framed.contains("nse-api["), "got: {framed}");
        assert!(
            framed.contains(&format!("[{}]", std::process::id())),
            "got: {framed}"
        );
        assert!(framed.ends_with(": hello"), "got: {framed}");
    }

    #[test]
    fn every_level_maps_to_a_syslog_severity() {
        // Arrange / Act / Assert
        assert_eq!(severity(&tracing::Level::ERROR), 3);
        assert_eq!(severity(&tracing::Level::WARN), 4);
        assert_eq!(severity(&tracing::Level::INFO), 6);
        // No finer level exists in syslog, so both map to debug rather than
        // inventing a distinction the reader cannot honour.
        assert_eq!(severity(&tracing::Level::DEBUG), 7);
        assert_eq!(severity(&tracing::Level::TRACE), 7);
    }

    #[test]
    fn a_missing_socket_is_not_an_error() {
        // Arrange: hosts without a syslog daemon are common, and a logging sink
        // must not stop a service from starting.
        let config = SyslogConfig::new("svc");

        // Act
        let mut writer = SyslogWriter::connect(&config, tracing::Level::INFO);
        let result = writer.write(b"still fine\n");

        // Assert
        assert!(result.is_ok());
        assert_eq!(result.expect("ok"), 11);
    }

    #[test]
    fn an_empty_line_is_not_sent() {
        // Arrange
        let config = SyslogConfig::new("svc");
        let mut writer = SyslogWriter::connect(&config, tracing::Level::INFO);

        // Act
        let written = writer.write(b"   \n").expect("ok");

        // Assert: reported consumed, but nothing framed.
        assert_eq!(written, 4);
    }

    #[test]
    fn the_default_facility_is_user() {
        // Arrange / Act / Assert
        assert_eq!(Facility::default(), Facility::User);
        assert_eq!(Facility::User.code(), 1);
    }
}
