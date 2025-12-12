//! Integration tests for nix-bench-agent
//!
//! These tests verify the agent's AWS interactions.
//! Run with: AWS_PROFILE=beme_sandbox cargo nextest run --test agent_integration

use anyhow::Result;
use uuid::Uuid;

const TEST_REGION: &str = "us-east-2";
const TEST_PREFIX: &str = "nix-bench-agent-test";

fn test_id() -> String {
    let uuid = Uuid::now_v7();
    format!("{}-{}", TEST_PREFIX, &uuid.to_string()[..8])
}

mod cloudwatch_tests {
    use super::*;
    use aws_sdk_cloudwatch::types::{Dimension, MetricDatum, StandardUnit};

    const NAMESPACE: &str = "NixBenchAgentTest";

    /// Test that agent can push status metrics
    #[tokio::test]
    async fn test_agent_status_push() -> Result<()> {
        let test_id = test_id();
        println!("Testing agent status push with test_id: {}", test_id);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_cloudwatch::Client::new(&config);

        let dimensions = vec![
            Dimension::builder()
                .name("RunId")
                .value(&test_id)
                .build(),
            Dimension::builder()
                .name("InstanceType")
                .value("t3.micro")
                .build(),
            Dimension::builder()
                .name("System")
                .value("x86_64-linux")
                .build(),
        ];

        // Simulate agent status updates
        for (status_value, status_name) in [(0.0, "Pending"), (1.0, "Running"), (2.0, "Complete")] {
            let datum = MetricDatum::builder()
                .metric_name("Status")
                .set_dimensions(Some(dimensions.clone()))
                .value(status_value)
                .unit(StandardUnit::Count)
                .build();

            client
                .put_metric_data()
                .namespace(NAMESPACE)
                .metric_data(datum)
                .send()
                .await?;

            println!("Pushed status: {} ({})", status_name, status_value);
        }

        Ok(())
    }

    /// Test that agent can push run progress and duration
    #[tokio::test]
    async fn test_agent_progress_metrics() -> Result<()> {
        let test_id = test_id();
        println!("Testing agent progress metrics with test_id: {}", test_id);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_cloudwatch::Client::new(&config);

        let dimensions = vec![
            Dimension::builder()
                .name("RunId")
                .value(&test_id)
                .build(),
            Dimension::builder()
                .name("InstanceType")
                .value("c7id.metal")
                .build(),
            Dimension::builder()
                .name("System")
                .value("x86_64-linux")
                .build(),
        ];

        // Simulate 3 runs
        for run in 1..=3 {
            let progress_datum = MetricDatum::builder()
                .metric_name("RunProgress")
                .set_dimensions(Some(dimensions.clone()))
                .value(run as f64)
                .unit(StandardUnit::Count)
                .build();

            let duration_datum = MetricDatum::builder()
                .metric_name("RunDuration")
                .set_dimensions(Some(dimensions.clone()))
                .value(1800.0 + (run as f64 * 60.0)) // ~30-33 minutes
                .unit(StandardUnit::Seconds)
                .build();

            client
                .put_metric_data()
                .namespace(NAMESPACE)
                .metric_data(progress_datum)
                .metric_data(duration_datum)
                .send()
                .await?;

            println!("Pushed progress for run {}", run);
        }

        Ok(())
    }
}

mod s3_tests {
    use super::*;

    /// Test that agent can upload results to S3
    #[tokio::test]
    async fn test_agent_results_upload() -> Result<()> {
        let test_id = test_id();
        let bucket_name = format!("{}-results", test_id);
        println!("Testing agent results upload with bucket: {}", bucket_name);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_s3::Client::new(&config);

        // Create test bucket
        let location = aws_sdk_s3::types::BucketLocationConstraint::from(TEST_REGION);
        let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
            .location_constraint(location)
            .build();

        client
            .create_bucket()
            .bucket(&bucket_name)
            .create_bucket_configuration(create_config)
            .send()
            .await?;

        // Simulate agent results upload
        let results = serde_json::json!({
            "run_id": test_id,
            "instance_type": "c7id.metal",
            "system": "x86_64-linux",
            "attr": "large-deep",
            "runs": [
                {"run": 1, "duration_secs": 1842.3, "success": true},
                {"run": 2, "duration_secs": 1856.1, "success": true},
                {"run": 3, "duration_secs": 1839.7, "success": true}
            ]
        });

        let json_bytes = serde_json::to_vec_pretty(&results)?;
        let key = format!("{}/c7id.metal/results.json", test_id);

        client
            .put_object()
            .bucket(&bucket_name)
            .key(&key)
            .body(aws_sdk_s3::primitives::ByteStream::from(json_bytes))
            .content_type("application/json")
            .send()
            .await?;

        println!("Uploaded results to s3://{}/{}", bucket_name, key);

        // Verify upload
        let resp = client
            .get_object()
            .bucket(&bucket_name)
            .key(&key)
            .send()
            .await?;

        let body = resp.body.collect().await?.into_bytes();
        let downloaded: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(downloaded["instance_type"], "c7id.metal");
        assert_eq!(downloaded["runs"].as_array().unwrap().len(), 3);

        // Cleanup
        client
            .delete_object()
            .bucket(&bucket_name)
            .key(&key)
            .send()
            .await?;

        client.delete_bucket().bucket(&bucket_name).send().await?;

        println!("Cleaned up bucket: {}", bucket_name);

        Ok(())
    }
}

mod config_tests {
    use super::*;
    use std::io::Write;

    /// Test config file parsing
    #[test]
    fn test_config_parsing() -> Result<()> {
        let config_json = serde_json::json!({
            "run_id": "test-123",
            "bucket": "nix-bench-test",
            "region": "us-east-2",
            "attr": "large-deep",
            "runs": 10,
            "instance_type": "c7id.metal",
            "system": "x86_64-linux"
        });

        let mut temp_file = tempfile::NamedTempFile::new()?;
        write!(temp_file, "{}", serde_json::to_string_pretty(&config_json)?)?;

        // Parse it back
        let content = std::fs::read_to_string(temp_file.path())?;
        let parsed: serde_json::Value = serde_json::from_str(&content)?;

        assert_eq!(parsed["run_id"], "test-123");
        assert_eq!(parsed["runs"], 10);
        assert_eq!(parsed["instance_type"], "c7id.metal");

        Ok(())
    }

    /// Test config with all optional fields
    #[test]
    fn test_config_all_fields() -> Result<()> {
        let config_json = serde_json::json!({
            "run_id": Uuid::now_v7().to_string(),
            "bucket": "nix-bench-full-test",
            "region": "us-west-2",
            "attr": "medium-shallow-4x",
            "runs": 5,
            "instance_type": "m8g.24xlarge",
            "system": "aarch64-linux"
        });

        let mut temp_file = tempfile::NamedTempFile::new()?;
        write!(temp_file, "{}", serde_json::to_string_pretty(&config_json)?)?;

        let content = std::fs::read_to_string(temp_file.path())?;
        let parsed: serde_json::Value = serde_json::from_str(&content)?;

        assert_eq!(parsed["system"], "aarch64-linux");
        assert_eq!(parsed["attr"], "medium-shallow-4x");

        Ok(())
    }
}
