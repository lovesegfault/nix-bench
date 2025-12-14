//! Canonical status codes for agent/coordinator communication
//!
//! Provides a shared `StatusCode` enum used in gRPC messages,
//! replacing magic numbers and string-based status values.
//!
//! These values match the protobuf `StatusCode` enum in `proto/nix_bench.proto`.

use std::fmt;
use std::str::FromStr;

/// Canonical status codes matching protobuf enum values
///
/// These codes are transmitted via gRPC and must remain stable:
/// - `Pending = 0`: Not yet started
/// - `Running = 1`: In progress
/// - `Complete = 2`: Successfully finished
/// - `Failed = 3`: Failed with error
/// - `Bootstrap = 4`: Bootstrap phase (setting up environment)
/// - `Warmup = 5`: Warmup phase (cache warming build)
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
    Failed = 3,
    /// Bootstrap phase (setting up environment)
    Bootstrap = 4,
    /// Warmup phase (cache warming build)
    Warmup = 5,
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
            3 => Some(Self::Failed),
            4 => Some(Self::Bootstrap),
            5 => Some(Self::Warmup),
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
            Self::Bootstrap => "bootstrap",
            Self::Warmup => "warmup",
        }
    }

    /// Check if the status represents a terminal state
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
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
            "bootstrap" => Ok(Self::Bootstrap),
            "warmup" => Ok(Self::Warmup),
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
