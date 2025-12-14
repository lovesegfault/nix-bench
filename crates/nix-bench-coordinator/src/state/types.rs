//! State types and type aliases

use chrono::{DateTime, Utc};
use nix_bench_common::ResourceKind;

/// Type alias for backward compatibility
pub type ResourceType = ResourceKind;

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

/// Parse ResourceType from string with fallback to S3Object
pub fn parse_resource_type(s: &str) -> ResourceType {
    ResourceKind::parse(s).unwrap_or(ResourceKind::S3Object)
}

/// A tracked resource
#[derive(Debug, Clone)]
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
