//! CRUD operations for state database

use super::db::DbPool;
use super::types::{ResourceType, RunStatus};
use crate::aws::AccountId;
use anyhow::Result;
use chrono::Utc;

/// Insert a new run
pub async fn insert_run(
    pool: &DbPool,
    run_id: &str,
    account_id: &AccountId,
    region: &str,
    instances: &[String],
    attr: &str,
) -> Result<()> {
    let instances_json = serde_json::to_string(instances)?;
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO runs (run_id, account_id, created_at, status, region, instances, attr)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(account_id.as_str())
    .bind(&now)
    .bind(RunStatus::Running.as_str())
    .bind(region)
    .bind(&instances_json)
    .bind(attr)
    .execute(pool)
    .await?;

    Ok(())
}

/// Insert a resource
pub async fn insert_resource(
    pool: &DbPool,
    run_id: &str,
    account_id: &AccountId,
    resource_type: ResourceType,
    resource_id: &str,
    region: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO resources (run_id, account_id, resource_type, resource_id, region, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(account_id.as_str())
    .bind(resource_type.as_str())
    .bind(resource_id)
    .bind(region)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark a resource as deleted
pub async fn mark_resource_deleted(
    pool: &DbPool,
    resource_type: ResourceType,
    resource_id: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE resources SET deleted_at = ?
         WHERE resource_type = ? AND resource_id = ? AND deleted_at IS NULL",
    )
    .bind(&now)
    .bind(resource_type.as_str())
    .bind(resource_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update run status
pub async fn update_run_status(pool: &DbPool, run_id: &str, status: RunStatus) -> Result<()> {
    sqlx::query("UPDATE runs SET status = ? WHERE run_id = ?")
        .bind(status.as_str())
        .bind(run_id)
        .execute(pool)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::AccountId;
    use crate::state::db::open_test_db;
    use nix_bench_common::ResourceKind;

    fn test_account_id() -> AccountId {
        AccountId::new("123456789012".to_string())
    }

    #[tokio::test]
    async fn test_insert_run() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        let result = insert_run(
            &pool,
            "run-123",
            &account_id,
            "us-east-2",
            &["c7a.medium".to_string()],
            "hello",
        )
        .await;

        assert!(result.is_ok(), "Should insert run successfully");

        // Verify run was inserted
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE run_id = 'run-123'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_insert_run_with_multiple_instances() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        let instances = vec!["c7a.medium".to_string(), "c7g.medium".to_string()];
        insert_run(
            &pool,
            "run-multi",
            &account_id,
            "us-west-2",
            &instances,
            "firefox",
        )
        .await
        .unwrap();

        // Verify instances are stored as JSON
        let instances_json: String =
            sqlx::query_scalar("SELECT instances FROM runs WHERE run_id = 'run-multi'")
                .fetch_one(&pool)
                .await
                .unwrap();

        let parsed: Vec<String> = serde_json::from_str(&instances_json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&"c7a.medium".to_string()));
        assert!(parsed.contains(&"c7g.medium".to_string()));
    }

    #[tokio::test]
    async fn test_insert_resource() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        // First insert a run (required for foreign key)
        insert_run(&pool, "run-res", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();

        let result = insert_resource(
            &pool,
            "run-res",
            &account_id,
            ResourceKind::S3Bucket,
            "my-bucket",
            "us-east-2",
        )
        .await;

        assert!(result.is_ok(), "Should insert resource successfully");

        // Verify resource was inserted
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM resources WHERE resource_id = 'my-bucket'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_insert_multiple_resource_types() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-types", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();

        // Insert different resource types
        insert_resource(
            &pool,
            "run-types",
            &account_id,
            ResourceKind::S3Bucket,
            "bucket-1",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-types",
            &account_id,
            ResourceKind::Ec2Instance,
            "i-123456",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-types",
            &account_id,
            ResourceKind::SecurityGroup,
            "sg-789",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-types",
            &account_id,
            ResourceKind::IamRole,
            "role-xyz",
            "us-east-2",
        )
        .await
        .unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM resources WHERE run_id = 'run-types'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 4);
    }

    #[tokio::test]
    async fn test_mark_resource_deleted() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-del", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();
        insert_resource(
            &pool,
            "run-del",
            &account_id,
            ResourceKind::S3Bucket,
            "del-bucket",
            "us-east-2",
        )
        .await
        .unwrap();

        // Verify not deleted initially
        let deleted_at: Option<String> =
            sqlx::query_scalar("SELECT deleted_at FROM resources WHERE resource_id = 'del-bucket'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(deleted_at.is_none());

        // Mark as deleted
        mark_resource_deleted(&pool, ResourceKind::S3Bucket, "del-bucket")
            .await
            .unwrap();

        // Verify now deleted
        let deleted_at: Option<String> =
            sqlx::query_scalar("SELECT deleted_at FROM resources WHERE resource_id = 'del-bucket'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(deleted_at.is_some());
    }

    #[tokio::test]
    async fn test_mark_resource_deleted_idempotent() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-idem", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();
        insert_resource(
            &pool,
            "run-idem",
            &account_id,
            ResourceKind::Ec2Instance,
            "i-idem",
            "us-east-2",
        )
        .await
        .unwrap();

        // Mark as deleted twice - should not error
        mark_resource_deleted(&pool, ResourceKind::Ec2Instance, "i-idem")
            .await
            .unwrap();
        let first_deleted_at: String =
            sqlx::query_scalar("SELECT deleted_at FROM resources WHERE resource_id = 'i-idem'")
                .fetch_one(&pool)
                .await
                .unwrap();

        // Second deletion should not update the timestamp
        mark_resource_deleted(&pool, ResourceKind::Ec2Instance, "i-idem")
            .await
            .unwrap();
        let second_deleted_at: String =
            sqlx::query_scalar("SELECT deleted_at FROM resources WHERE resource_id = 'i-idem'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(first_deleted_at, second_deleted_at);
    }

    #[tokio::test]
    async fn test_update_run_status() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-status", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();

        // Initial status is running
        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE run_id = 'run-status'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "running");

        // Update to completed
        update_run_status(&pool, "run-status", RunStatus::Completed)
            .await
            .unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE run_id = 'run-status'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "completed");

        // Update to failed
        update_run_status(&pool, "run-status", RunStatus::Failed)
            .await
            .unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE run_id = 'run-status'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "failed");
    }
}
