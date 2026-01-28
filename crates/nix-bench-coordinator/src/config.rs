//! Configuration types for the coordinator

// Re-export from common for use by orchestration code
pub use nix_bench_common::{AgentConfig, Architecture, detect_system};

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
/// Composed of focused sub-configs for organization. Fields are accessible
/// both through the sub-configs and through flat accessor methods for convenience.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub benchmark: BenchmarkConfig,
    pub aws: AwsConfig,
    pub instances: InstanceConfig,
    pub flags: RuntimeFlags,
}

// Flat accessor methods for backwards compatibility.
// These delegate to the sub-config fields, allowing gradual migration
// of call sites from `config.field` to `config.sub.field`.
impl RunConfig {
    pub fn attr(&self) -> &str {
        &self.benchmark.attr
    }
    pub fn runs(&self) -> u32 {
        self.benchmark.runs
    }
    pub fn flake_ref(&self) -> &str {
        &self.benchmark.flake_ref
    }
    pub fn build_timeout(&self) -> u64 {
        self.benchmark.build_timeout
    }
    pub fn max_failures(&self) -> u32 {
        self.benchmark.max_failures
    }
    pub fn gc_between_runs(&self) -> bool {
        self.benchmark.gc_between_runs
    }

    pub fn region(&self) -> &str {
        &self.aws.region
    }
    pub fn aws_profile(&self) -> Option<&str> {
        self.aws.aws_profile.as_deref()
    }
    pub fn subnet_id(&self) -> Option<&str> {
        self.aws.subnet_id.as_deref()
    }
    pub fn security_group_id(&self) -> Option<&str> {
        self.aws.security_group_id.as_deref()
    }
    pub fn instance_profile(&self) -> Option<&str> {
        self.aws.instance_profile.as_deref()
    }

    pub fn instance_types(&self) -> &[String] {
        &self.instances.instance_types
    }
    pub fn agent_x86_64(&self) -> Option<&str> {
        self.instances.agent_x86_64.as_deref()
    }
    pub fn agent_aarch64(&self) -> Option<&str> {
        self.instances.agent_aarch64.as_deref()
    }

    pub fn keep(&self) -> bool {
        self.flags.keep
    }
    pub fn timeout(&self) -> u64 {
        self.flags.timeout
    }
    pub fn no_tui(&self) -> bool {
        self.flags.no_tui
    }
    pub fn dry_run(&self) -> bool {
        self.flags.dry_run
    }
    pub fn output(&self) -> Option<&str> {
        self.flags.output.as_deref()
    }
}
