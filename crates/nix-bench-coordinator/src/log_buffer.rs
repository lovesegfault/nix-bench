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
    pub fn new(max_lines: usize) -> Self {
        Self {
            lines: VecDeque::new(),
            max_lines,
        }
    }

    pub fn push_line(&mut self, line: String) {
        if self.max_lines == 0 {
            return;
        }
        if self.lines.len() >= self.max_lines {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn push_lines(&mut self, text: &str) {
        for line in text.lines() {
            self.push_line(line.to_string());
        }
    }

    pub fn replace(&mut self, text: &str) {
        self.lines.clear();
        self.push_lines(text);
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|s| s.as_str())
    }

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
    fn push_and_overflow() {
        let mut buf = LogBuffer::new(3);
        buf.push_line("a".into());
        buf.push_line("b".into());
        buf.push_line("c".into());
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.as_string(), "a\nb\nc");

        buf.push_line("d".into());
        assert_eq!(buf.len(), 3);
        let lines: Vec<&str> = buf.lines().collect();
        assert_eq!(lines, ["b", "c", "d"]);
    }

    #[test]
    fn push_lines_and_replace() {
        let mut buf = LogBuffer::new(100);
        buf.push_lines("x\ny\nz");
        assert_eq!(buf.len(), 3);

        buf.replace("new1\nnew2");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.as_string(), "new1\nnew2");
    }

    #[test]
    fn zero_capacity() {
        let mut buf = LogBuffer::new(0);
        buf.push_line("ignored".into());
        assert!(buf.is_empty());
        assert_eq!(buf.as_string(), "");
    }

    #[test]
    fn default_capacity() {
        let buf = LogBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.max_lines, DEFAULT_LOG_BUFFER_MAX_LINES);
    }
}
