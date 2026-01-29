//! IAM integration tests - actually call AWS APIs
//!
//! These tests are marked `#[ignore]` and only run with:
//! ```
//! AWS_PROFILE=your_profile cargo test --test aws_iam_integration -- --ignored
//! ```

mod aws_test_helpers;

use aws_test_helpers::*;
use nix_bench_coordinator::aws::{FromAwsContext, IamClient};

/// Test IAM role and instance profile create/delete lifecycle
///
/// This test verifies:
/// 1. Role creation with trust policy
/// 2. Inline policy attachment
/// 3. Instance profile creation
/// 4. Role added to instance profile
/// 5. IAM propagation wait
/// 6. Clean deletion of all resources
#[tokio::test]
#[ignore]
async fn test_create_and_delete_role() {
    let region = get_test_region();
    let client = IamClient::new(&region)
        .await
        .expect("AWS credentials required - set AWS_PROFILE or AWS_ACCESS_KEY_ID");

    let run_id = test_run_id();
    let bucket_name = format!("nix-bench-{}", run_id);

    // Create role and instance profile
    // The create_benchmark_role function handles:
    // - Creating the IAM role with EC2 trust policy
    // - Attaching inline policy for S3 access
    // - Attaching SSM managed policy
    // - Creating instance profile
    // - Adding role to instance profile
    // - Waiting for IAM propagation
    let (role_name, profile_name) = client
        .create_benchmark_role(&run_id, &bucket_name, None)
        .await
        .expect("Should create role and instance profile");

    // Verify the names match expected format
    assert!(
        role_name.starts_with("nix-bench-agent-"),
        "Role name should start with 'nix-bench-agent-', got: {}",
        role_name
    );
    assert_eq!(
        role_name, profile_name,
        "Role and profile names should match"
    );

    // Verify instance profile exists
    let exists = client.instance_profile_exists(&profile_name).await;
    assert!(exists, "Instance profile should exist after creation");

    // Delete role and instance profile
    client
        .delete_benchmark_role(&role_name)
        .await
        .expect("Should delete role and instance profile");

    // Verify instance profile no longer exists
    // (may take a moment for IAM to propagate)
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    let exists_after = client.instance_profile_exists(&profile_name).await;
    assert!(
        !exists_after,
        "Instance profile should not exist after deletion"
    );
}
