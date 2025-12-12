//! Integration tests for SQLite state management
//!
//! These tests use a temporary database file and clean up after themselves.

use anyhow::Result;
use rusqlite::Connection;
use tempfile::TempDir;
use uuid::Uuid;

/// Test ID generator
fn test_id() -> String {
    Uuid::now_v7().to_string()
}

/// Create a test database in a temporary directory
fn create_test_db(temp_dir: &TempDir) -> Result<Connection> {
    let db_path = temp_dir.path().join("test-state.db");
    let conn = Connection::open(&db_path)?;

    // Create tables (same schema as production)
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
    )?;

    Ok(conn)
}

#[test]
fn test_create_run() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();

    let instances = vec!["c7id.metal".to_string(), "m8a.48xlarge".to_string()];
    let instances_json = serde_json::to_string(&instances)?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![run_id, now, "running", "us-east-2", instances_json, "large-deep"],
    )?;

    // Verify
    let mut stmt = conn.prepare("SELECT run_id, status, attr FROM runs WHERE run_id = ?1")?;
    let (db_run_id, status, attr): (String, String, String) =
        stmt.query_row([&run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;

    assert_eq!(db_run_id, run_id);
    assert_eq!(status, "running");
    assert_eq!(attr, "large-deep");

    Ok(())
}

#[test]
fn test_create_and_track_resources() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();
    let now = chrono::Utc::now().to_rfc3339();

    // Create run first
    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, 'running', 'us-east-2', '[]', 'test')",
        rusqlite::params![run_id, now],
    )?;

    // Create resources
    let resources = vec![
        ("ec2_instance", "i-abc123"),
        ("ec2_instance", "i-def456"),
        ("s3_bucket", "nix-bench-test-bucket"),
    ];

    for (resource_type, resource_id) in &resources {
        conn.execute(
            "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![run_id, resource_type, resource_id, "us-east-2", now],
        )?;
    }

    // Verify resources created
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1",
        [&run_id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 3);

    // Verify undeleted count
    let undeleted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1 AND deleted_at IS NULL",
        [&run_id],
        |row| row.get(0),
    )?;
    assert_eq!(undeleted, 3);

    Ok(())
}

#[test]
fn test_mark_resources_deleted() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();
    let now = chrono::Utc::now().to_rfc3339();

    // Create run and resources
    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, 'running', 'us-east-2', '[]', 'test')",
        rusqlite::params![run_id, now],
    )?;

    conn.execute(
        "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
         VALUES (?1, 'ec2_instance', 'i-test123', 'us-east-2', ?2)",
        rusqlite::params![run_id, now],
    )?;

    // Mark as deleted
    let delete_time = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE resources SET deleted_at = ?1
         WHERE resource_type = 'ec2_instance' AND resource_id = 'i-test123'",
        [&delete_time],
    )?;

    // Verify deleted
    let deleted_at: Option<String> = conn.query_row(
        "SELECT deleted_at FROM resources WHERE resource_id = 'i-test123'",
        [],
        |row| row.get(0),
    )?;
    assert!(deleted_at.is_some());

    // Verify undeleted count is 0
    let undeleted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1 AND deleted_at IS NULL",
        [&run_id],
        |row| row.get(0),
    )?;
    assert_eq!(undeleted, 0);

    Ok(())
}

#[test]
fn test_update_run_status() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();
    let now = chrono::Utc::now().to_rfc3339();

    // Create run
    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, 'running', 'us-east-2', '[]', 'test')",
        rusqlite::params![run_id, now],
    )?;

    // Update to completed
    conn.execute(
        "UPDATE runs SET status = 'completed' WHERE run_id = ?1",
        [&run_id],
    )?;

    let status: String = conn.query_row(
        "SELECT status FROM runs WHERE run_id = ?1",
        [&run_id],
        |row| row.get(0),
    )?;
    assert_eq!(status, "completed");

    Ok(())
}

#[test]
fn test_orphaned_resources_query() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();
    let now = chrono::Utc::now().to_rfc3339();

    // Create run with orphaned status
    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, 'orphaned', 'us-east-2', '[]', 'test')",
        rusqlite::params![run_id, now],
    )?;

    // Create undeleted resources
    conn.execute(
        "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
         VALUES (?1, 'ec2_instance', 'i-orphan1', 'us-east-2', ?2)",
        rusqlite::params![run_id, now],
    )?;

    conn.execute(
        "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
         VALUES (?1, 's3_bucket', 'orphan-bucket', 'us-east-2', ?2)",
        rusqlite::params![run_id, now],
    )?;

    // Query for orphaned resources (same as cleanup command)
    let mut stmt = conn.prepare(
        "SELECT resource_type, resource_id, region FROM resources WHERE deleted_at IS NULL",
    )?;

    let orphans: Vec<(String, String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(orphans.len(), 2);
    assert!(orphans.iter().any(|(_, id, _)| id == "i-orphan1"));
    assert!(orphans.iter().any(|(_, id, _)| id == "orphan-bucket"));

    Ok(())
}

#[test]
fn test_cleanup_flow() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let run_id = test_id();
    let now = chrono::Utc::now().to_rfc3339();

    // Create orphaned run with resources
    conn.execute(
        "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
         VALUES (?1, ?2, 'orphaned', 'us-east-2', '[]', 'test')",
        rusqlite::params![run_id, now],
    )?;

    conn.execute(
        "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
         VALUES (?1, 'ec2_instance', 'i-cleanup', 'us-east-2', ?2)",
        rusqlite::params![run_id, now],
    )?;

    // Simulate cleanup: mark resource deleted
    let delete_time = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE resources SET deleted_at = ?1 WHERE resource_id = 'i-cleanup'",
        [&delete_time],
    )?;

    // Update run status if all resources cleaned
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

    // Verify run is now completed
    let status: String = conn.query_row(
        "SELECT status FROM runs WHERE run_id = ?1",
        [&run_id],
        |row| row.get(0),
    )?;
    assert_eq!(status, "completed");

    Ok(())
}

#[test]
fn test_multiple_runs_isolation() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let conn = create_test_db(&temp_dir)?;
    let now = chrono::Utc::now().to_rfc3339();

    // Create two separate runs with unique IDs
    let run_id_1 = test_id();
    // Small delay to ensure different UUIDv7
    std::thread::sleep(std::time::Duration::from_millis(2));
    let run_id_2 = test_id();

    for run_id in [&run_id_1, &run_id_2] {
        conn.execute(
            "INSERT INTO runs (run_id, created_at, status, region, instances, attr)
             VALUES (?1, ?2, 'running', 'us-east-2', '[]', 'test')",
            rusqlite::params![run_id, now],
        )?;

        // Use full run_id for truly unique resource_id
        conn.execute(
            "INSERT INTO resources (run_id, resource_type, resource_id, region, created_at)
             VALUES (?1, 'ec2_instance', ?2, 'us-east-2', ?3)",
            rusqlite::params![run_id, format!("i-{}", run_id), now],
        )?;
    }

    // Verify isolation
    let count_1: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1",
        [&run_id_1],
        |row| row.get(0),
    )?;
    let count_2: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1",
        [&run_id_2],
        |row| row.get(0),
    )?;

    assert_eq!(count_1, 1);
    assert_eq!(count_2, 1);

    // Deleting resources for run_1 shouldn't affect run_2
    conn.execute(
        "UPDATE resources SET deleted_at = ?1 WHERE run_id = ?2",
        rusqlite::params![now, run_id_1],
    )?;

    let undeleted_2: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resources WHERE run_id = ?1 AND deleted_at IS NULL",
        [&run_id_2],
        |row| row.get(0),
    )?;
    assert_eq!(undeleted_2, 1);

    Ok(())
}
