//! Capture ERROR and WARN log entries for display after TUI exits.
//!
//! Since tui_logger stores logs only in memory and they disappear when the
//! alternate screen is exited, this module provides a parallel capture mechanism
//! to print important log entries to stderr after the TUI closes.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// A captured log entry.
#[derive(Debug, Clone)]
struct CapturedLog {
    level: Level,
    target: String,
    message: String,
}

/// Captures ERROR and WARN level log entries in a ring buffer.
///
/// Clone this to get a handle that can be used to print captured logs
/// after the TUI exits.
#[derive(Debug, Clone)]
pub struct LogCapture {
    buffer: Arc<Mutex<VecDeque<CapturedLog>>>,
    max_entries: usize,
}

impl LogCapture {
    /// Create a new log capture with the specified maximum number of entries.
    pub fn new(max_entries: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(VecDeque::with_capacity(max_entries))),
            max_entries,
        }
    }

    /// Print all captured ERROR and WARN entries to stderr.
    pub fn print_to_stderr(&self) {
        let buffer = self.buffer.lock().unwrap();
        if buffer.is_empty() {
            return;
        }

        let errors: Vec<_> = buffer
            .iter()
            .filter(|log| log.level == Level::ERROR)
            .collect();
        let warnings: Vec<_> = buffer
            .iter()
            .filter(|log| log.level == Level::WARN)
            .collect();

        if !errors.is_empty() {
            eprintln!("\n=== Errors ({}) ===", errors.len());
            for log in errors {
                eprintln!("[{}] {}", log.target, log.message);
            }
        }

        if !warnings.is_empty() {
            eprintln!("\n=== Warnings ({}) ===", warnings.len());
            for log in warnings {
                eprintln!("[{}] {}", log.target, log.message);
            }
        }

        if !buffer.is_empty() {
            eprintln!();
        }
    }

    /// Returns true if there are any captured entries.
    pub fn has_entries(&self) -> bool {
        !self.buffer.lock().unwrap().is_empty()
    }

    fn push(&self, log: CapturedLog) {
        let mut buffer = self.buffer.lock().unwrap();
        if buffer.len() >= self.max_entries {
            buffer.pop_front();
        }
        buffer.push_back(log);
    }
}

/// Visitor that extracts the message field from a tracing event.
struct MessageVisitor {
    message: String,
}

impl MessageVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            write!(&mut self.message, "{:?}", value).ok();
        } else if self.message.is_empty() {
            // Fallback: use the first field as message
            write!(&mut self.message, "{:?}", value).ok();
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else if self.message.is_empty() {
            self.message = value.to_string();
        }
    }
}

/// Tracing layer that captures ERROR and WARN events.
#[derive(Debug, Clone)]
pub struct LogCaptureLayer {
    capture: LogCapture,
}

impl LogCaptureLayer {
    /// Create a new log capture layer.
    pub fn new(capture: LogCapture) -> Self {
        Self { capture }
    }
}

impl<S> Layer<S> for LogCaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();

        // Only capture ERROR and WARN
        if level != Level::ERROR && level != Level::WARN {
            return;
        }

        let target = event.metadata().target().to_string();

        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);

        self.capture.push(CapturedLog {
            level,
            target,
            message: visitor.message,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_capture_ring_buffer() {
        let capture = LogCapture::new(3);

        // Add 5 entries, should only keep last 3
        for i in 0..5 {
            capture.push(CapturedLog {
                level: Level::ERROR,
                target: "test".to_string(),
                message: format!("message {}", i),
            });
        }

        let buffer = capture.buffer.lock().unwrap();
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer[0].message, "message 2");
        assert_eq!(buffer[1].message, "message 3");
        assert_eq!(buffer[2].message, "message 4");
    }

    #[test]
    fn test_log_capture_empty() {
        let capture = LogCapture::new(10);
        assert!(!capture.has_entries());
    }

    #[test]
    fn test_log_capture_has_entries() {
        let capture = LogCapture::new(10);
        capture.push(CapturedLog {
            level: Level::WARN,
            target: "test".to_string(),
            message: "warning".to_string(),
        });
        assert!(capture.has_entries());
    }
}
