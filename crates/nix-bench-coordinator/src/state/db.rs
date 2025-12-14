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
