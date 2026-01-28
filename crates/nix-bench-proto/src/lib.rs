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
    AckCompleteRequest, AckCompleteResponse, LogEntry, RunResult, StatusCode, StatusRequest,
    StatusResponse, StreamLogsRequest,
};
