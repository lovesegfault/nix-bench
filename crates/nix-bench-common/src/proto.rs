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

// --- Proto <-> Domain type conversions ---

impl From<crate::StatusCode> for StatusCode {
    fn from(s: crate::StatusCode) -> Self {
        match s {
            crate::StatusCode::Pending => StatusCode::Pending,
            crate::StatusCode::Running => StatusCode::Running,
            crate::StatusCode::Complete => StatusCode::Complete,
            crate::StatusCode::Failed => StatusCode::Failed,
            crate::StatusCode::Bootstrap => StatusCode::Bootstrap,
            crate::StatusCode::Warmup => StatusCode::Warmup,
        }
    }
}

impl From<crate::RunResult> for RunResult {
    fn from(r: crate::RunResult) -> Self {
        RunResult {
            run_number: r.run_number,
            duration_secs: r.duration_secs,
            success: r.success,
        }
    }
}

impl From<RunResult> for crate::RunResult {
    fn from(r: RunResult) -> Self {
        crate::RunResult {
            run_number: r.run_number,
            duration_secs: r.duration_secs,
            success: r.success,
        }
    }
}

/// Convert an empty string to None, keeping non-empty strings.
pub fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}
