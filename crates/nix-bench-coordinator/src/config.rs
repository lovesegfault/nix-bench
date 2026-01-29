//! Configuration types for the coordinator

// Re-export from common for use by orchestration code
pub use nix_bench_common::{AgentConfig, Architecture};

use nix_bench_common::defaults::{DEFAULT_BUILD_TIMEOUT, DEFAULT_FLAKE_REF, DEFAULT_MAX_FAILURES};

/// Benchmark parameters
#[derive(Debug, Clone, clap::Args)]
pub struct BenchmarkConfig {
    /// nix-bench attribute to build (e.g., "large-deep")
    #[arg(short, long, default_value = "large-deep")]
    pub attr: String,
    /// Number of benchmark runs per instance
    #[arg(short, long, default_value = "10")]
    pub runs: u32,
    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    #[arg(long, default_value = DEFAULT_FLAKE_REF)]
    pub flake_ref: String,
    /// Build timeout in seconds per run
    #[arg(long, default_value_t = DEFAULT_BUILD_TIMEOUT)]
    pub build_timeout: u64,
    /// Maximum number of build failures before giving up
    #[arg(long, default_value_t = DEFAULT_MAX_FAILURES)]
    pub max_failures: u32,
    /// Run garbage collection between benchmark runs
    #[arg(long)]
    pub gc_between_runs: bool,
}

/// AWS infrastructure configuration
#[derive(Debug, Clone, clap::Args)]
pub struct AwsConfig {
    /// AWS region
    #[arg(long, default_value = "us-east-2")]
    pub region: String,
    /// AWS profile to use (overrides AWS_PROFILE env var)
    #[arg(long)]
    pub aws_profile: Option<String>,
    /// VPC subnet ID for launching instances (uses default VPC if not specified)
    #[arg(long)]
    pub subnet_id: Option<String>,
    /// Security group ID for instances
    #[arg(long)]
    pub security_group_id: Option<String>,
    /// IAM instance profile name for EC2 instances
    #[arg(long)]
    pub instance_profile: Option<String>,
}

/// Instance and agent binary configuration
#[derive(Debug, Clone, clap::Args)]
pub struct InstanceConfig {
    /// Comma-separated EC2 instance types to benchmark
    #[arg(short, long, value_delimiter = ',')]
    pub instance_types: Vec<String>,
    /// Path to pre-built agent binary for x86_64-linux
    #[arg(long, env = "NIX_BENCH_AGENT_X86_64")]
    pub agent_x86_64: Option<String>,
    /// Path to pre-built agent binary for aarch64-linux
    #[arg(long, env = "NIX_BENCH_AGENT_AARCH64")]
    pub agent_aarch64: Option<String>,
}

/// Runtime behavior flags
#[derive(Debug, Clone, clap::Args)]
pub struct RuntimeFlags {
    /// Keep instances after benchmark
    #[arg(long)]
    pub keep: bool,
    /// Per-run timeout in seconds
    #[arg(long, default_value = "7200")]
    pub timeout: u64,
    /// Disable TUI, print progress to stdout
    #[arg(long)]
    pub no_tui: bool,
    /// Validate configuration without launching instances
    #[arg(long)]
    pub dry_run: bool,
    /// Output JSON file for results
    #[arg(short, long)]
    pub output: Option<String>,
}

/// Configuration for a benchmark run
///
/// Composed of focused sub-configs. Each sub-config derives `clap::Args`
/// and is flattened into the CLI.
#[derive(Debug, Clone, clap::Args)]
pub struct RunConfig {
    #[command(flatten)]
    pub benchmark: BenchmarkConfig,
    #[command(flatten)]
    pub aws: AwsConfig,
    #[command(flatten)]
    pub instances: InstanceConfig,
    #[command(flatten)]
    pub flags: RuntimeFlags,
}
