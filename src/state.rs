//! SQLite state management for tracking AWS resources
//!
//! Uses sqlx for async database access with a connection pool.

use crate::aws::AccountId;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use tracing::warn;

/// Database connection pool type alias
pub type DbPool = SqlitePool;

/// Get the state database path
fn get_db_path() -> Result<PathBuf> {
    let proj_dirs =
        ProjectDirs::from("", "", "nix-bench").context("Failed to get project directories")?;

    let state_dir = proj_dirs.data_local_dir();
    fs::create_dir_all(state_dir).context("Failed to create state directory")?;

    Ok(state_dir.join("state.db"))
}

/// Open the state database, creating it if needed
pub async fn open_db() -> Result<DbPool> {
    let path = get_db_path()?;
    let db_url = format!("sqlite://{}?mode=rwc", path.display());

    let options = SqliteConnectOptions::from_str(&db_url)?
        .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("Failed to open state database")?;

    // Run migrations / schema setup
    setup_schema(&pool).await?;

    Ok(pool)
}

/// Setup database schema
async fn setup_schema(pool: &DbPool) -> Result<()> {
    // Check if we have the old schema (missing account_id column)
    let has_account_id: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('resources') WHERE name='account_id'"
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_account_id {
        // Check if tables exist at all
        let tables_exist: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='resources'"
        )
        .fetch_one(pool)
        .await
        .unwrap_or(false);

        if tables_exist {
            warn!("Old schema detected (missing account_id) - dropping and recreating tables");
            sqlx::query("DROP TABLE IF EXISTS resources")
                .execute(pool)
                .await?;
            sqlx::query("DROP TABLE IF EXISTS runs")
                .execute(pool)
                .await?;
        }
    }

    // Create tables with account_id column
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS runs (
            run_id TEXT PRIMARY KEY,
            account_id TEXT NOT NULL,
            created_at TEXT NOT NULL,
            status TEXT NOT NULL,
            region TEXT NOT NULL,
            instances TEXT NOT NULL,
            attr TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS resources (
            id INTEGER PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(run_id),
            account_id TEXT NOT NULL,
            resource_type TEXT NOT NULL,
            resource_id TEXT NOT NULL,
            region TEXT NOT NULL,
            created_at TEXT NOT NULL,
            deleted_at TEXT,
            UNIQUE(resource_type, resource_id, account_id)
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_resources_run ON resources(run_id)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_resources_type ON resources(resource_type)")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_resources_account ON resources(account_id)")
        .execute(pool)
        .await?;

    Ok(())
}

/// Run status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    Completed,
    Failed,
    Orphaned,
}

impl RunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Orphaned => "orphaned",
        }
    }
}

/// Resource type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Ec2Instance,
    S3Bucket,
    S3Object,
    IamRole,
    IamInstanceProfile,
    SecurityGroup,
    SecurityGroupRule,
    ElasticIp,
}

impl ResourceType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResourceType::Ec2Instance => "ec2_instance",
            ResourceType::S3Bucket => "s3_bucket",
            ResourceType::S3Object => "s3_object",
            ResourceType::IamRole => "iam_role",
            ResourceType::IamInstanceProfile => "iam_instance_profile",
            ResourceType::SecurityGroup => "security_group",
            ResourceType::SecurityGroupRule => "security_group_rule",
            ResourceType::ElasticIp => "elastic_ip",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "ec2_instance" => ResourceType::Ec2Instance,
            "s3_bucket" => ResourceType::S3Bucket,
            "s3_object" => ResourceType::S3Object,
            "iam_role" => ResourceType::IamRole,
            "iam_instance_profile" => ResourceType::IamInstanceProfile,
            "security_group" => ResourceType::SecurityGroup,
            "security_group_rule" => ResourceType::SecurityGroupRule,
            "elastic_ip" => ResourceType::ElasticIp,
            _ => ResourceType::S3Object,
        }
    }
}

/// A tracked resource
#[derive(Debug)]
pub struct Resource {
    pub id: i64,
    pub run_id: String,
    pub account_id: String,
    pub resource_type: ResourceType,
    pub resource_id: String,
    pub region: String,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

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
            resource_type: ResourceType::from_str(row.get("resource_type")),
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
            resource_type: ResourceType::from_str(row.get("resource_type")),
            resource_id: row.get("resource_id"),
            region: row.get("region"),
            created_at,
            deleted_at: None,
        });
    }

    Ok(resources)
}

/// List all tracked resources
pub async fn list_resources() -> Result<()> {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    let pool = open_db().await?;
    let resources = get_undeleted_resources(&pool).await?;

    if resources.is_empty() {
        println!("No tracked resources");
        return Ok(());
    }

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Type"),
            Cell::new("ID"),
            Cell::new("Region"),
            Cell::new("Account"),
            Cell::new("Created"),
        ]);

    for resource in resources {
        table.add_row(vec![
            Cell::new(resource.resource_type.as_str()),
            Cell::new(&resource.resource_id),
            Cell::new(&resource.region),
            Cell::new(&resource.account_id),
            Cell::new(resource.created_at.format("%Y-%m-%d %H:%M:%S").to_string()),
        ]);
    }

    println!("{table}");

    Ok(())
}

/// Check if an error indicates a resource doesn't exist
fn is_not_found_error(e: &anyhow::Error) -> bool {
    crate::aws::classify_anyhow_error(e).is_not_found()
}

/// Handle the result of a cleanup operation, updating DB and collecting errors
async fn handle_cleanup_result(
    pool: &DbPool,
    resource: &Resource,
    result: Result<()>,
    cleanup_errors: &mut Vec<String>,
) -> Result<()> {
    match result {
        Ok(()) => {
            mark_resource_deleted(pool, resource.resource_type, &resource.resource_id).await?;
            println!("    Deleted successfully");
        }
        Err(e) => {
            if is_not_found_error(&e) {
                mark_resource_deleted(pool, resource.resource_type, &resource.resource_id).await?;
                println!("    Already deleted (marking in DB)");
            } else {
                let error_msg = format!(
                    "{} {}: {}",
                    resource.resource_type.as_str(),
                    resource.resource_id,
                    e
                );
                println!("    Failed to delete: {}", e);
                cleanup_errors.push(error_msg);
            }
        }
    }
    Ok(())
}

/// Cleanup orphaned resources by actually terminating/deleting them in AWS
pub async fn cleanup_resources() -> Result<()> {
    use crate::aws::{get_current_account_id, Ec2Client, IamClient, S3Client};
    use std::collections::HashMap;

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

        println!("Processing account {} region {}...", stored_account_id, region);

        let ec2 = Ec2Client::new(&region).await?;
        let s3 = S3Client::new(&region).await?;
        let iam = IamClient::new(&region).await?;

        let (instances, other_resources): (Vec<_>, Vec<_>) = region_resources
            .into_iter()
            .partition(|r| r.resource_type == ResourceType::Ec2Instance);

        let (security_groups, non_sg_resources): (Vec<_>, Vec<_>) = other_resources
            .into_iter()
            .partition(|r| r.resource_type == ResourceType::SecurityGroup);

        // Terminate EC2 instances
        for resource in &instances {
            println!("  Cleaning up {} {}...", resource.resource_type.as_str(), resource.resource_id);
            let result = ec2.terminate_instance(&resource.resource_id).await;
            handle_cleanup_result(&pool, resource, result, &mut cleanup_errors).await?;
        }

        // Wait for instances to terminate
        if !instances.is_empty() && !security_groups.is_empty() {
            println!("  Waiting for instances to terminate...");
            for resource in &instances {
                let _ = ec2.wait_for_terminated(&resource.resource_id).await;
            }
        }

        // Delete non-security-group resources
        for resource in &non_sg_resources {
            println!("  Cleaning up {} {}...", resource.resource_type.as_str(), resource.resource_id);

            let result = match resource.resource_type {
                ResourceType::Ec2Instance => unreachable!(),
                ResourceType::S3Bucket => s3.delete_bucket(&resource.resource_id).await,
                ResourceType::S3Object => Ok(()),
                ResourceType::IamRole => iam.delete_benchmark_role(&resource.resource_id).await,
                ResourceType::IamInstanceProfile => Ok(()),
                ResourceType::SecurityGroupRule => {
                    if let Some((sg_id, cidr_ip)) = resource.resource_id.split_once(':') {
                        ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await
                    } else {
                        Err(anyhow::anyhow!("Invalid SecurityGroupRule resource_id format"))
                    }
                }
                ResourceType::ElasticIp => ec2.release_elastic_ip(&resource.resource_id).await,
                ResourceType::SecurityGroup => unreachable!(),
            };
            handle_cleanup_result(&pool, resource, result, &mut cleanup_errors).await?;
        }

        // Delete security groups
        for resource in &security_groups {
            println!("  Cleaning up {} {}...", resource.resource_type.as_str(), resource.resource_id);
            let result = ec2.delete_security_group(&resource.resource_id).await;
            handle_cleanup_result(&pool, resource, result, &mut cleanup_errors).await?;
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

/// Remove stale entries from the database
pub async fn prune_database() -> Result<()> {
    let pool = open_db().await?;

    let result = sqlx::query(
        "DELETE FROM resources
         WHERE deleted_at IS NOT NULL
         AND datetime(deleted_at) < datetime('now', '-30 days')",
    )
    .execute(&pool)
    .await?;

    println!("Pruned {} old resource records", result.rows_affected());

    let result = sqlx::query(
        "DELETE FROM runs
         WHERE status = 'completed'
         AND datetime(created_at) < datetime('now', '-30 days')
         AND NOT EXISTS (
             SELECT 1 FROM resources WHERE resources.run_id = runs.run_id
         )",
    )
    .execute(&pool)
    .await?;

    println!("Pruned {} old run records", result.rows_affected());

    Ok(())
}
