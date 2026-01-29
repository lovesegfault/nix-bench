//! Default configuration values shared between agent and coordinator
//!
//! These constants ensure consistent defaults across all nix-bench components.

/// Default flake reference for nix-bench builds
pub const DEFAULT_FLAKE_REF: &str = "github:lovesegfault/nix-bench";

/// Default build timeout in seconds (2 hours)
pub const DEFAULT_BUILD_TIMEOUT: u64 = 7200;

/// Default maximum number of build failures before giving up
pub const DEFAULT_MAX_FAILURES: u32 = 3;

/// Default gRPC port for agent server
pub const DEFAULT_GRPC_PORT: u16 = 50051;

/// Default broadcast channel capacity for gRPC log streaming
pub const DEFAULT_BROADCAST_CAPACITY: usize = 1024;

/// Default log buffer size (lines) for agent-side message buffering
pub const DEFAULT_LOG_BUFFER_SIZE: usize = 1000;

/// Default timeout for waiting for coordinator acknowledgment (seconds)
pub const DEFAULT_ACK_TIMEOUT_SECS: u64 = 120;

/// Default timeout for waiting for TLS config from S3 (seconds)
pub const DEFAULT_TLS_CONFIG_TIMEOUT_SECS: u64 = 300;

/// Default timeout for waiting for instance termination (seconds)
pub const DEFAULT_TERMINATION_WAIT_TIMEOUT_SECS: u64 = 300;

/// Default root EBS volume size in GiB for benchmark instances
pub const DEFAULT_ROOT_VOLUME_SIZE_GIB: i32 = 100;

// Serde default functions for struct field defaults

/// Returns the default flake reference
pub fn default_flake_ref() -> String {
    DEFAULT_FLAKE_REF.to_string()
}

/// Returns the default build timeout
pub fn default_build_timeout() -> u64 {
    DEFAULT_BUILD_TIMEOUT
}

/// Returns the default max failures
pub fn default_max_failures() -> u32 {
    DEFAULT_MAX_FAILURES
}
