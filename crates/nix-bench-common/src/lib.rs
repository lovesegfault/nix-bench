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
    let jitter = rand::rng().random_range(0.0..jitter_factor);
    std::time::Duration::from_secs_f64(base.as_secs_f64() * (1.0 + jitter))
}

/// Add 0-25% jitter to a duration (standard for retry backoff).
pub fn jittered_delay_25(base: std::time::Duration) -> std::time::Duration {
    jittered_delay(base, 0.25)
}

/// Known EC2 instance family prefixes (for validation).
/// Not exhaustive, but covers all common types.
const KNOWN_PREFIXES: &[&str] = &[
    // General purpose
    "a1", "m4", "m5", "m5a", "m5ad", "m5d", "m5dn", "m5n", "m5zn", "m6a", "m6i", "m6id", "m6idn",
    "m6in", "m7a", "m7i", "m7i-flex", "m8a", "mac1", "mac2",
    // Graviton general purpose
    "m6g", "m6gd", "m7g", "m7gd", "m8g", // Compute optimized
    "c4", "c5", "c5a", "c5ad", "c5d", "c5n", "c6a", "c6i", "c6id", "c6in", "c7a", "c7i",
    "c7i-flex", "c8a", // Graviton compute
    "c6g", "c6gd", "c6gn", "c7g", "c7gd", "c7gn", "c8g", // Memory optimized
    "r4", "r5", "r5a", "r5ad", "r5b", "r5d", "r5dn", "r5n", "r6a", "r6i", "r6id", "r6idn", "r6in",
    "r7a", "r7i", "r7iz", "r8a", "x1", "x1e", "x2idn", "x2iedn", "x2iezn",
    // Graviton memory
    "r6g", "r6gd", "r7g", "r7gd", "r8g", "x2gd", // Storage optimized
    "d2", "d3", "d3en", "h1", "i3", "i3en", "i4i", "im4gn", "is4gen",
    // Accelerated compute
    "p3", "p3dn", "p4d", "p4de", "p5", "g4ad", "g4dn", "g5", "g6", "inf1", "inf2", "dl1", "trn1",
    "trn1n", // Graviton accelerated
    "g5g",   // HPC
    "hpc6a", "hpc6id", "hpc7a", // Graviton HPC
    "hpc7g", // Burstable
    "t2", "t3", "t3a", // Graviton burstable
    "t4g",
];

/// Detect system architecture from EC2 instance type.
///
/// Graviton instances have a 'g' after the generation number (e.g., c7g, m7gd).
/// Returns "aarch64-linux" for Graviton, "x86_64-linux" for all others.
///
/// Logs a warning for unrecognized instance type prefixes.
pub fn detect_system(instance_type: &str) -> &'static str {
    let prefix = instance_type.split('.').next().unwrap_or("");

    // Check if the prefix is known
    if !KNOWN_PREFIXES.contains(&prefix) {
        eprintln!(
            "Warning: unrecognized instance type prefix '{}' (from '{}'), defaulting to x86_64-linux",
            prefix, instance_type
        );
    }

    // Detect Graviton: digit followed by 'g' in the prefix
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
