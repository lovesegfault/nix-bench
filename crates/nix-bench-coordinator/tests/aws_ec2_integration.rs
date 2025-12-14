//! EC2 integration tests - actually call AWS APIs
//!
//! These tests are marked `#[ignore]` and only run with:
//! ```
//! AWS_PROFILE=your_profile cargo test --test aws_ec2_integration -- --ignored
//! ```
//!
//! Or via cargo-make:
//! ```
//! AWS_PROFILE=your_profile cargo make ta
//! ```

mod aws_test_helpers;

use aws_test_helpers::*;
use nix_bench_coordinator::aws::ec2::LaunchInstanceConfig;
use nix_bench_coordinator::aws::Ec2Client;

/// Instance type to use for integration tests
const TEST_INSTANCE_TYPE: &str = "c7a.medium";

/// Timeout for instance operations (5 minutes)
const INSTANCE_TIMEOUT_SECS: u64 = 300;

/// Test that we can find AL2023 AMIs for both architectures
#[tokio::test]
#[ignore]
async fn test_get_al2023_ami() {
    let region = get_test_region();
    let client = Ec2Client::new(&region)
        .await
        .expect("AWS credentials required - set AWS_PROFILE or AWS_ACCESS_KEY_ID");

    // Test x86_64
    let ami_x86 = client
        .get_al2023_ami("x86_64-linux")
        .await
        .expect("Should find x86_64 AMI");
    assert!(
        ami_x86.starts_with("ami-"),
        "AMI ID should start with 'ami-', got: {}",
        ami_x86
    );

    // Test aarch64
    let ami_arm = client
        .get_al2023_ami("aarch64-linux")
        .await
        .expect("Should find aarch64 AMI");
    assert!(
        ami_arm.starts_with("ami-"),
        "AMI ID should start with 'ami-', got: {}",
        ami_arm
    );

    // They should be different AMIs
    assert_ne!(ami_x86, ami_arm, "x86_64 and aarch64 AMIs should differ");
}

/// Test security group create/modify/delete lifecycle
#[tokio::test]
#[ignore]
async fn test_security_group_lifecycle() {
    let region = get_test_region();
    let client = Ec2Client::new(&region)
        .await
        .expect("AWS credentials required");

    let run_id = test_run_id();

    // Create security group
    let sg_id = client
        .create_security_group(&run_id, "10.0.0.1/32", None)
        .await
        .expect("Should create security group");
    assert!(
        sg_id.starts_with("sg-"),
        "SG ID should start with 'sg-', got: {}",
        sg_id
    );

    // Add another ingress rule
    client
        .add_grpc_ingress_rule(&sg_id, "10.0.0.2/32")
        .await
        .expect("Should add ingress rule");

    // Remove the rule we just added
    client
        .remove_grpc_ingress_rule(&sg_id, "10.0.0.2/32")
        .await
        .expect("Should remove ingress rule");

    // Delete security group
    client
        .delete_security_group(&sg_id)
        .await
        .expect("Should delete security group");
}

/// Test full instance launch and terminate cycle
#[tokio::test]
#[ignore]
async fn test_launch_and_terminate_instance() {
    let region = get_test_region();
    let client = Ec2Client::new(&region)
        .await
        .expect("AWS credentials required");

    let run_id = test_run_id();

    // Minimal user-data that just exits
    let user_data = "#!/bin/bash\necho 'Integration test instance'\n";

    // Launch instance
    let launch_config = LaunchInstanceConfig::new(&run_id, TEST_INSTANCE_TYPE, "x86_64-linux", user_data);
    let instance = client
        .launch_instance(launch_config)
        .await
        .expect("Should launch instance");

    assert!(
        instance.instance_id.starts_with("i-"),
        "Instance ID should start with 'i-', got: {}",
        instance.instance_id
    );
    assert_eq!(instance.instance_type, TEST_INSTANCE_TYPE);
    assert_eq!(instance.system, "x86_64-linux");

    // Wait for running (with timeout)
    let public_ip = client
        .wait_for_running(&instance.instance_id, Some(INSTANCE_TIMEOUT_SECS))
        .await
        .expect("Instance should reach running state");

    // Public IP may or may not be assigned depending on VPC config
    if let Some(ip) = &public_ip {
        assert!(ip.contains('.'), "Public IP should contain dots, got: {}", ip);
    }

    // Terminate instance
    client
        .terminate_instance(&instance.instance_id)
        .await
        .expect("Should terminate instance");

    // Wait for terminated
    client
        .wait_for_terminated(&instance.instance_id)
        .await
        .expect("Instance should reach terminated state");
}
