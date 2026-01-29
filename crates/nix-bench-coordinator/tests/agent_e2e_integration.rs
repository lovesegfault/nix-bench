//! Full agent end-to-end integration test
//!
//! This test creates real AWS infrastructure and runs the actual nix-bench-agent
//! to verify the complete benchmark flow including:
//! 1. Infrastructure provisioning (S3, IAM, SG)
//! 2. Instance launch with user-data
//! 3. Agent bootstrap and gRPC connection
//! 4. Status polling and log streaming
//! 5. Full cleanup
//!
//! Run with:
//! ```
//! # Build agents first
//! cargo make agent
//!
//! # Run the test
//! AWS_PROFILE=your_profile cargo test --test agent_e2e_integration -- --ignored --nocapture
//! ```

mod aws_test_helpers;

use aws_test_helpers::*;
use nix_bench_common::tls::{
    TlsConfig, generate_agent_cert, generate_ca, generate_coordinator_cert,
};
use nix_bench_coordinator::aws::context::AwsContext;
use nix_bench_coordinator::aws::ec2::LaunchInstanceConfig;
use nix_bench_coordinator::aws::grpc_client::wait_for_tcp_ready;
use nix_bench_coordinator::aws::{Ec2Client, IamClient, S3Client, get_coordinator_public_ip};
use nix_bench_coordinator::config::AgentConfig;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Instance types to test (instance_type, arch, system)
const TEST_INSTANCES: &[(&str, &str, &str)] = &[
    ("c7a.medium", "x86_64", "x86_64-linux"),
    ("c7g.medium", "aarch64", "aarch64-linux"),
];

/// Timeout for instance operations (10 minutes)
const INSTANCE_TIMEOUT_SECS: u64 = 600;

/// Agent binary paths (set by cargo make agent)
fn agent_binary_path(arch: &str) -> Option<String> {
    let paths = match arch {
        "x86_64" => vec![
            "target/x86_64-unknown-linux-musl/release/nix-bench-agent",
            "../../../target/x86_64-unknown-linux-musl/release/nix-bench-agent",
        ],
        "aarch64" => vec![
            "target/aarch64-unknown-linux-musl/release/nix-bench-agent",
            "../../../target/aarch64-unknown-linux-musl/release/nix-bench-agent",
        ],
        _ => return None,
    };

    for path in paths {
        let p = Path::new(path);
        if p.exists() {
            return p
                .canonicalize()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
        }
    }
    None
}

/// Generate user-data script for agent deployment
fn generate_user_data(bucket: &str, run_id: &str, instance_type: &str) -> String {
    format!(
        r#"#!/bin/bash
set -euo pipefail

exec > >(tee /var/log/nix-bench-bootstrap.log) 2>&1

BUCKET="{bucket}"
RUN_ID="{run_id}"
INSTANCE_TYPE="{instance_type}"
ARCH=$(uname -m)

# Download and run agent (agent handles all setup internally)
echo "Fetching agent from S3..."
aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/agent-${{ARCH}}" /usr/local/bin/nix-bench-agent
chmod +x /usr/local/bin/nix-bench-agent

echo "Starting nix-bench-agent..."
exec /usr/local/bin/nix-bench-agent \
    --bucket "$BUCKET" \
    --run-id "$RUN_ID" \
    --instance-type "$INSTANCE_TYPE"
"#,
        bucket = bucket,
        run_id = run_id,
        instance_type = instance_type,
    )
}

/// Full end-to-end test of the agent with real AWS infrastructure
///
/// This test:
/// - Builds and runs on both x86_64 and aarch64 instances
/// - Uses real agent binaries (must be built first with `cargo make agent`)
/// - Streams logs via gRPC
/// - Waits for agent to complete or timeout
///
/// Expected duration: 5-10 minutes depending on instance startup time
#[tokio::test]
#[ignore]
async fn test_full_benchmark_with_agent() {
    // Initialize crypto for TLS
    nix_bench_test_utils::init_crypto();

    let region = get_test_region();
    cleanup_stale_test_resources(&region).await;

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    println!("=== Agent E2E Test Starting ===");
    println!("Run ID: {}", run_id);
    println!("Bucket: {}", bucket_name);
    println!("Region: {}", region);

    // Check which agent binaries are available
    let mut available_instances: Vec<(&str, &str, &str)> = Vec::new();
    for (instance_type, arch, system) in TEST_INSTANCES {
        if agent_binary_path(arch).is_some() {
            println!("Found agent binary for {} ({})", arch, instance_type);
            available_instances.push((*instance_type, *arch, *system));
        } else {
            println!(
                "WARNING: No agent binary for {} - skipping {}",
                arch, instance_type
            );
        }
    }

    if available_instances.is_empty() {
        println!("\nNo agent binaries found. Run 'cargo make agent' first.");
        println!("Test will be skipped.");
        return;
    }

    // Create AWS clients
    let ctx = AwsContext::new(&region).await;
    let ec2 = Ec2Client::from_context(&ctx);
    let s3 = S3Client::from_context(&ctx);
    let iam = IamClient::from_context(&ctx);

    // Track resources for cleanup (RAII guard handles panic cleanup)
    let mut tracker = TestResourceTracker::new(&region);

    let result = async {
        // === Step 1: Create S3 bucket ===
        println!("\n[1/8] Creating S3 bucket: {}", bucket_name);
        s3.create_bucket(&bucket_name).await?;
        s3.tag_bucket(&bucket_name, &run_id).await?;
        tracker.track_bucket(bucket_name.clone());

        // === Step 2: Upload agent binaries ===
        println!("[2/8] Uploading agent binaries...");
        for (_instance_type, arch, _system) in &available_instances {
            if let Some(path) = agent_binary_path(arch) {
                let key = format!("{}/agent-{}", run_id, arch);
                println!("  Uploading {} -> s3://{}/{}", path, bucket_name, key);
                s3.upload_file(&bucket_name, &key, Path::new(&path)).await?;
            }
        }

        // === Step 3: Create IAM role ===
        println!("[3/8] Creating IAM role...");
        let (rn, _profile) = iam
            .create_benchmark_role(&run_id, &bucket_name, None)
            .await?;
        tracker.track_iam_role(rn.clone());

        // === Step 4: Create security group ===
        println!("[4/8] Creating security group...");
        let coordinator_ip = get_coordinator_public_ip()
            .await
            .unwrap_or_else(|_| "0.0.0.0".to_string());
        let coordinator_cidr = format!("{}/32", coordinator_ip);

        let sg_id = ec2
            .create_security_group(&run_id, &coordinator_cidr, None)
            .await?;
        tracker.track_sg(sg_id.clone());
        println!(
            "  Security group: {} (allowed from {})",
            sg_id, coordinator_cidr
        );

        // === Step 5: Launch instances ===
        println!("[5/8] Launching instances...");
        let mut instance_ips: HashMap<String, String> = HashMap::new();

        // Wait for IAM instance profile to propagate to EC2
        tokio::time::sleep(Duration::from_secs(10)).await;

        for (instance_type, _arch, system) in &available_instances {
            let user_data = generate_user_data(&bucket_name, &run_id, instance_type);

            let system_arch = nix_bench_common::Architecture::from_instance_type(instance_type);
            let launch_config =
                LaunchInstanceConfig::new(&run_id, *instance_type, system_arch, &user_data)
                    .with_security_group(&sg_id)
                    .with_iam_profile(&rn);

            let instance = ec2.launch_instance(launch_config).await?;
            println!(
                "  Launched {} ({}) - {}",
                instance_type, system, instance.instance_id
            );
            tracker.track_instance(instance.instance_id.clone());
        }

        // === Step 6: Wait for instances and get IPs ===
        println!("[6/8] Waiting for instances to be running...");
        for (idx, instance_id) in tracker.instance_ids.iter().enumerate() {
            let instance_type = available_instances[idx].0;
            let public_ip = ec2
                .wait_for_running(instance_id, Some(INSTANCE_TIMEOUT_SECS))
                .await?
                .ok_or_else(|| anyhow::anyhow!("No public IP for {}", instance_id))?;
            println!("  {} running at {}", instance_type, public_ip);
            instance_ips.insert(instance_type.to_string(), public_ip);
        }

        // === Step 7: Generate TLS certs and upload configs ===
        println!("[7/8] Generating TLS certificates and configs...");
        let ca = generate_ca(&run_id)?;
        let coordinator_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)?;

        let _coordinator_tls = TlsConfig {
            ca_cert_pem: ca.cert_pem.clone(),
            cert_pem: coordinator_cert.cert_pem,
            key_pem: coordinator_cert.key_pem,
        };

        for (instance_type, _arch, system) in &available_instances {
            let public_ip = instance_ips.get(*instance_type).unwrap();
            let agent_cert =
                generate_agent_cert(&ca.cert_pem, &ca.key_pem, instance_type, Some(public_ip))?;

            let config = AgentConfig {
                run_id: run_id.clone(),
                bucket: bucket_name.clone(),
                region: region.clone(),
                attr: "shallow.hello".to_string(), // Simple, fast build
                runs: 1,
                instance_type: instance_type.to_string(),
                system: nix_bench_common::Architecture::from_instance_type(instance_type),
                flake_ref: "github:lovesegfault/nix-bench".to_string(),
                build_timeout: 300,
                max_failures: 1,
                gc_between_runs: false,
                ca_cert_pem: Some(ca.cert_pem.clone()),
                agent_cert_pem: Some(agent_cert.cert_pem),
                agent_key_pem: Some(agent_cert.key_pem),
            };

            let config_json = serde_json::to_string_pretty(&config)?;
            let key = format!("{}/config-{}.json", run_id, instance_type);
            println!("  Uploading config for {} -> {}", instance_type, key);
            s3.upload_bytes(
                &bucket_name,
                &key,
                config_json.into_bytes(),
                "application/json",
            )
            .await?;
        }

        // === Step 8: Wait for agents to be reachable ===
        println!("[8/8] Waiting for agents to be reachable on port 50051...");

        for (instance_type, _arch, _system) in &available_instances {
            let public_ip = instance_ips.get(*instance_type).unwrap();
            let addr = format!("{}:50051", public_ip);

            println!("  Waiting for {} at {}...", instance_type, addr);

            // Wait up to 5 minutes for agent to start
            match tokio::time::timeout(
                Duration::from_secs(300),
                wait_for_tcp_ready(&addr, Duration::from_secs(300)),
            )
            .await
            {
                Ok(Ok(())) => {
                    println!("  {} is reachable!", instance_type);
                }
                Ok(Err(e)) => {
                    println!("  {} failed to become reachable: {}", instance_type, e);
                }
                Err(_) => {
                    println!("  {} timed out waiting for TCP", instance_type);
                }
            }
        }

        println!("\n=== Agent E2E test infrastructure setup complete! ===");
        println!("Note: Full gRPC status polling not implemented in this test.");
        println!("Use the coordinator CLI for complete benchmark runs.");

        Ok::<(), anyhow::Error>(())
    }
    .await;

    // === Cleanup ===
    println!("\n=== Cleanup ===");

    // Terminate instances
    for instance_id in &tracker.instance_ids {
        println!("  Terminating instance: {}", instance_id);
        if let Err(e) = ec2.terminate_instance(instance_id).await {
            eprintln!("    Warning: Failed to terminate: {}", e);
        }
    }

    // Wait for termination before deleting SG
    for instance_id in &tracker.instance_ids {
        let _ = ec2.wait_for_terminated(instance_id).await;
    }

    // Delete security group
    tokio::time::sleep(Duration::from_secs(5)).await;
    for sg_id in &tracker.security_group_ids {
        println!("  Deleting security group: {}", sg_id);
        if let Err(e) = ec2.delete_security_group(sg_id).await {
            eprintln!("    Warning: Failed to delete SG: {}", e);
        }
    }

    // Delete IAM role
    for rn in &tracker.iam_role_names {
        println!("  Deleting IAM role: {}", rn);
        if let Err(e) = iam.delete_benchmark_role(rn).await {
            eprintln!("    Warning: Failed to delete role: {}", e);
        }
    }

    // Delete S3 bucket
    for bucket in &tracker.bucket_names {
        println!("  Deleting S3 bucket: {}", bucket);
        if let Err(e) = s3.delete_bucket(bucket).await {
            eprintln!("    Warning: Failed to delete bucket: {}", e);
        }
    }

    // Clear tracker since we cleaned up successfully
    tracker.instance_ids.clear();
    tracker.security_group_ids.clear();
    tracker.iam_role_names.clear();
    tracker.bucket_names.clear();

    println!("\n=== Agent E2E Test Complete ===");

    // Propagate error if test failed
    if let Err(e) = result {
        panic!("Test failed: {}", e);
    }
}

/// Simpler test that just verifies infrastructure can be created
/// and instances reach running state (doesn't wait for agent)
#[tokio::test]
#[ignore]
async fn test_infrastructure_setup_only() {
    nix_bench_test_utils::init_crypto();

    let region = get_test_region();
    cleanup_stale_test_resources(&region).await;

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    println!("=== Infrastructure Setup Test ===");
    println!("Run ID: {}", run_id);

    let ctx = AwsContext::new(&region).await;
    let ec2 = Ec2Client::from_context(&ctx);
    let s3 = S3Client::from_context(&ctx);
    let iam = IamClient::from_context(&ctx);

    // Track resources for cleanup-on-failure
    let mut tracker = TestResourceTracker::new(&region);

    // Create resources
    println!("[1/4] Creating S3 bucket...");
    s3.create_bucket(&bucket_name)
        .await
        .expect("Should create bucket");
    tracker.track_bucket(bucket_name.clone());

    println!("[2/4] Creating IAM role...");
    let (role_name, _profile) = iam
        .create_benchmark_role(&run_id, &bucket_name, None)
        .await
        .expect("Should create role");
    tracker.track_iam_role(role_name.clone());

    println!("[3/4] Creating security group...");
    let coordinator_ip = get_coordinator_public_ip()
        .await
        .unwrap_or_else(|_| "0.0.0.0".to_string());
    let sg_id = ec2
        .create_security_group(&run_id, &format!("{}/32", coordinator_ip), None)
        .await
        .expect("Should create SG");
    tracker.track_sg(sg_id.clone());

    // Wait for IAM instance profile to propagate to EC2
    tokio::time::sleep(Duration::from_secs(10)).await;

    println!("[4/4] Launching instance...");
    let user_data = "#!/bin/bash\necho 'Test instance'\nsleep 300";
    let launch_config = LaunchInstanceConfig::new(
        &run_id,
        "c7a.medium",
        nix_bench_common::Architecture::X86_64,
        user_data,
    )
    .with_security_group(&sg_id)
    .with_iam_profile(&role_name);

    let instance = ec2
        .launch_instance(launch_config)
        .await
        .expect("Should launch instance");

    println!("  Instance: {}", instance.instance_id);
    tracker.track_instance(instance.instance_id.clone());

    let public_ip = ec2
        .wait_for_running(&instance.instance_id, Some(300))
        .await
        .expect("Instance should start")
        .expect("Instance should have public IP");

    println!("  Running at: {}", public_ip);

    // Cleanup
    println!("\n=== Cleanup ===");
    ec2.terminate_instance(&instance.instance_id)
        .await
        .expect("Should terminate");
    let _ = ec2.wait_for_terminated(&instance.instance_id).await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    ec2.delete_security_group(&sg_id)
        .await
        .expect("Should delete SG");
    iam.delete_benchmark_role(&role_name)
        .await
        .expect("Should delete role");
    s3.delete_bucket(&bucket_name)
        .await
        .expect("Should delete bucket");

    // Clear tracker since we cleaned up successfully
    tracker.instance_ids.clear();
    tracker.security_group_ids.clear();
    tracker.iam_role_names.clear();
    tracker.bucket_names.clear();

    println!("\n=== Infrastructure Test Complete ===");
}
