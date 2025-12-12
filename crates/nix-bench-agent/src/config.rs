//! Configuration loading from JSON

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;
use std::path::Path;

/// Agent configuration loaded from JSON
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Unique run identifier (UUIDv7)
    pub run_id: String,

    /// S3 bucket for results
    pub bucket: String,

    /// AWS region
    pub region: String,

    /// nix-bench attribute to build (e.g., "large-deep")
    pub attr: String,

    /// Number of benchmark runs
    pub runs: u32,

    /// EC2 instance type (for metrics dimensions)
    pub instance_type: String,

    /// System architecture (e.g., "x86_64-linux")
    pub system: String,
}

impl Config {
    /// Load configuration from a JSON file
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_config() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(
            file,
            r#"{{
                "run_id": "abc123",
                "bucket": "nix-bench-abc123",
                "region": "us-east-2",
                "attr": "large-deep",
                "runs": 10,
                "instance_type": "c7id.metal",
                "system": "x86_64-linux"
            }}"#
        )
        .unwrap();

        let config = Config::load(file.path()).unwrap();
        assert_eq!(config.run_id, "abc123");
        assert_eq!(config.runs, 10);
        assert_eq!(config.attr, "large-deep");
    }
}
