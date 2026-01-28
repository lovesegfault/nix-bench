//! Query operations for state database

use super::db::DbPool;
use super::types::{parse_resource_type, Resource};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::Row;

/// Get undeleted resources for a specific run
pub async fn get_run_resources(pool: &DbPool, run_id: &str) -> Result<Vec<Resource>> {
    let rows = sqlx::query(
        "SELECT id, run_id, account_id, resource_type, resource_id, region, created_at, deleted_at
         FROM resources WHERE run_id = ? AND deleted_at IS NULL",
    )
    .bind(run_id)
    .fetch_all(pool)
    .await?;

    let mut resources = Vec::new();
    for row in rows {
        let created_at_str: String = row.get("created_at");
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .context("Invalid created_at timestamp")?
            .with_timezone(&Utc);

        resources.push(Resource {
            id: row.get("id"),
            run_id: row.get("run_id"),
            account_id: row.get("account_id"),
            resource_type: parse_resource_type(row.get("resource_type")),
            resource_id: row.get("resource_id"),
            region: row.get("region"),
            created_at,
            deleted_at: None,
        });
    }

    Ok(resources)
}

/// Get all undeleted resources
pub async fn get_undeleted_resources(pool: &DbPool) -> Result<Vec<Resource>> {
    let rows = sqlx::query(
        "SELECT id, run_id, account_id, resource_type, resource_id, region, created_at, deleted_at
         FROM resources WHERE deleted_at IS NULL",
    )
    .fetch_all(pool)
    .await?;

    let mut resources = Vec::new();
    for row in rows {
        let created_at_str: String = row.get("created_at");
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .context("Invalid created_at timestamp")?
            .with_timezone(&Utc);

        resources.push(Resource {
            id: row.get("id"),
            run_id: row.get("run_id"),
            account_id: row.get("account_id"),
            resource_type: parse_resource_type(row.get("resource_type")),
            resource_id: row.get("resource_id"),
            region: row.get("region"),
            created_at,
            deleted_at: None,
        });
    }

    Ok(resources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::AccountId;
    use crate::state::crud::{insert_resource, insert_run, mark_resource_deleted};
    use crate::state::db::open_test_db;
    use nix_bench_common::ResourceKind;

    fn test_account_id() -> AccountId {
        AccountId::new("123456789012".to_string())
    }

    #[tokio::test]
    async fn test_get_run_resources_returns_undeleted_only() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-query", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();

        // Insert two resources
        insert_resource(
            &pool,
            "run-query",
            &account_id,
            ResourceKind::S3Bucket,
            "bucket-a",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-query",
            &account_id,
            ResourceKind::Ec2Instance,
            "i-keep",
            "us-east-2",
        )
        .await
        .unwrap();

        // Delete one
        mark_resource_deleted(&pool, ResourceKind::S3Bucket, "bucket-a")
            .await
            .unwrap();

        // Query should return only the undeleted one
        let resources = get_run_resources(&pool, "run-query").await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].resource_id, "i-keep");
    }

    #[tokio::test]
    async fn test_get_run_resources_returns_empty_for_nonexistent_run() {
        let pool = open_test_db().await.unwrap();

        let resources = get_run_resources(&pool, "nonexistent").await.unwrap();
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_get_run_resources_returns_correct_types() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-types-q", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();

        insert_resource(
            &pool,
            "run-types-q",
            &account_id,
            ResourceKind::S3Bucket,
            "bucket",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-types-q",
            &account_id,
            ResourceKind::IamRole,
            "role",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-types-q",
            &account_id,
            ResourceKind::IamInstanceProfile,
            "profile",
            "us-east-2",
        )
        .await
        .unwrap();

        let resources = get_run_resources(&pool, "run-types-q").await.unwrap();
        assert_eq!(resources.len(), 3);

        // Check resource types are parsed correctly
        let types: Vec<ResourceKind> = resources.iter().map(|r| r.resource_type).collect();
        assert!(types.contains(&ResourceKind::S3Bucket));
        assert!(types.contains(&ResourceKind::IamRole));
        assert!(types.contains(&ResourceKind::IamInstanceProfile));
    }

    #[tokio::test]
    async fn test_get_undeleted_resources_across_runs() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        // Create two runs with resources
        insert_run(&pool, "run-a", &account_id, "us-east-2", &[], "test")
            .await
            .unwrap();
        insert_run(&pool, "run-b", &account_id, "us-west-2", &[], "test")
            .await
            .unwrap();

        insert_resource(
            &pool,
            "run-a",
            &account_id,
            ResourceKind::S3Bucket,
            "bucket-a",
            "us-east-2",
        )
        .await
        .unwrap();
        insert_resource(
            &pool,
            "run-b",
            &account_id,
            ResourceKind::S3Bucket,
            "bucket-b",
            "us-west-2",
        )
        .await
        .unwrap();

        // All should be returned
        let resources = get_undeleted_resources(&pool).await.unwrap();
        assert_eq!(resources.len(), 2);

        // Delete one
        mark_resource_deleted(&pool, ResourceKind::S3Bucket, "bucket-a")
            .await
            .unwrap();

        // Only one should remain
        let resources = get_undeleted_resources(&pool).await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].resource_id, "bucket-b");
    }

    #[tokio::test]
    async fn test_get_undeleted_resources_empty_database() {
        let pool = open_test_db().await.unwrap();

        let resources = get_undeleted_resources(&pool).await.unwrap();
        assert!(resources.is_empty());
    }

    #[tokio::test]
    async fn test_resource_has_correct_metadata() {
        let pool = open_test_db().await.unwrap();
        let account_id = test_account_id();

        insert_run(&pool, "run-meta", &account_id, "us-west-1", &[], "test")
            .await
            .unwrap();
        insert_resource(
            &pool,
            "run-meta",
            &account_id,
            ResourceKind::Ec2Instance,
            "i-meta",
            "us-west-1",
        )
        .await
        .unwrap();

        let resources = get_run_resources(&pool, "run-meta").await.unwrap();
        assert_eq!(resources.len(), 1);

        let resource = &resources[0];
        assert_eq!(resource.run_id, "run-meta");
        assert_eq!(resource.account_id, "123456789012");
        assert_eq!(resource.resource_type, ResourceKind::Ec2Instance);
        assert_eq!(resource.resource_id, "i-meta");
        assert_eq!(resource.region, "us-west-1");
        assert!(resource.deleted_at.is_none());
    }
}
