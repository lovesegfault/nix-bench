//! Integration tests for AWS clients
//!
//! These tests require AWS credentials and will create real resources.
//! Run with: AWS_PROFILE=beme_sandbox cargo nextest run --test aws_integration

use anyhow::Result;
use std::time::Duration;
use uuid::Uuid;

// Test configuration
const TEST_REGION: &str = "us-east-2";
const TEST_PREFIX: &str = "nix-bench-test";

/// Generate a unique test ID using UUIDv7 for temporal ordering
fn test_id() -> String {
    let uuid = Uuid::now_v7();
    format!("{}-{}", TEST_PREFIX, &uuid.to_string()[..8])
}

mod s3_tests {
    use super::*;

    /// Test S3 bucket lifecycle: create, upload, download, delete
    #[tokio::test]
    async fn test_s3_bucket_lifecycle() -> Result<()> {
        let bucket_name = test_id();
        println!("Testing S3 with bucket: {}", bucket_name);

        // Create client
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_s3::Client::new(&config);

        // Cleanup function to ensure bucket is deleted even on failure
        let cleanup = |client: aws_sdk_s3::Client, bucket: String| async move {
            // Delete all objects first
            let objects = client
                .list_objects_v2()
                .bucket(&bucket)
                .send()
                .await;

            if let Ok(resp) = objects {
                for obj in resp.contents() {
                    if let Some(key) = obj.key() {
                        let _ = client
                            .delete_object()
                            .bucket(&bucket)
                            .key(key)
                            .send()
                            .await;
                    }
                }
            }

            // Delete bucket
            let _ = client.delete_bucket().bucket(&bucket).send().await;
        };

        // Create bucket
        let location = aws_sdk_s3::types::BucketLocationConstraint::from(TEST_REGION);
        let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
            .location_constraint(location)
            .build();

        let create_result = client
            .create_bucket()
            .bucket(&bucket_name)
            .create_bucket_configuration(create_config)
            .send()
            .await;

        if let Err(e) = create_result {
            println!("Failed to create bucket: {:?}", e);
            return Err(e.into());
        }

        // Test upload
        let test_content = b"Hello, nix-bench integration test!";
        let upload_result = client
            .put_object()
            .bucket(&bucket_name)
            .key("test-object.txt")
            .body(aws_sdk_s3::primitives::ByteStream::from(test_content.to_vec()))
            .send()
            .await;

        if let Err(e) = upload_result {
            cleanup(client.clone(), bucket_name.clone()).await;
            return Err(e.into());
        }

        // Test download
        let download_result = client
            .get_object()
            .bucket(&bucket_name)
            .key("test-object.txt")
            .send()
            .await;

        match download_result {
            Ok(resp) => {
                let body = resp.body.collect().await?.into_bytes();
                assert_eq!(body.as_ref(), test_content);
            }
            Err(e) => {
                cleanup(client.clone(), bucket_name.clone()).await;
                return Err(e.into());
            }
        }

        // Cleanup
        cleanup(client, bucket_name).await;

        Ok(())
    }

    /// Test S3 JSON upload/download (simulating config files)
    #[tokio::test]
    async fn test_s3_json_roundtrip() -> Result<()> {
        let bucket_name = test_id();
        println!("Testing S3 JSON with bucket: {}", bucket_name);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_s3::Client::new(&config);

        // Create bucket
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

        // Upload JSON config
        let test_config = serde_json::json!({
            "run_id": "test-123",
            "bucket": bucket_name,
            "region": TEST_REGION,
            "attr": "small-shallow",
            "runs": 3,
            "instance_type": "t3.micro",
            "system": "x86_64-linux"
        });

        let json_bytes = serde_json::to_vec_pretty(&test_config)?;

        client
            .put_object()
            .bucket(&bucket_name)
            .key("config.json")
            .body(aws_sdk_s3::primitives::ByteStream::from(json_bytes.clone()))
            .content_type("application/json")
            .send()
            .await?;

        // Download and verify
        let resp = client
            .get_object()
            .bucket(&bucket_name)
            .key("config.json")
            .send()
            .await?;

        let body = resp.body.collect().await?.into_bytes();
        let downloaded: serde_json::Value = serde_json::from_slice(&body)?;
        assert_eq!(downloaded, test_config);

        // Cleanup
        client
            .delete_object()
            .bucket(&bucket_name)
            .key("config.json")
            .send()
            .await?;

        client.delete_bucket().bucket(&bucket_name).send().await?;

        Ok(())
    }
}

mod cloudwatch_tests {
    use super::*;
    use aws_sdk_cloudwatch::types::{Dimension, MetricDatum, StandardUnit};

    const NAMESPACE: &str = "NixBenchTest";

    /// Test CloudWatch metrics push and retrieval
    #[tokio::test]
    async fn test_cloudwatch_put_metric() -> Result<()> {
        let test_id = test_id();
        println!("Testing CloudWatch with test_id: {}", test_id);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_cloudwatch::Client::new(&config);

        // Create dimensions for isolation
        let dimensions = vec![
            Dimension::builder()
                .name("TestId")
                .value(&test_id)
                .build(),
            Dimension::builder()
                .name("Environment")
                .value("integration-test")
                .build(),
        ];

        // Put a test metric
        let datum = MetricDatum::builder()
            .metric_name("TestMetric")
            .set_dimensions(Some(dimensions.clone()))
            .value(42.0)
            .unit(StandardUnit::Count)
            .build();

        client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(datum)
            .send()
            .await?;

        println!("Successfully pushed metric to CloudWatch");

        // Note: CloudWatch metrics take time to become queryable,
        // so we just verify the put succeeded without querying back
        // In real usage, the coordinator polls metrics over time

        Ok(())
    }

    /// Test multiple metrics push (simulating benchmark progress)
    #[tokio::test]
    async fn test_cloudwatch_benchmark_metrics() -> Result<()> {
        let test_id = test_id();
        println!("Testing CloudWatch benchmark metrics with test_id: {}", test_id);

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

        // Simulate benchmark progress
        for run in 1..=3 {
            // Status metric
            let status_datum = MetricDatum::builder()
                .metric_name("Status")
                .set_dimensions(Some(dimensions.clone()))
                .value(1.0) // Running
                .unit(StandardUnit::Count)
                .build();

            // Progress metric
            let progress_datum = MetricDatum::builder()
                .metric_name("RunProgress")
                .set_dimensions(Some(dimensions.clone()))
                .value(run as f64)
                .unit(StandardUnit::Count)
                .build();

            // Duration metric (simulated)
            let duration_datum = MetricDatum::builder()
                .metric_name("RunDuration")
                .set_dimensions(Some(dimensions.clone()))
                .value(120.0 + (run as f64 * 10.0))
                .unit(StandardUnit::Seconds)
                .build();

            client
                .put_metric_data()
                .namespace(NAMESPACE)
                .metric_data(status_datum)
                .metric_data(progress_datum)
                .metric_data(duration_datum)
                .send()
                .await?;

            println!("Pushed metrics for run {}", run);
        }

        // Final status: complete
        let complete_datum = MetricDatum::builder()
            .metric_name("Status")
            .set_dimensions(Some(dimensions))
            .value(2.0) // Complete
            .unit(StandardUnit::Count)
            .build();

        client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(complete_datum)
            .send()
            .await?;

        println!("Successfully pushed all benchmark metrics");

        Ok(())
    }
}

mod ec2_tests {
    use super::*;

    /// Test EC2 AMI lookup for AL2023
    #[tokio::test]
    async fn test_ec2_ami_lookup() -> Result<()> {
        println!("Testing EC2 AMI lookup for AL2023");

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_ec2::Client::new(&config);

        // Look up latest AL2023 x86_64 AMI
        let response = client
            .describe_images()
            .owners("amazon")
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("name")
                    .values("al2023-ami-*-x86_64")
                    .build(),
            )
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("state")
                    .values("available")
                    .build(),
            )
            .send()
            .await?;

        let images = response.images();
        assert!(!images.is_empty(), "Should find at least one AL2023 AMI");

        // Sort by creation date and get latest
        let mut images: Vec<_> = images.iter().collect();
        images.sort_by(|a, b| {
            b.creation_date()
                .unwrap_or_default()
                .cmp(a.creation_date().unwrap_or_default())
        });

        let latest = images.first().unwrap();
        println!(
            "Found latest AL2023 AMI: {} ({})",
            latest.image_id().unwrap_or("unknown"),
            latest.name().unwrap_or("unknown")
        );

        Ok(())
    }

    /// Test EC2 AMI lookup for ARM64
    #[tokio::test]
    async fn test_ec2_ami_lookup_arm64() -> Result<()> {
        println!("Testing EC2 AMI lookup for AL2023 ARM64");

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_ec2::Client::new(&config);

        let response = client
            .describe_images()
            .owners("amazon")
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("name")
                    .values("al2023-ami-*-arm64")
                    .build(),
            )
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("state")
                    .values("available")
                    .build(),
            )
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("architecture")
                    .values("arm64")
                    .build(),
            )
            .send()
            .await?;

        let images = response.images();
        assert!(!images.is_empty(), "Should find at least one AL2023 ARM64 AMI");

        let mut images: Vec<_> = images.iter().collect();
        images.sort_by(|a, b| {
            b.creation_date()
                .unwrap_or_default()
                .cmp(a.creation_date().unwrap_or_default())
        });

        let latest = images.first().unwrap();
        println!(
            "Found latest AL2023 ARM64 AMI: {} ({})",
            latest.image_id().unwrap_or("unknown"),
            latest.name().unwrap_or("unknown")
        );

        Ok(())
    }

    /// Test EC2 instance launch and terminate (uses t3.micro for cost efficiency)
    /// This test is marked ignore by default as it takes time and costs money
    #[tokio::test]
    #[ignore = "Launches real EC2 instance - run with --ignored to execute"]
    async fn test_ec2_instance_lifecycle() -> Result<()> {
        let test_id = test_id();
        println!("Testing EC2 instance lifecycle with test_id: {}", test_id);

        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(TEST_REGION))
            .load()
            .await;
        let client = aws_sdk_ec2::Client::new(&config);

        // Get latest AL2023 AMI
        let ami_response = client
            .describe_images()
            .owners("amazon")
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("name")
                    .values("al2023-ami-*-x86_64")
                    .build(),
            )
            .filters(
                aws_sdk_ec2::types::Filter::builder()
                    .name("state")
                    .values("available")
                    .build(),
            )
            .send()
            .await?;

        let mut images: Vec<_> = ami_response.images().iter().collect();
        images.sort_by(|a, b| {
            b.creation_date()
                .unwrap_or_default()
                .cmp(a.creation_date().unwrap_or_default())
        });

        let ami_id = images
            .first()
            .and_then(|i| i.image_id())
            .ok_or_else(|| anyhow::anyhow!("No AMI found"))?;

        println!("Using AMI: {}", ami_id);

        // Launch instance
        let run_response = client
            .run_instances()
            .image_id(ami_id)
            .instance_type(aws_sdk_ec2::types::InstanceType::T3Micro)
            .min_count(1)
            .max_count(1)
            .tag_specifications(
                aws_sdk_ec2::types::TagSpecification::builder()
                    .resource_type(aws_sdk_ec2::types::ResourceType::Instance)
                    .tags(
                        aws_sdk_ec2::types::Tag::builder()
                            .key("Name")
                            .value(format!("{}-instance", test_id))
                            .build(),
                    )
                    .tags(
                        aws_sdk_ec2::types::Tag::builder()
                            .key("nix-bench:test")
                            .value("true")
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await?;

        let instance_id = run_response
            .instances()
            .first()
            .and_then(|i| i.instance_id())
            .ok_or_else(|| anyhow::anyhow!("No instance ID returned"))?
            .to_string();

        println!("Launched instance: {}", instance_id);

        // Wait a moment for the instance to start transitioning
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Check instance state
        let describe_response = client
            .describe_instances()
            .instance_ids(&instance_id)
            .send()
            .await?;

        let state = describe_response
            .reservations()
            .first()
            .and_then(|r| r.instances().first())
            .and_then(|i| i.state())
            .and_then(|s| s.name().cloned());

        println!("Instance state: {:?}", state);

        // Terminate instance (cleanup)
        println!("Terminating instance: {}", instance_id);
        client
            .terminate_instances()
            .instance_ids(&instance_id)
            .send()
            .await?;

        // Wait for termination
        tokio::time::sleep(Duration::from_secs(5)).await;

        let describe_response = client
            .describe_instances()
            .instance_ids(&instance_id)
            .send()
            .await?;

        let final_state = describe_response
            .reservations()
            .first()
            .and_then(|r| r.instances().first())
            .and_then(|i| i.state())
            .and_then(|s| s.name().cloned());

        println!("Final instance state: {:?}", final_state);

        Ok(())
    }
}
