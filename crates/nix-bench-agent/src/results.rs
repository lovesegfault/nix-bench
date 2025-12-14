//! S3 operations: config download only
//!
//! Results are now sent via gRPC, not S3.

use crate::config::Config;
use anyhow::{Context, Result};
use aws_sdk_s3::Client;
use serde::Serialize;
use tracing::{debug, info};

/// Result of a single benchmark run
#[derive(Debug, Clone, Serialize)]
pub struct RunResult {
    pub run: u32,
    pub duration_secs: f64,
    pub success: bool,
}

/// Download config JSON from S3
///
/// This is called early in agent startup, before we have a full Config.
/// Uses the default AWS region from instance metadata.
pub async fn download_config(
    bucket: &str,
    run_id: &str,
    instance_type: &str,
) -> Result<Config> {
    info!(bucket, run_id, instance_type, "Downloading config from S3");

    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;

    let client = Client::new(&aws_config);

    let key = format!("{}/config-{}.json", run_id, instance_type);
    debug!(bucket, key = %key, "Fetching config object");

    let response = client
        .get_object()
        .bucket(bucket)
        .key(&key)
        .send()
        .await
        .with_context(|| format!("Failed to download config from s3://{}/{}", bucket, key))?;

    let body = response
        .body
        .collect()
        .await
        .context("Failed to read config body from S3")?;

    let json = String::from_utf8(body.into_bytes().to_vec())
        .context("Config file is not valid UTF-8")?;

    let config: Config = serde_json::from_str(&json)
        .context("Failed to parse config JSON")?;

    info!("Config downloaded successfully");
    Ok(config)
}

/// Format S3 config key path
pub fn config_key(run_id: &str, instance_type: &str) -> String {
    format!("{}/config-{}.json", run_id, instance_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_result_serialization() {
        let result = RunResult {
            run: 1,
            duration_secs: 45.678,
            success: true,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"run\":1"));
        assert!(json.contains("\"duration_secs\":45.678"));
        assert!(json.contains("\"success\":true"));

        // Verify roundtrip
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["run"], 1);
        assert_eq!(parsed["duration_secs"], 45.678);
        assert_eq!(parsed["success"], true);
    }

    #[test]
    fn test_run_result_failed_run() {
        let result = RunResult {
            run: 3,
            duration_secs: 0.0,
            success: false,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":false"));
    }

    #[test]
    fn test_config_key_format() {
        assert_eq!(
            config_key("run-123", "c6i.xlarge"),
            "run-123/config-c6i.xlarge.json"
        );
        assert_eq!(
            config_key("abc-def-ghi", "g4dn.xlarge"),
            "abc-def-ghi/config-g4dn.xlarge.json"
        );
    }

    #[test]
    fn test_run_result_duration_precision() {
        // Test that we preserve decimal precision
        let result = RunResult {
            run: 1,
            duration_secs: 123.456789,
            success: true,
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

        // JSON should preserve reasonable precision
        let duration = parsed["duration_secs"].as_f64().unwrap();
        assert!((duration - 123.456789).abs() < 1e-6);
    }
}
