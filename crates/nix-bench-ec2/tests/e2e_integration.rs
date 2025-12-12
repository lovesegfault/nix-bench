//! End-to-end integration tests
//!
//! These tests launch real EC2 instances and cost money.
//! Run with: AWS_PROFILE=beme_sandbox cargo nextest run --test e2e_integration --run-ignored all

use anyhow::Result;
use std::time::Duration;
use uuid::Uuid;

const TEST_REGION: &str = "us-east-2";
const TEST_PREFIX: &str = "nix-bench-e2e";

fn test_id() -> String {
    let uuid = Uuid::now_v7();
    format!("{}-{}", TEST_PREFIX, &uuid.to_string()[..8])
}

/// Full orchestration flow test (without actual benchmarking)
/// This test:
/// 1. Creates S3 bucket
/// 2. Uploads a dummy "agent" binary (shell script that just exits)
/// 3. Launches a t3.micro instance
/// 4. Waits for it to be running
/// 5. Terminates it
/// 6. Cleans up S3
#[tokio::test]
#[ignore = "Launches real EC2 instance - costs money - run with --run-ignored all"]
async fn test_orchestration_flow() -> Result<()> {
    let test_id = test_id();
    // Use full UUID for bucket name (S3 names are globally unique)
    let bucket_name = format!("nix-bench-{}", test_id);
    println!("Starting E2E test with ID: {}", test_id);
    println!("Bucket: {}", bucket_name);

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(TEST_REGION))
        .load()
        .await;

    let ec2 = aws_sdk_ec2::Client::new(&config);
    let s3 = aws_sdk_s3::Client::new(&config);

    // === Step 1: Create S3 bucket ===
    println!("Creating S3 bucket...");
    let location = aws_sdk_s3::types::BucketLocationConstraint::from(TEST_REGION);
    let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
        .location_constraint(location)
        .build();

    s3.create_bucket()
        .bucket(&bucket_name)
        .create_bucket_configuration(create_config)
        .send()
        .await?;

    // Cleanup function
    let cleanup = |ec2: aws_sdk_ec2::Client,
                   s3: aws_sdk_s3::Client,
                   bucket: String,
                   instance_id: Option<String>| async move {
        // Terminate instance if it exists
        if let Some(id) = instance_id {
            println!("Terminating instance: {}", id);
            let _ = ec2.terminate_instances().instance_ids(&id).send().await;
        }

        // Delete S3 objects
        println!("Cleaning up S3 bucket: {}", bucket);
        let objects = s3.list_objects_v2().bucket(&bucket).send().await;
        if let Ok(resp) = objects {
            for obj in resp.contents() {
                if let Some(key) = obj.key() {
                    let _ = s3.delete_object().bucket(&bucket).key(key).send().await;
                }
            }
        }

        // Delete bucket
        let _ = s3.delete_bucket().bucket(&bucket).send().await;
    };

    // === Step 2: Upload dummy agent (shell script) ===
    println!("Uploading dummy agent...");
    let dummy_agent = r#"#!/bin/bash
echo "Dummy agent started"
sleep 10
echo "Dummy agent complete"
"#;

    let agent_key = format!("{}/agent-x86_64", test_id);
    s3.put_object()
        .bucket(&bucket_name)
        .key(&agent_key)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            dummy_agent.as_bytes().to_vec(),
        ))
        .send()
        .await?;

    // Upload config
    let config_json = serde_json::json!({
        "run_id": test_id,
        "bucket": bucket_name,
        "region": TEST_REGION,
        "attr": "small-shallow",
        "runs": 1,
        "instance_type": "t3.micro",
        "system": "x86_64-linux"
    });

    let config_key = format!("{}/config-x86_64.json", test_id);
    s3.put_object()
        .bucket(&bucket_name)
        .key(&config_key)
        .body(aws_sdk_s3::primitives::ByteStream::from(
            serde_json::to_vec_pretty(&config_json)?,
        ))
        .content_type("application/json")
        .send()
        .await?;

    // === Step 3: Get AMI ===
    println!("Looking up AL2023 AMI...");
    let ami_response = ec2
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

    // === Step 4: Launch instance ===
    println!("Launching t3.micro instance...");

    // Simple user-data that just logs and exits
    let user_data = format!(
        r#"#!/bin/bash
exec > /var/log/user-data.log 2>&1
echo "Test instance started at $(date)"
echo "Test ID: {}"
echo "Done"
"#,
        test_id
    );

    let user_data_b64 =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, user_data.as_bytes());

    let run_response = ec2
        .run_instances()
        .image_id(ami_id)
        .instance_type(aws_sdk_ec2::types::InstanceType::T3Micro)
        .min_count(1)
        .max_count(1)
        .user_data(&user_data_b64)
        .tag_specifications(
            aws_sdk_ec2::types::TagSpecification::builder()
                .resource_type(aws_sdk_ec2::types::ResourceType::Instance)
                .tags(
                    aws_sdk_ec2::types::Tag::builder()
                        .key("Name")
                        .value(format!("{}-test", test_id))
                        .build(),
                )
                .tags(
                    aws_sdk_ec2::types::Tag::builder()
                        .key("nix-bench:test")
                        .value("true")
                        .build(),
                )
                .tags(
                    aws_sdk_ec2::types::Tag::builder()
                        .key("nix-bench:run-id")
                        .value(&test_id)
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

    println!("Instance launched: {}", instance_id);

    // === Step 5: Wait for running ===
    println!("Waiting for instance to be running...");
    let mut attempts = 0;
    loop {
        if attempts > 30 {
            cleanup(ec2.clone(), s3.clone(), bucket_name.clone(), Some(instance_id.clone())).await;
            anyhow::bail!("Timeout waiting for instance to be running");
        }

        let describe = ec2
            .describe_instances()
            .instance_ids(&instance_id)
            .send()
            .await?;

        let state = describe
            .reservations()
            .first()
            .and_then(|r| r.instances().first())
            .and_then(|i| i.state())
            .and_then(|s| s.name().cloned());

        println!("Instance state: {:?}", state);

        match state {
            Some(aws_sdk_ec2::types::InstanceStateName::Running) => {
                println!("Instance is running!");
                break;
            }
            Some(aws_sdk_ec2::types::InstanceStateName::Pending) => {
                tokio::time::sleep(Duration::from_secs(5)).await;
                attempts += 1;
            }
            other => {
                cleanup(ec2.clone(), s3.clone(), bucket_name.clone(), Some(instance_id.clone())).await;
                anyhow::bail!("Unexpected instance state: {:?}", other);
            }
        }
    }

    // === Step 6: Cleanup ===
    println!("Test successful! Cleaning up...");
    cleanup(ec2, s3, bucket_name, Some(instance_id)).await;

    println!("E2E test complete!");
    Ok(())
}

/// Test S3 bucket creation and cleanup in isolation
#[tokio::test]
async fn test_s3_lifecycle_isolated() -> Result<()> {
    let test_id = test_id();
    // Use full UUID for bucket name (S3 names are globally unique)
    let bucket_name = format!("nix-bench-{}", test_id);
    println!("Testing S3 lifecycle with bucket: {}", bucket_name);

    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(TEST_REGION))
        .load()
        .await;

    let s3 = aws_sdk_s3::Client::new(&config);

    // Create bucket
    let location = aws_sdk_s3::types::BucketLocationConstraint::from(TEST_REGION);
    let create_config = aws_sdk_s3::types::CreateBucketConfiguration::builder()
        .location_constraint(location)
        .build();

    s3.create_bucket()
        .bucket(&bucket_name)
        .create_bucket_configuration(create_config)
        .send()
        .await?;

    // Upload multiple objects
    for i in 1..=3 {
        let key = format!("test-object-{}.txt", i);
        s3.put_object()
            .bucket(&bucket_name)
            .key(&key)
            .body(aws_sdk_s3::primitives::ByteStream::from(
                format!("Content {}", i).into_bytes(),
            ))
            .send()
            .await?;
    }

    // Verify objects exist
    let list = s3.list_objects_v2().bucket(&bucket_name).send().await?;
    assert_eq!(list.contents().len(), 3);

    // Delete objects
    for obj in list.contents() {
        if let Some(key) = obj.key() {
            s3.delete_object().bucket(&bucket_name).key(key).send().await?;
        }
    }

    // Delete bucket
    s3.delete_bucket().bucket(&bucket_name).send().await?;

    println!("S3 lifecycle test complete");
    Ok(())
}
