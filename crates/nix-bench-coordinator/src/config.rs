//! Configuration types for the coordinator

use crate::tui::LogCapture;

// Re-export AgentConfig from common for use by orchestration code
pub use nix_bench_common::AgentConfig;

/// Configuration for a benchmark run
#[derive(Debug, Clone)]
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

    /// Run garbage collection between benchmark runs
    /// Preserves fixed-output derivations (fetched sources) but removes build outputs
    pub gc_between_runs: bool,

    /// Log capture for printing errors/warnings after TUI exit
    pub log_capture: Option<LogCapture>,
}

/// Detect system architecture from instance type
pub fn detect_system(instance_type: &str) -> &'static str {
    // Graviton instances have a 'g' after the generation number
    // e.g., c7g, m7g, r7g, c6g, m7gd, c8g, m9g, etc.
    // Pattern: family + generation + 'g' (optionally followed by 'd' for NVMe)
    // We look for a digit followed by 'g' (and optionally 'd') before the dot
    let prefix = instance_type.split('.').next().unwrap_or("");

    // Check if there's a digit followed by 'g' (with optional 'd' suffix)
    // This matches: c7g, m7gd, r8g, c9g, etc.
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
        assert_eq!(detect_system("r6i.large"), "x86_64-linux");

        // Graviton (aarch64) instances
        assert_eq!(detect_system("c7g.metal"), "aarch64-linux");
        assert_eq!(detect_system("m7gd.16xlarge"), "aarch64-linux");
        assert_eq!(detect_system("c6gd.metal"), "aarch64-linux");
        assert_eq!(detect_system("m9g.xlarge"), "aarch64-linux");
        assert_eq!(detect_system("r8g.large"), "aarch64-linux");
        assert_eq!(detect_system("c9gd.metal"), "aarch64-linux");
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
