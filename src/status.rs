//! Canonical status codes for agent/coordinator communication
//!
//! Provides a shared `StatusCode` enum used in gRPC messages,
//! replacing magic numbers and string-based status values.

use std::fmt;

/// Canonical status codes (stable i32 values for gRPC)
///
/// These codes are transmitted via gRPC and must remain stable:
/// - `Pending = 0`: Not yet started
/// - `Running = 1`: In progress
/// - `Complete = 2`: Successfully finished
/// - `Failed = -1`: Failed with error
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(i32)]
pub enum StatusCode {
    /// Not yet started
    #[default]
    Pending = 0,
    /// Currently running
    Running = 1,
    /// Successfully completed
    Complete = 2,
    /// Failed with error
    Failed = -1,
}

impl StatusCode {
    /// Convert from i32 (from protobuf)
    ///
    /// Returns `None` for unknown values.
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Pending),
            1 => Some(Self::Running),
            2 => Some(Self::Complete),
            -1 => Some(Self::Failed),
            _ => None,
        }
    }

    /// Convert from status string (legacy compatibility)
    ///
    /// Returns `None` for unknown values.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "complete" | "completed" => Some(Self::Complete),
            "failed" | "error" => Some(Self::Failed),
            _ => None,
        }
    }

    /// Convert to i32 (for protobuf)
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Convert to status string
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }

    /// Check if the status represents a terminal state
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
    }

    /// Check if the status represents success
    pub fn is_success(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Check if the status represents failure
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_i32() {
        assert_eq!(StatusCode::from_i32(0), Some(StatusCode::Pending));
        assert_eq!(StatusCode::from_i32(1), Some(StatusCode::Running));
        assert_eq!(StatusCode::from_i32(2), Some(StatusCode::Complete));
        assert_eq!(StatusCode::from_i32(-1), Some(StatusCode::Failed));
        assert_eq!(StatusCode::from_i32(99), None);
    }

    #[test]
    fn test_from_str() {
        assert_eq!(StatusCode::from_str("pending"), Some(StatusCode::Pending));
        assert_eq!(StatusCode::from_str("RUNNING"), Some(StatusCode::Running));
        assert_eq!(StatusCode::from_str("complete"), Some(StatusCode::Complete));
        assert_eq!(StatusCode::from_str("completed"), Some(StatusCode::Complete));
        assert_eq!(StatusCode::from_str("failed"), Some(StatusCode::Failed));
        assert_eq!(StatusCode::from_str("error"), Some(StatusCode::Failed));
        assert_eq!(StatusCode::from_str("unknown"), None);
    }

    #[test]
    fn test_as_i32() {
        assert_eq!(StatusCode::Pending.as_i32(), 0);
        assert_eq!(StatusCode::Running.as_i32(), 1);
        assert_eq!(StatusCode::Complete.as_i32(), 2);
        assert_eq!(StatusCode::Failed.as_i32(), -1);
    }

    #[test]
    fn test_as_str() {
        assert_eq!(StatusCode::Pending.as_str(), "pending");
        assert_eq!(StatusCode::Running.as_str(), "running");
        assert_eq!(StatusCode::Complete.as_str(), "complete");
        assert_eq!(StatusCode::Failed.as_str(), "failed");
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", StatusCode::Running), "running");
    }

    #[test]
    fn test_is_terminal() {
        assert!(!StatusCode::Pending.is_terminal());
        assert!(!StatusCode::Running.is_terminal());
        assert!(StatusCode::Complete.is_terminal());
        assert!(StatusCode::Failed.is_terminal());
    }

    #[test]
    fn test_default() {
        assert_eq!(StatusCode::default(), StatusCode::Pending);
    }
}
