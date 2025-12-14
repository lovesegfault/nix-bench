//! Database setup and schema management

use anyhow::{Context, Result};
use directories::ProjectDirs;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
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

    let options = SqliteConnectOptions::from_str(&db_url)?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("Failed to open state database")?;

    setup_schema(&pool).await?;

    Ok(pool)
}

/// Setup database schema
async fn setup_schema(pool: &DbPool) -> Result<()> {
    // Check if we have the old schema (missing account_id column)
    let has_account_id: bool = sqlx::query_scalar(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('resources') WHERE name='account_id'",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false);

    if !has_account_id {
        let tables_exist: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='resources'",
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

/// Open an in-memory test database with schema
///
/// This is only available in tests and creates a fresh database
/// with the full schema set up.
#[cfg(test)]
pub(crate) async fn open_test_db() -> Result<DbPool> {
    use std::str::FromStr;

    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1) // Single connection for in-memory to maintain state
        .connect_with(options)
        .await
        .context("Failed to open test database")?;

    setup_schema(&pool).await?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_test_db_creates_schema() {
        let pool = open_test_db().await.unwrap();

        // Verify runs table exists
        let runs_exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='runs'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(runs_exists, "runs table should exist");

        // Verify resources table exists
        let resources_exists: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='resources'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(resources_exists, "resources table should exist");

        // Verify account_id column exists in resources
        let has_account_id: bool = sqlx::query_scalar(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('resources') WHERE name='account_id'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(has_account_id, "resources should have account_id column");
    }

    #[tokio::test]
    async fn test_schema_is_idempotent() {
        let pool = open_test_db().await.unwrap();

        // Run schema setup again - should not error
        let result = setup_schema(&pool).await;
        assert!(result.is_ok(), "Schema setup should be idempotent");
    }
}
