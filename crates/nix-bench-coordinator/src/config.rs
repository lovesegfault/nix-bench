//! Configuration types for the coordinator

use crate::tui::LogCapture;

// Re-export from common for use by orchestration code
pub use nix_bench_common::{detect_system, AgentConfig};

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

    /// AWS profile name (overrides default credential resolution)
    pub aws_profile: Option<String>,

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
