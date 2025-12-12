//! Configuration types for the coordinator

use serde::{Deserialize, Serialize};

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
}
