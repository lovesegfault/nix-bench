//! Database test utilities
//!
//! Provides in-memory SQLite database setup for testing.
//!
//! Note: Schema setup should be done by the consuming crate since
//! schema definitions live in the coordinator crate.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;

/// Database connection pool type alias
pub type TestDbPool = SqlitePool;

/// Create an in-memory SQLite connection pool for testing.
///
/// This creates a fresh database with no schema. The caller is responsible
/// for setting up any required schema.
///
/// # Example
///
/// ```ignore
/// use nix_bench_test_utils::db::open_test_db;
///
/// #[tokio::test]
/// async fn test_database() {
///     let pool = open_test_db().await.unwrap();
///     // Setup schema and run tests...
/// }
/// ```
pub async fn open_test_db() -> Result<TestDbPool> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1) // Single connection for in-memory to maintain state
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Create an in-memory SQLite connection pool with a specific identifier.
///
/// Using the same identifier across tests allows sharing an in-memory database.
/// Different identifiers create isolated databases.
///
/// # Arguments
///
/// * `name` - A unique identifier for this database instance
pub async fn open_named_test_db(name: &str) -> Result<TestDbPool> {
    // Use file::name?mode=memory&cache=shared for named in-memory DBs
    let uri = format!("sqlite:file:{}?mode=memory&cache=shared", name);
    let options = SqliteConnectOptions::from_str(&uri)?.create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_test_db() {
        let pool = open_test_db().await.unwrap();

        // Verify we can execute queries
        let result: (i64,) = sqlx::query_as("SELECT 1 + 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(result.0, 2);
    }

    #[tokio::test]
    async fn test_open_named_test_db() {
        let pool = open_named_test_db("test_db_1").await.unwrap();

        // Create a table
        sqlx::query("CREATE TABLE test_table (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();

        // Verify table exists
        let result: (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='test_table'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(result.0, 1);
    }
}
