//! SQLite state management for tracking AWS resources

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::ProjectDirs;
use rusqlite::{params, Connection};
use std::fs;
use std::path::PathBuf;

/// Get the state database path
fn get_db_path() -> Result<PathBuf> {
    let proj_dirs = ProjectDirs::from("", "", "nix-bench")
        .context("Failed to get project directories")?;

    let state_dir = proj_dirs.data_local_dir();
    fs::create_dir_all(state_dir).context("Failed to create state directory")?;

    Ok(state_dir.join("state.db"))
}

/// Open the state database, creating it if needed
pub fn open_db() -> Result<Connection> {
    let path = get_db_path()?;
    let conn = Connection::open(&path).context("Failed to open state database")?;

    // Create tables if they don't exist
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS runs (
            run_id TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            status TEXT NOT NULL,
            region TEXT NOT NULL,
            instances TEXT NOT NULL,
            attr TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS resources (
            id INTEGER PRIMARY KEY,
            run_id TEXT NOT NULL REFERENCES runs(run_id),
            resource_type TEXT NOT NULL,
            resource_id TEXT NOT NULL,
            region TEXT NOT NULL,
            created_at TEXT NOT NULL,
            deleted_at TEXT,
            UNIQUE(resource_type, resource_id)
        );

        CREATE INDEX IF NOT EXISTS idx_resources_run ON resources(run_id);
        CREATE INDEX IF NOT EXISTS idx_resources_type ON resources(resource_type);
        "#,
    )
    .context("Failed to create tables")?;

    Ok(conn)
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
    fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Completed => "completed",
            RunStatus::Failed => "failed",
            RunStatus::Orphaned => "orphaned",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "running" => RunStatus::Running,
            "completed" => RunStatus::Completed,
            "failed" => RunStatus::Failed,
            "orphaned" => RunStatus::Orphaned,
            _ => RunStatus::Orphaned,
        }
    }
}

/// Resource type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceType {
    Ec2Instance,
    S3Bucket,
    S3Object,
}

impl ResourceType {
    fn as_str(&self) -> &'static str {
        match self {
            ResourceType::Ec2Instance => "ec2_instance",
            ResourceType::S3Bucket => "s3_bucket",
            ResourceType::S3Object => "s3_object",
        }
    }

    fn from_str(s: &str) -> Self {
        match s {
            "ec2_instance" => ResourceType::Ec2Instance,
            "s3_bucket" => ResourceType::S3Bucket,
            "s3_object" => ResourceType::S3Object,
            _ => ResourceType::S3Object,
        }
    }
}

/// A tracked resource
#[derive(Debug)]
pub struct Resource {
    pub id: i64,
    pub run_id: String,
    pub resource_type: ResourceType,
    pub resource_id: String,
    pub region: String,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// Insert a new run
pub fn insert_run(
    conn: &Connection,
    run_id: &str,
    region: &str,
    instances: &[String],
    attr: &str,
) -> Result<()> {
    let instances_json = serde_json::to_string(instances)?;
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![run_id, now, RunStatus::Running.as_str(), region, instances_json, attr],
    )?;

    Ok(())
}

/// Insert a resource
pub fn insert_resource(
    conn: &Connection,
    run_id: &str,
    resource_type: ResourceType,
    resource_id: &str,
    region: &str,
) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![run_id, resource_type.as_str(), resource_id, region, now],
    )?;

    Ok(())
}

/// Mark a resource as deleted
pub fn mark_resource_deleted(conn: &Connection, resource_type: ResourceType, resource_id: &str) -> Result<()> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "UPDATE resources SET deleted_at = ?1
         WHERE resource_type = ?2 AND resource_id = ?3 AND deleted_at IS NULL",
        params![now, resource_type.as_str(), resource_id],
    )?;

    Ok(())
}

/// Update run status
pub fn update_run_status(conn: &Connection, run_id: &str, status: RunStatus) -> Result<()> {
    conn.execute(
        "UPDATE runs SET status = ?1 WHERE run_id = ?2",
        params![status.as_str(), run_id],
    )?;

    Ok(())
}

/// Get all undeleted resources
pub fn get_undeleted_resources(conn: &Connection) -> Result<Vec<Resource>> {
    let mut stmt = conn.prepare(
        "SELECT id, run_id, resource_type, resource_id, region, created_at, deleted_at
         FROM resources WHERE deleted_at IS NULL"
    )?;

    let resources = stmt
        .query_map([], |row| {
            Ok(Resource {
                id: row.get(0)?,
                run_id: row.get(1)?,
                resource_type: ResourceType::from_str(&row.get::<_, String>(2)?),
                resource_id: row.get(3)?,
                region: row.get(4)?,
                created_at: DateTime::parse_from_rfc3339(&row.get::<_, String>(5)?)
                    .unwrap()
                    .with_timezone(&Utc),
                deleted_at: None,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(resources)
}

/// List all tracked resources
pub async fn list_resources() -> Result<()> {
    let conn = open_db()?;
    let resources = get_undeleted_resources(&conn)?;

    if resources.is_empty() {
        println!("No tracked resources");
        return Ok(());
    }

    println!("{:<15} {:<40} {:<15} {:<25}", "TYPE", "ID", "REGION", "CREATED");
    println!("{}", "-".repeat(95));

    for resource in resources {
        println!(
            "{:<15} {:<40} {:<15} {:<25}",
            resource.resource_type.as_str(),
            resource.resource_id,
            resource.region,
            resource.created_at.format("%Y-%m-%d %H:%M:%S")
        );
    }

    Ok(())
}

/// Cleanup orphaned resources
pub async fn cleanup_resources() -> Result<()> {
    let conn = open_db()?;
    let resources = get_undeleted_resources(&conn)?;

    if resources.is_empty() {
        println!("No orphaned resources to clean up");
        return Ok(());
    }

    println!("Found {} undeleted resources", resources.len());

    for resource in resources {
        println!(
            "Checking {} {}...",
            resource.resource_type.as_str(),
            resource.resource_id
        );

        // TODO: Check if resource exists in AWS and delete it
        // For now, just mark as deleted in DB
        mark_resource_deleted(&conn, resource.resource_type, &resource.resource_id)?;
        println!("  Marked as deleted");
    }

    // Update orphaned runs to completed
    conn.execute(
        "UPDATE runs SET status = 'completed'
         WHERE status = 'orphaned'
         AND NOT EXISTS (
             SELECT 1 FROM resources
             WHERE resources.run_id = runs.run_id
             AND resources.deleted_at IS NULL
         )",
        [],
    )?;

    println!("Cleanup complete");
    Ok(())
}

/// Remove stale entries from the database
pub fn prune_database() -> Result<()> {
    let conn = open_db()?;

    // Delete resources older than 30 days that are already marked deleted
    let deleted = conn.execute(
        "DELETE FROM resources
         WHERE deleted_at IS NOT NULL
         AND datetime(deleted_at) < datetime('now', '-30 days')",
        [],
    )?;

    println!("Pruned {} old resource records", deleted);

    // Delete completed runs older than 30 days
    let deleted = conn.execute(
        "DELETE FROM runs
         WHERE status = 'completed'
         AND datetime(created_at) < datetime('now', '-30 days')
         AND NOT EXISTS (
             SELECT 1 FROM resources WHERE resources.run_id = runs.run_id
         )",
        [],
    )?;

    println!("Pruned {} old run records", deleted);

    Ok(())
}
