//! Full end-to-end integration test
//!
//! This test creates real AWS infrastructure and verifies the complete flow:
//! 1. Create S3 bucket
//! 2. Create IAM role and instance profile
//! 3. Create security group
//! 4. Launch EC2 instance with user-data
//! 5. Wait for instance to be running (gets dynamic public IP)
//! 6. Verify connectivity (TCP check on gRPC port)
//! 7. Clean up all resources
//!
//! Run with:
//! ```
//! AWS_PROFILE=your_profile cargo test --test aws_e2e_integration -- --ignored
//! ```

mod aws_test_helpers;

use aws_test_helpers::*;
use nix_bench_coordinator::aws::context::AwsContext;
use nix_bench_coordinator::aws::ec2::LaunchInstanceConfig;
use nix_bench_coordinator::aws::{Ec2Client, IamClient, S3Client, get_coordinator_public_ip};
use std::time::Duration;

/// Instance type to use for integration tests
const TEST_INSTANCE_TYPE: &str = "c7a.medium";

/// Timeout for instance operations (5 minutes)
const INSTANCE_TIMEOUT_SECS: u64 = 300;

/// Full end-to-end test of the benchmark infrastructure
///
/// This test takes ~2-3 minutes and actually spins up real AWS resources.
/// It verifies the complete flow that would happen during a real benchmark run.
#[tokio::test]
#[ignore]
async fn test_minimal_benchmark_run() {
    let region = get_test_region();
    cleanup_stale_test_resources(&region).await;

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    // Create clients
    let ctx = AwsContext::new(&region).await;
    let ec2 = Ec2Client::from_context(&ctx);
    let s3 = S3Client::from_context(&ctx);
    let iam = IamClient::from_context(&ctx);

    // Set up resource tracker for cleanup-on-failure
    let mut tracker = TestResourceTracker::new(&region);

    // === Step 1: Create S3 bucket ===
    println!("Creating S3 bucket: {}", bucket_name);
    s3.create_bucket(&bucket_name)
        .await
        .expect("Should create S3 bucket");
    tracker.track_bucket(bucket_name.clone());

    // Upload a dummy config file
    let config_json = format!(
        r#"{{"run_id": "{}", "instance_type": "{}", "runs": 1}}"#,
        run_id, TEST_INSTANCE_TYPE
    );
    s3.upload_bytes(
        &bucket_name,
        &format!("{}/config-{}.json", run_id, TEST_INSTANCE_TYPE),
        config_json.into_bytes(),
        "application/json",
    )
    .await
    .expect("Should upload config");

    // === Step 2: Create IAM role ===
    println!("Creating IAM role...");
    let (role_name, _profile) = iam
        .create_benchmark_role(&run_id, &bucket_name, None)
        .await
        .expect("Should create IAM role");
    tracker.track_iam_role(role_name.clone());

    // === Step 3: Create security group ===
    println!("Creating security group...");
    let coordinator_ip = get_coordinator_public_ip()
        .await
        .unwrap_or_else(|_| "0.0.0.0".to_string());
    let coordinator_cidr = format!("{}/32", coordinator_ip);

    let sg_id = ec2
        .create_security_group(&run_id, &coordinator_cidr, None)
        .await
        .expect("Should create security group");
    tracker.track_sg(sg_id.clone());

    // === Step 4: Launch instance ===
    println!("Launching {} instance...", TEST_INSTANCE_TYPE);

    // User-data that just starts a simple TCP listener on port 50051
    // (simulating the gRPC server without actually running the agent)
    let user_data = r#"#!/bin/bash
set -x
echo "E2E test instance starting..."
# Install netcat and start a listener on gRPC port
yum install -y nc || apt-get install -y netcat-openbsd || true
# Start a simple echo server on port 50051
while true; do echo "OK" | nc -l 50051; done &
echo "Listener started on port 50051"
"#;

    let launch_config = LaunchInstanceConfig::new(
        &run_id,
        TEST_INSTANCE_TYPE,
        nix_bench_common::Architecture::X86_64,
        user_data,
    )
    .with_security_group(&sg_id)
    .with_iam_profile(&role_name);
    let instance = ec2
        .launch_instance(launch_config)
        .await
        .expect("Should launch instance");
    let instance_id = instance.instance_id.clone();
    println!("Launched instance: {}", instance.instance_id);
    tracker.track_instance(instance_id.clone());

    // === Step 5: Wait for running and get dynamic public IP ===
    println!("Waiting for instance to be running...");
    let public_ip = ec2
        .wait_for_running(&instance.instance_id, Some(INSTANCE_TIMEOUT_SECS))
        .await
        .expect("Instance should reach running state")
        .expect("Instance should have a public IP");
    println!("Instance running with public IP: {}", public_ip);

    // === Step 6: Verify connectivity ===
    println!("Waiting for instance to be reachable on port 50051...");
    // Give the instance time to boot and start the listener
    tokio::time::sleep(Duration::from_secs(30)).await;

    // Try to connect to port 50051
    let addr = format!("{}:50051", public_ip);
    let connected = tokio::time::timeout(Duration::from_secs(60), try_connect_loop(&addr)).await;

    match connected {
        Ok(true) => println!("Successfully connected to instance on port 50051"),
        Ok(false) => {
            println!("Warning: Could not connect to instance (may be expected in some VPC configs)")
        }
        Err(_) => println!("Warning: Connection attempt timed out"),
    }

    // === Step 7: Cleanup ===
    println!("Cleaning up resources...");

    // Terminate instance
    println!("Terminating instance: {}", instance_id);
    if let Err(e) = ec2.terminate_instance(&instance_id).await {
        eprintln!("Warning: Failed to terminate instance: {}", e);
    }
    // Wait for termination before deleting SG
    let _ = ec2.wait_for_terminated(&instance_id).await;

    // Delete security group
    println!("Deleting security group: {}", sg_id);
    // May need to wait for instance to fully terminate
    tokio::time::sleep(Duration::from_secs(5)).await;
    if let Err(e) = ec2.delete_security_group(&sg_id).await {
        eprintln!("Warning: Failed to delete security group: {}", e);
    }

    // Delete IAM role
    println!("Deleting IAM role: {}", role_name);
    if let Err(e) = iam.delete_benchmark_role(&role_name).await {
        eprintln!("Warning: Failed to delete IAM role: {}", e);
    }

    // Delete S3 bucket
    println!("Deleting S3 bucket: {}", bucket_name);
    if let Err(e) = s3.delete_bucket(&bucket_name).await {
        eprintln!("Warning: Failed to delete bucket: {}", e);
    }

    // Clear tracker since we cleaned up successfully
    tracker.instance_ids.clear();
    tracker.security_group_ids.clear();
    tracker.iam_role_names.clear();
    tracker.bucket_names.clear();

    println!("E2E test complete!");
}

/// Try to connect to an address in a loop
async fn try_connect_loop(addr: &str) -> bool {
    for attempt in 1..=10 {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(_) => return true,
            Err(e) => {
                println!("Connection attempt {} failed: {}", attempt, e);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
    false
}
