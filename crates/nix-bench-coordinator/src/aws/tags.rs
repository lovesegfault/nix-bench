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

/// Build an EC2 TagSpecification with standard nix-bench tags plus optional extra tags.
pub fn ec2_tag_spec(
    resource_type: aws_sdk_ec2::types::ResourceType,
    run_id: &str,
    extra_tags: &[(&str, &str)],
) -> aws_sdk_ec2::types::TagSpecification {
    use aws_sdk_ec2::types::{Tag, TagSpecification};

    let created_at = format_created_at(chrono::Utc::now());
    let mut builder = TagSpecification::builder()
        .resource_type(resource_type)
        .tags(Tag::builder().key(TAG_TOOL).value(TAG_TOOL_VALUE).build())
        .tags(Tag::builder().key(TAG_RUN_ID).value(run_id).build())
        .tags(
            Tag::builder()
                .key(TAG_CREATED_AT)
                .value(&created_at)
                .build(),
        )
        .tags(
            Tag::builder()
                .key(TAG_STATUS)
                .value(status::CREATING)
                .build(),
        );
    for (k, v) in extra_tags {
        builder = builder.tags(Tag::builder().key(*k).value(*v).build());
    }
    builder.build()
}

/// Build S3 Tagging with standard nix-bench tags.
pub fn s3_tagging(run_id: &str) -> anyhow::Result<aws_sdk_s3::types::Tagging> {
    use aws_sdk_s3::types::Tag;

    let created_at = format_created_at(chrono::Utc::now());
    Ok(aws_sdk_s3::types::Tagging::builder()
        .tag_set(Tag::builder().key(TAG_TOOL).value(TAG_TOOL_VALUE).build()?)
        .tag_set(Tag::builder().key(TAG_RUN_ID).value(run_id).build()?)
        .tag_set(
            Tag::builder()
                .key(TAG_CREATED_AT)
                .value(&created_at)
                .build()?,
        )
        .tag_set(
            Tag::builder()
                .key(TAG_STATUS)
                .value(status::CREATING)
                .build()?,
        )
        .build()?)
}
