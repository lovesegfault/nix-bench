//! Cleanup operations for orphaned AWS resources

use super::db::open_db;
use super::queries::get_undeleted_resources;
use super::types::Resource;
use crate::aws::CleanupResult;
use anyhow::Result;
use std::collections::HashMap;

/// Handle the result of a cleanup operation for CLI output
fn handle_cli_cleanup_result(
    result: CleanupResult,
    resource: &Resource,
    cleanup_errors: &mut Vec<String>,
) {
    match result {
        CleanupResult::Deleted => println!("    Deleted successfully"),
        CleanupResult::AlreadyDeleted => println!("    Already deleted (marking in DB)"),
        CleanupResult::Failed => {
            let error_msg = format!(
                "{} {}: cleanup failed",
                resource.resource_type.as_str(),
                resource.resource_id
            );
            println!("    Failed to delete");
            cleanup_errors.push(error_msg);
        }
        CleanupResult::Skipped => println!("    Skipped (handled by parent resource)"),
    }
}

/// Cleanup orphaned resources by actually terminating/deleting them in AWS
pub async fn cleanup_resources() -> Result<()> {
    use crate::aws::{
        delete_resource, get_current_account_id, partition_resources_for_cleanup, Ec2Client,
        IamClient, S3Client,
    };

    let pool = open_db().await?;
    let resources = get_undeleted_resources(&pool).await?;

    if resources.is_empty() {
        println!("No orphaned resources to clean up");
        return Ok(());
    }

    println!("Found {} undeleted resources", resources.len());

    let mut cleanup_errors: Vec<String> = Vec::new();

    // Group resources by (account_id, region)
    let mut by_account_region: HashMap<(String, String), Vec<&Resource>> = HashMap::new();
    for resource in &resources {
        by_account_region
            .entry((resource.account_id.clone(), resource.region.clone()))
            .or_default()
            .push(resource);
    }

    for ((stored_account_id, region), region_resources) in by_account_region {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.clone()))
            .load()
            .await;

        let current_account = get_current_account_id(&config).await?;

        if current_account.as_str() != stored_account_id {
            anyhow::bail!(
                "Account mismatch! Resources belong to account {} but current credentials are for account {}.",
                stored_account_id,
                current_account
            );
        }

        println!(
            "Processing account {} region {}...",
            stored_account_id, region
        );

        let ec2 = Ec2Client::new(&region).await?;
        let s3 = S3Client::new(&region).await?;
        let iam = IamClient::new(&region).await?;

        // Partition resources into cleanup order
        let (instances, other, security_groups) =
            partition_resources_for_cleanup(region_resources.into_iter().cloned().collect(), |r| {
                r.resource_type
            });

        // Terminate EC2 instances
        for resource in &instances {
            println!(
                "  Cleaning up {} {}...",
                resource.resource_type.as_str(),
                resource.resource_id
            );
            let result = delete_resource(
                resource.resource_type,
                &resource.resource_id,
                &ec2,
                &s3,
                &iam,
                Some(&pool),
            )
            .await;
            handle_cli_cleanup_result(result, resource, &mut cleanup_errors);
        }

        // Wait for instances to terminate before deleting security groups
        if !instances.is_empty() && !security_groups.is_empty() {
            println!("  Waiting for instances to terminate...");
            for resource in &instances {
                let _ = ec2.wait_for_terminated(&resource.resource_id).await;
            }
        }

        // Delete non-security-group resources
        for resource in &other {
            println!(
                "  Cleaning up {} {}...",
                resource.resource_type.as_str(),
                resource.resource_id
            );
            let result = delete_resource(
                resource.resource_type,
                &resource.resource_id,
                &ec2,
                &s3,
                &iam,
                Some(&pool),
            )
            .await;
            handle_cli_cleanup_result(result, resource, &mut cleanup_errors);
        }

        // Delete security groups (must be last)
        for resource in &security_groups {
            println!(
                "  Cleaning up {} {}...",
                resource.resource_type.as_str(),
                resource.resource_id
            );
            let result = delete_resource(
                resource.resource_type,
                &resource.resource_id,
                &ec2,
                &s3,
                &iam,
                Some(&pool),
            )
            .await;
            handle_cli_cleanup_result(result, resource, &mut cleanup_errors);
        }
    }

    // Update orphaned runs to completed
    sqlx::query(
        "UPDATE runs SET status = 'completed'
         WHERE status = 'orphaned'
         AND NOT EXISTS (
             SELECT 1 FROM resources
             WHERE resources.run_id = runs.run_id
             AND resources.deleted_at IS NULL
         )",
    )
    .execute(&pool)
    .await?;

    if !cleanup_errors.is_empty() {
        anyhow::bail!(
            "Cleanup completed with {} error(s):\n  - {}",
            cleanup_errors.len(),
            cleanup_errors.join("\n  - ")
        );
    }

    println!("Cleanup complete");
    Ok(())
}
