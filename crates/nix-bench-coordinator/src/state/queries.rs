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
