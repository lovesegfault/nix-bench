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
