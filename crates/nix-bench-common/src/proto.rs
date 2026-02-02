//! Protocol buffer definitions for nix-bench.
//!
//! Contains gRPC service definitions and message types for communication
//! between the coordinator and agent.

/// Generated protobuf types and gRPC service definitions.
pub mod nix_bench {
    tonic::include_proto!("nix_bench");
}

// Re-export commonly used types at module root for convenience

// gRPC server types (used by agent)
pub use nix_bench::log_stream_server::{LogStream, LogStreamServer};

// gRPC client types (used by coordinator)
pub use nix_bench::log_stream_client::LogStreamClient;

// Message types (used by both)
pub use nix_bench::{
    AckCompleteRequest, AckCompleteResponse, CancelBenchmarkRequest, CancelBenchmarkResponse,
    LogEntry, RunResult, StatusCode, StatusRequest, StatusResponse, StreamLogsRequest,
};

impl StatusCode {
    /// Check if the status represents a terminal state
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
    }
}

impl RunResult {
    /// Create a successful run result
    pub fn success(run_number: u32, duration_secs: f64) -> Self {
        Self {
            run_number,
            duration_secs,
            success: true,
        }
    }

    /// Create a failed run result
    pub fn failure(run_number: u32) -> Self {
        Self {
            run_number,
            duration_secs: 0.0,
            success: false,
        }
    }
}

/// Convert an empty string to None, keeping non-empty strings.
pub fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}
