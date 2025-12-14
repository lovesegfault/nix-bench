//! nix-bench-common - Shared types and utilities
//!
//! This crate provides shared types used by both the agent and coordinator,
//! without any AWS SDK dependencies to keep it lightweight.
//!
//! ## Modules
//!
//! - [`agent_config`]: Agent configuration for benchmark runs
//! - [`defaults`]: Default configuration values
//! - [`run_result`]: Benchmark run result type
//! - [`stats`]: Duration statistics (min/avg/max)
//! - [`status`]: Canonical status codes for gRPC communication
//! - [`tags`]: AWS resource tag constants for discovery and cleanup
//! - [`tls`]: TLS certificate generation for mTLS

pub mod agent_config;
pub mod defaults;
pub mod resource_kind;
pub mod run_result;
pub mod stats;
pub mod status;
pub mod tags;
pub mod tls;

// Re-export commonly used types
pub use agent_config::AgentConfig;
pub use resource_kind::ResourceKind;
pub use run_result::RunResult;
pub use stats::DurationStats;
pub use status::StatusCode;
pub use tls::{CertKeyPair, TlsConfig};

/// Get the current timestamp in milliseconds since UNIX epoch.
///
/// Returns 0 if system time is before the epoch (should never happen in practice).
#[inline]
pub fn timestamp_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Add jitter to a duration to prevent thundering herd.
///
/// # Arguments
/// * `base` - Base duration
/// * `jitter_factor` - Jitter multiplier (0.0 to 1.0). E.g., 0.25 adds 0-25% to base.
pub fn jittered_delay(base: std::time::Duration, jitter_factor: f64) -> std::time::Duration {
    use rand::Rng;
    if jitter_factor <= 0.0 {
        return base;
    }
    let jitter = rand::thread_rng().gen_range(0.0..jitter_factor);
    std::time::Duration::from_secs_f64(base.as_secs_f64() * (1.0 + jitter))
}

/// Add 0-25% jitter to a duration (standard for retry backoff).
pub fn jittered_delay_25(base: std::time::Duration) -> std::time::Duration {
    jittered_delay(base, 0.25)
}

/// Detect system architecture from EC2 instance type.
///
/// Graviton instances have a 'g' after the generation number (e.g., c7g, m7gd).
/// Returns "aarch64-linux" for Graviton, "x86_64-linux" for all others.
pub fn detect_system(instance_type: &str) -> &'static str {
    let prefix = instance_type.split('.').next().unwrap_or("");
    let chars: Vec<char> = prefix.chars().collect();
    for i in 0..chars.len().saturating_sub(1) {
        if chars[i].is_ascii_digit() && chars[i + 1] == 'g' {
            return "aarch64-linux";
        }
    }
    "x86_64-linux"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_system() {
        // x86_64 instances
        assert_eq!(detect_system("c7i.metal"), "x86_64-linux");
        assert_eq!(detect_system("m8a.48xlarge"), "x86_64-linux");
        assert_eq!(detect_system("c5.xlarge"), "x86_64-linux");

        // Graviton (aarch64) instances
        assert_eq!(detect_system("c7g.metal"), "aarch64-linux");
        assert_eq!(detect_system("m7gd.16xlarge"), "aarch64-linux");
        assert_eq!(detect_system("c6gd.metal"), "aarch64-linux");
    }
}
