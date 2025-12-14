//! Canonical status codes for agent/coordinator communication
//!
//! Provides a shared `StatusCode` enum used in gRPC messages,
//! replacing magic numbers and string-based status values.

use std::fmt;
use std::str::FromStr;

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
    #[cfg(test)]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Check if the status represents failure
    #[cfg(test)]
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Failed)
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Error type for parsing StatusCode
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStatusCodeError(String);

impl fmt::Display for ParseStatusCodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid status code: '{}'", self.0)
    }
}

impl std::error::Error for ParseStatusCodeError {}

impl FromStr for StatusCode {
    type Err = ParseStatusCodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "complete" | "completed" => Ok(Self::Complete),
            "failed" | "error" => Ok(Self::Failed),
            _ => Err(ParseStatusCodeError(s.to_string())),
        }
    }
}

impl StatusCode {
    /// Parse from string, returning None for unknown values
    ///
    /// This is a convenience wrapper around `FromStr` that returns `Option`.
    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
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
        // Using the FromStr trait
        assert_eq!("pending".parse::<StatusCode>(), Ok(StatusCode::Pending));
        assert_eq!("RUNNING".parse::<StatusCode>(), Ok(StatusCode::Running));
        assert_eq!("complete".parse::<StatusCode>(), Ok(StatusCode::Complete));
        assert_eq!("completed".parse::<StatusCode>(), Ok(StatusCode::Complete));
        assert_eq!("failed".parse::<StatusCode>(), Ok(StatusCode::Failed));
        assert_eq!("error".parse::<StatusCode>(), Ok(StatusCode::Failed));
        assert!("unknown".parse::<StatusCode>().is_err());
    }

    #[test]
    fn test_parse_helper() {
        // Using the parse() convenience method
        assert_eq!(StatusCode::parse("pending"), Some(StatusCode::Pending));
        assert_eq!(StatusCode::parse("RUNNING"), Some(StatusCode::Running));
        assert_eq!(StatusCode::parse("unknown"), None);
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

    // Property-based tests
    use proptest::prelude::*;

    proptest! {
        /// from_i32 followed by as_i32 should be identity for valid codes
        #[test]
        fn i32_roundtrip(code in prop_oneof![Just(0i32), Just(1), Just(2), Just(-1)]) {
            let status = StatusCode::from_i32(code).unwrap();
            prop_assert_eq!(status.as_i32(), code);
        }

        /// as_str followed by parse should be identity
        #[test]
        fn str_roundtrip(code in prop_oneof![
            Just(StatusCode::Pending),
            Just(StatusCode::Running),
            Just(StatusCode::Complete),
            Just(StatusCode::Failed)
        ]) {
            let s = code.as_str();
            let parsed = StatusCode::parse(s);
            prop_assert_eq!(parsed, Some(code));
        }

        /// from_i32 returns None for invalid codes
        #[test]
        fn from_i32_invalid_returns_none(code in -1000i32..1000) {
            if ![-1, 0, 1, 2].contains(&code) {
                prop_assert!(StatusCode::from_i32(code).is_none());
            }
        }

        /// parse never panics on arbitrary input
        #[test]
        fn parse_never_panics(s in "\\PC*") {
            let _ = StatusCode::parse(&s);
        }

        /// is_terminal is consistent with Complete and Failed
        #[test]
        fn terminal_consistency(code in prop_oneof![
            Just(StatusCode::Pending),
            Just(StatusCode::Running),
            Just(StatusCode::Complete),
            Just(StatusCode::Failed)
        ]) {
            let is_terminal = code.is_terminal();
            let expected = matches!(code, StatusCode::Complete | StatusCode::Failed);
            prop_assert_eq!(is_terminal, expected);
        }

        /// Display output matches as_str
        #[test]
        fn display_matches_as_str(code in prop_oneof![
            Just(StatusCode::Pending),
            Just(StatusCode::Running),
            Just(StatusCode::Complete),
            Just(StatusCode::Failed)
        ]) {
            prop_assert_eq!(format!("{}", code), code.as_str());
        }
    }
}
