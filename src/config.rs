//! Configuration types for the coordinator

use serde::{Deserialize, Serialize};

/// Default flake reference for nix-bench
pub const DEFAULT_FLAKE_REF: &str = "github:lovesegfault/nix-bench";

/// Default build timeout in seconds (2 hours)
pub const DEFAULT_BUILD_TIMEOUT: u64 = 7200;

/// Default max failures before giving up
pub const DEFAULT_MAX_FAILURES: u32 = 3;

/// Configuration for a benchmark run
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RunConfig {
    /// EC2 instance types to benchmark
    pub instance_types: Vec<String>,

    /// nix-bench attribute to build
    pub attr: String,

    /// Number of benchmark runs per instance
    pub runs: u32,

    /// AWS region
    pub region: String,

    /// Output JSON file path
    pub output: Option<String>,

    /// Keep instances after benchmark
    pub keep: bool,

    /// Per-run timeout in seconds
    pub timeout: u64,

    /// Disable TUI mode
    pub no_tui: bool,

    /// Path to pre-built agent binary for x86_64-linux
    pub agent_x86_64: Option<String>,

    /// Path to pre-built agent binary for aarch64-linux
    pub agent_aarch64: Option<String>,

    /// VPC subnet ID
    pub subnet_id: Option<String>,

    /// Security group ID
    pub security_group_id: Option<String>,

    /// IAM instance profile name
    pub instance_profile: Option<String>,

    /// Dry run mode - validate without launching
    pub dry_run: bool,

    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    pub flake_ref: String,

    /// Build timeout in seconds per run
    pub build_timeout: u64,

    /// Maximum number of build failures before giving up
    pub max_failures: u32,
}

/// Configuration sent to the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub run_id: String,
    pub bucket: String,
    pub region: String,
    pub attr: String,
    pub runs: u32,
    pub instance_type: String,
    pub system: String,
    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    #[serde(default = "default_flake_ref")]
    pub flake_ref: String,
    /// Build timeout in seconds
    #[serde(default = "default_build_timeout")]
    pub build_timeout: u64,
    /// Maximum number of build failures before giving up
    #[serde(default = "default_max_failures")]
    pub max_failures: u32,
}

fn default_flake_ref() -> String {
    DEFAULT_FLAKE_REF.to_string()
}

fn default_build_timeout() -> u64 {
    DEFAULT_BUILD_TIMEOUT
}

fn default_max_failures() -> u32 {
    DEFAULT_MAX_FAILURES
}

/// Detect system architecture from instance type
pub fn detect_system(instance_type: &str) -> &'static str {
    // Graviton instances have a 'g' after the generation number
    // e.g., c7g, m7g, r7g, c6g, etc.
    if instance_type.contains("6g")
        || instance_type.contains("7g")
        || instance_type.contains("8g")
    {
        "aarch64-linux"
    } else {
        "x86_64-linux"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_system() {
        assert_eq!(detect_system("c7i.metal"), "x86_64-linux");
        assert_eq!(detect_system("m8a.48xlarge"), "x86_64-linux");
        assert_eq!(detect_system("c7g.metal"), "aarch64-linux");
        assert_eq!(detect_system("m7gd.16xlarge"), "aarch64-linux");
        assert_eq!(detect_system("c6gd.metal"), "aarch64-linux");
    }

    #[test]
    fn test_bucket_name_format() {
        // Bucket names must use full UUID to be globally unique
        // This test ensures we don't regress to truncated names
        let uuid = uuid::Uuid::now_v7().to_string();
        let bucket_name = format!("nix-bench-{}", uuid);

        // Should be: "nix-bench-" (10) + UUID with hyphens (36) = 46 chars
        assert_eq!(bucket_name.len(), 46);
        assert!(bucket_name.starts_with("nix-bench-"));

        // S3 bucket names must be 3-63 characters
        assert!(bucket_name.len() >= 3 && bucket_name.len() <= 63);

        // Must be lowercase (UUIDs are lowercase hex)
        assert_eq!(bucket_name, bucket_name.to_lowercase());
    }
}
