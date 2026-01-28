//! Ring buffer for log lines with capped memory usage

use std::collections::VecDeque;

/// Default maximum number of lines to keep in the log buffer
pub const DEFAULT_LOG_BUFFER_MAX_LINES: usize = 10_000;

/// A ring buffer for log lines that caps memory usage by limiting line count.
/// Once the buffer is full, oldest lines are dropped to make room for new ones.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: VecDeque<String>,
    max_lines: usize,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::new(DEFAULT_LOG_BUFFER_MAX_LINES)
    }
}

impl LogBuffer {
    /// Create a new log buffer with the specified maximum line count.
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
        }
    }

    /// Push a single line to the buffer. If at capacity, drops the oldest line.
    pub fn push_line(&mut self, line: String) {
        // Don't add anything if max_lines is 0
        if self.max_lines == 0 {
            return;
        }
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Push multiple lines (from splitting on newlines) to the buffer.
    pub fn push_lines(&mut self, text: &str) {
        for line in text.lines() {
            self.push_line(line.to_string());
        }
    }

    /// Replace all content with new text (splits on newlines).
    pub fn replace(&mut self, text: &str) {
        self.lines.clear();
        self.push_lines(text);
    }

    /// Get the current line count.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// Iterate over lines.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|s| s.as_str())
    }

    /// Join all lines with newlines for rendering.
    pub fn as_string(&self) -> String {
        let total_len: usize = self.lines.iter().map(|s| s.len() + 1).sum();
        let mut result = String::with_capacity(total_len);
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                result.push('\n');
            }
            result.push_str(line);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_buffer_new() {
        let buf = LogBuffer::new(100);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_log_buffer_default() {
        let buf = LogBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.max_lines, DEFAULT_LOG_BUFFER_MAX_LINES);
    }

    #[test]
    fn test_log_buffer_push_line() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());

        assert_eq!(buf.len(), 2);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 1", "line 2"]);
    }

    #[test]
    fn test_log_buffer_overflow() {
        let mut buf = LogBuffer::new(3);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());
        buf.push_line("line 3".to_string());
        buf.push_line("line 4".to_string());

        // Should have dropped line 1
        assert_eq!(buf.len(), 3);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 2", "line 3", "line 4"]);
    }

    #[test]
    fn test_log_buffer_push_lines() {
        let mut buf = LogBuffer::new(100);
        buf.push_lines("line 1\nline 2\nline 3");

        assert_eq!(buf.len(), 3);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["line 1", "line 2", "line 3"]);
    }

    #[test]
    fn test_log_buffer_replace() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("old line".to_string());
        buf.replace("new line 1\nnew line 2");

        assert_eq!(buf.len(), 2);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, vec!["new line 1", "new line 2"]);
    }

    #[test]
    fn test_log_buffer_as_string() {
        let mut buf = LogBuffer::new(100);
        buf.push_line("line 1".to_string());
        buf.push_line("line 2".to_string());

        assert_eq!(buf.as_string(), "line 1\nline 2");
    }

    #[test]
    fn test_log_buffer_as_string_empty() {
        let buf = LogBuffer::new(100);
        assert_eq!(buf.as_string(), "");
    }

    #[test]
    fn test_log_buffer_max_lines_respected() {
        let mut buf = LogBuffer::new(5);
        for i in 0..100 {
            buf.push_line(format!("line {}", i));
        }

        // Should have exactly 5 lines (the last 5)
        assert_eq!(buf.len(), 5);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(
            lines,
            vec!["line 95", "line 96", "line 97", "line 98", "line 99"]
        );
    }

    #[test]
    fn test_log_buffer_zero_max_lines() {
        let mut buf = LogBuffer::new(0);
        buf.push_line("line 1".to_string());

        // With max_lines=0, no lines should be added
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn test_log_buffer_under_capacity() {
        // Test normal operation when well under capacity
        let mut buf = LogBuffer::new(1000);

        // Add fewer lines than capacity
        for i in 0..50 {
            buf.push_line(format!("line {}", i));
        }

        assert_eq!(buf.len(), 50);

        // Verify all lines are present in order
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[49], "line 49");

        // Verify as_string works correctly
        let s = buf.as_string();
        assert!(s.starts_with("line 0\n"));
        assert!(s.ends_with("line 49"));
    }
}
