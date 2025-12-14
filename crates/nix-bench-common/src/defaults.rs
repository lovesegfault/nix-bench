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

/// Returns the default gRPC port
pub fn default_grpc_port() -> u16 {
    DEFAULT_GRPC_PORT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_values() {
        assert_eq!(DEFAULT_FLAKE_REF, "github:lovesegfault/nix-bench");
        assert_eq!(DEFAULT_BUILD_TIMEOUT, 7200);
        assert_eq!(DEFAULT_MAX_FAILURES, 3);
        assert_eq!(DEFAULT_GRPC_PORT, 50051);
    }

    #[test]
    fn test_default_functions() {
        assert_eq!(default_flake_ref(), "github:lovesegfault/nix-bench");
        assert_eq!(default_build_timeout(), 7200);
        assert_eq!(default_max_failures(), 3);
        assert_eq!(default_grpc_port(), 50051);
    }
}
