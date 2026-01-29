//! Configuration types for the coordinator

// Re-export from common for use by orchestration code
pub use nix_bench_common::{AgentConfig, Architecture};

/// Benchmark parameters
#[derive(Debug, Clone)]
pub struct BenchmarkConfig {
    /// nix-bench attribute to build
    pub attr: String,
    /// Number of benchmark runs per instance
    pub runs: u32,
    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    pub flake_ref: String,
    /// Build timeout in seconds per run
    pub build_timeout: u64,
    /// Maximum number of build failures before giving up
    pub max_failures: u32,
    /// Run garbage collection between benchmark runs
    pub gc_between_runs: bool,
}

/// AWS infrastructure configuration
#[derive(Debug, Clone)]
pub struct AwsConfig {
    /// AWS region
    pub region: String,
    /// AWS profile name (overrides default credential resolution)
    pub aws_profile: Option<String>,
    /// VPC subnet ID
    pub subnet_id: Option<String>,
    /// Security group ID
    pub security_group_id: Option<String>,
    /// IAM instance profile name
    pub instance_profile: Option<String>,
}

/// Instance and agent binary configuration
#[derive(Debug, Clone)]
pub struct InstanceConfig {
    /// EC2 instance types to benchmark
    pub instance_types: Vec<String>,
    /// Path to pre-built agent binary for x86_64-linux
    pub agent_x86_64: Option<String>,
    /// Path to pre-built agent binary for aarch64-linux
    pub agent_aarch64: Option<String>,
}

/// Runtime behavior flags
#[derive(Debug, Clone)]
pub struct RuntimeFlags {
    /// Keep instances after benchmark
    pub keep: bool,
    /// Per-run timeout in seconds
    pub timeout: u64,
    /// Disable TUI mode
    pub no_tui: bool,
    /// Dry run mode - validate without launching
    pub dry_run: bool,
    /// Output JSON file path
    pub output: Option<String>,
}

/// Configuration for a benchmark run
///
/// Composed of focused sub-configs for organization. Access fields
/// through the sub-config structs (e.g., `config.benchmark.attr`).
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub benchmark: BenchmarkConfig,
    pub aws: AwsConfig,
    pub instances: InstanceConfig,
    pub flags: RuntimeFlags,
}
