//! Protocol buffer definitions for nix-bench.
//!
//! This crate contains the gRPC service definitions and message types
//! for communication between the coordinator and agent.

/// Generated protobuf types and gRPC service definitions.
pub mod nix_bench {
    tonic::include_proto!("nix_bench");
}

// Re-export commonly used types at crate root for convenience

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

impl From<nix_bench_common::StatusCode> for StatusCode {
    fn from(s: nix_bench_common::StatusCode) -> Self {
        match s {
            nix_bench_common::StatusCode::Pending => StatusCode::Pending,
            nix_bench_common::StatusCode::Running => StatusCode::Running,
            nix_bench_common::StatusCode::Complete => StatusCode::Complete,
            nix_bench_common::StatusCode::Failed => StatusCode::Failed,
            nix_bench_common::StatusCode::Bootstrap => StatusCode::Bootstrap,
            nix_bench_common::StatusCode::Warmup => StatusCode::Warmup,
        }
    }
}

impl From<nix_bench_common::RunResult> for RunResult {
    fn from(r: nix_bench_common::RunResult) -> Self {
        RunResult {
            run_number: r.run_number,
            duration_secs: r.duration_secs,
            success: r.success,
        }
    }
}

impl From<RunResult> for nix_bench_common::RunResult {
    fn from(r: RunResult) -> Self {
        nix_bench_common::RunResult {
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
