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
