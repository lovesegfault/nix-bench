//! Agent configuration shared between coordinator and agent
//!
//! The coordinator serializes this configuration to JSON and uploads it to S3.
//! The agent downloads and deserializes it to configure the benchmark run.

use crate::Architecture;
use crate::defaults::{default_build_timeout, default_flake_ref, default_max_failures};
use serde::{Deserialize, Serialize};

/// Default gc_between_runs setting
fn default_gc_between_runs() -> bool {
    false
}

/// Agent configuration for a benchmark run
///
/// This struct is serialized by the coordinator and deserialized by the agent.
/// All fields needed for the agent to execute benchmarks are included here.
/// Validation is done via `garde::Validate`.
#[derive(Debug, Clone, Serialize, Deserialize, garde::Validate)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Unique run identifier (UUIDv7)
    #[garde(length(min = 1))]
    pub run_id: String,

    /// S3 bucket for results
    #[garde(length(min = 1))]
    pub bucket: String,

    /// AWS region
    #[garde(length(min = 1))]
    pub region: String,

    /// nix-bench attribute to build (e.g., "large-deep")
    #[garde(length(min = 1))]
    pub attr: String,

    /// Number of benchmark runs
    #[garde(range(min = 1))]
    pub runs: u32,

    /// EC2 instance type (for metrics dimensions)
    #[garde(length(min = 1))]
    pub instance_type: String,

    /// System architecture
    #[garde(skip)]
    pub system: Architecture,

    /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
    #[serde(default = "default_flake_ref")]
    #[garde(length(min = 1))]
    pub flake_ref: String,

    /// Build timeout in seconds (default: 7200 = 2 hours)
    #[serde(default = "default_build_timeout")]
    #[garde(range(min = 1))]
    pub build_timeout: u64,

    /// Maximum number of build failures before giving up (default: 3)
    #[serde(default = "default_max_failures")]
    #[garde(range(min = 1))]
    pub max_failures: u32,

    /// Run garbage collection between benchmark runs (default: false)
    #[serde(default = "default_gc_between_runs")]
    #[garde(skip)]
    pub gc_between_runs: bool,

    /// CA certificate (PEM) for mTLS
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(required)]
    pub ca_cert_pem: Option<String>,

    /// Agent certificate (PEM) for mTLS
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(required)]
    pub agent_cert_pem: Option<String>,

    /// Agent private key (PEM) for mTLS
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[garde(required)]
    pub agent_key_pem: Option<String>,
}
