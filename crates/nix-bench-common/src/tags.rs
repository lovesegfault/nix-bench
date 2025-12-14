//! AWS resource tag constants for nix-bench
//!
//! All nix-bench-created AWS resources are tagged with these standard tags
//! to enable discovery, cleanup, and lifecycle management.
//!
//! ## Tag Schema
//!
//! | Tag Key | Description |
//! |---------|-------------|
//! | `nix-bench:tool` | Static identifier ("nix-bench") |
//! | `nix-bench:run-id` | Unique run identifier (UUID) |
//! | `nix-bench:created-at` | RFC 3339 creation timestamp |
//! | `nix-bench:status` | Lifecycle status (creating/active) |
//! | `nix-bench:instance-type` | EC2 instance type (optional) |

/// Tag key for tool identification - all nix-bench resources have this
pub const TAG_TOOL: &str = "nix-bench:tool";

/// Tag value for tool identification
pub const TAG_TOOL_VALUE: &str = "nix-bench";

/// Tag key for run ID - unique identifier per benchmark run
pub const TAG_RUN_ID: &str = "nix-bench:run-id";

/// Tag key for creation timestamp (RFC 3339 format)
pub const TAG_CREATED_AT: &str = "nix-bench:created-at";

/// Tag key for resource lifecycle status
pub const TAG_STATUS: &str = "nix-bench:status";

/// Tag key for instance type association (EC2)
pub const TAG_INSTANCE_TYPE: &str = "nix-bench:instance-type";

/// Resource lifecycle status values
pub mod status {
    /// Resource is being created - not yet fully initialized
    pub const CREATING: &str = "creating";

    /// Resource is active and in use
    pub const ACTIVE: &str = "active";
}

/// Helper to format creation timestamp for tags
pub fn format_created_at(time: chrono::DateTime<chrono::Utc>) -> String {
    time.to_rfc3339()
}

/// Helper to parse creation timestamp from tags
pub fn parse_created_at(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_format_parse_roundtrip() {
        let now = Utc::now();
        let formatted = format_created_at(now);
        let parsed = parse_created_at(&formatted).unwrap();

        // Timestamps should be within 1 second (sub-second precision may vary)
        let diff = (now - parsed).num_seconds().abs();
        assert!(diff <= 1, "Roundtrip diff {} > 1 second", diff);
    }

    #[test]
    fn test_parse_invalid() {
        assert!(parse_created_at("not a timestamp").is_none());
        assert!(parse_created_at("").is_none());
    }
}
