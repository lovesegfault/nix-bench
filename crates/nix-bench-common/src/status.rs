//! Canonical status codes for agent/coordinator communication
//!
//! Provides a shared `StatusCode` enum used in gRPC messages,
//! replacing magic numbers and string-based status values.
//!
//! These values match the protobuf `StatusCode` enum in `proto/nix_bench.proto`.

/// Canonical status codes matching protobuf enum values
///
/// These codes are transmitted via gRPC and must remain stable:
/// - `Pending = 0`: Not yet started
/// - `Running = 1`: In progress
/// - `Complete = 2`: Successfully finished
/// - `Failed = 3`: Failed with error
/// - `Bootstrap = 4`: Bootstrap phase (setting up environment)
/// - `Warmup = 5`: Warmup phase (cache warming build)
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    strum::Display,
    strum::EnumString,
    strum::FromRepr,
    strum::AsRefStr,
)]
#[strum(ascii_case_insensitive)]
#[repr(i32)]
pub enum StatusCode {
    /// Not yet started
    #[default]
    #[strum(serialize = "pending")]
    Pending = 0,
    /// Currently running
    #[strum(serialize = "running")]
    Running = 1,
    /// Successfully completed
    #[strum(serialize = "complete", serialize = "completed")]
    Complete = 2,
    /// Failed with error
    #[strum(serialize = "failed", serialize = "error")]
    Failed = 3,
    /// Bootstrap phase (setting up environment)
    #[strum(serialize = "bootstrap")]
    Bootstrap = 4,
    /// Warmup phase (cache warming build)
    #[strum(serialize = "warmup")]
    Warmup = 5,
}

impl StatusCode {
    /// Check if the status represents a terminal state
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
    }

    /// Parse from string, returning None for unknown values
    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }
}
