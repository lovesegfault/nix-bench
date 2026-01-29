//! RAII-style resource guards for AWS resource cleanup
//!
//! Provides automatic cleanup of AWS resources when guards are dropped
//! without being committed.

mod guard;
pub mod types;

pub use guard::{
    CleanupExecutor, CleanupMessage, Ec2InstanceGuard, IamRoleGuard, ResourceGuard,
    ResourceGuardBuilder, ResourceRegistry, S3BucketGuard, SecurityGroupGuard,
    SecurityGroupRuleGuard, create_cleanup_system,
};
pub use types::{ResourceId, ResourceMeta};
