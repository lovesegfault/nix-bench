//! SQLite state management for tracking AWS resources
//!
//! Uses sqlx for async database access with a connection pool.

mod cleanup;
mod cli;
mod crud;
mod db;
mod queries;
mod types;

// Re-export types
pub use db::{open_db, DbPool};
pub use types::{parse_resource_type, Resource, ResourceType, RunStatus};

// Re-export CRUD operations
pub use crud::{insert_resource, insert_run, mark_resource_deleted, update_run_status};

// Re-export query operations
pub use queries::{get_run_resources, get_undeleted_resources};

// Re-export CLI operations
pub use cli::{list_resources, prune_database};

// Re-export cleanup operations
pub use cleanup::cleanup_resources;
