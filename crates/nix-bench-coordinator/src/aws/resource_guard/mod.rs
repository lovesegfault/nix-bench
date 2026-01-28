//! RAII-style resource guards for AWS resource cleanup
//!
//! This module provides automatic cleanup of AWS resources when guards are dropped
//! without being committed. It addresses the problem of resources being created
//! but not recorded to the database if the process crashes between creation and
//! database write.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  ResourceGuard<T>                                           │
//! │  - RAII wrapper, tracks resource immediately on creation    │
//! │  - Drop triggers cleanup via channel to executor            │
//! │  - commit() transfers ownership to DB (no cleanup)          │
//! └─────────────────────────────────────────────────────────────┘
//!                             │
//!                             ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │  ResourceRegistry                                           │
//! │  - Thread-safe tracking of live resources                   │
//! │  - Sends cleanup messages to executor on drop               │
//! └─────────────────────────────────────────────────────────────┘
//!                             │
//!                             ▼
//! ┌─────────────────────────────────────────────────────────────┐
//! │  CleanupExecutor                                            │
//! │  - Background task for async cleanup                        │
//! │  - Processes resources in dependency order                  │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! // Create the cleanup system
//! let (registry, executor) = create_cleanup_system();
//!
//! // Spawn the executor as a background task
//! let executor_handle = tokio::spawn(executor.run());
//!
//! // Create a builder for consistent metadata
//! let builder = ResourceGuardBuilder::new(registry.clone(), "run-123", "us-east-2");
//!
//! // Create resources with guards
//! s3.create_bucket("my-bucket").await?;
//! let bucket_guard = builder.s3_bucket("my-bucket");
//! // ^^ If we crash here, bucket will be cleaned up by executor
//!
//! let instance = ec2.launch_instance(...).await?;
//! let instance_guard = builder.ec2_instance(instance.instance_id.clone());
//!
//! // Commit guards - no cleanup will happen
//! bucket_guard.commit();
//! instance_guard.commit();
//!
//! // Shutdown cleanup system when done
//! registry.shutdown();
//! let _ = executor_handle.await;
//! ```

mod builder;
mod executor;
mod guard;
mod registry;
mod types;

pub use builder::ResourceGuardBuilder;
pub use executor::{CleanupExecutor, create_cleanup_system};
pub use guard::{
    Ec2InstanceGuard, IamRoleGuard, ResourceGuard, S3BucketGuard, SecurityGroupGuard,
    SecurityGroupRuleGuard,
};
pub use registry::{CleanupMessage, ResourceRegistry};
pub use types::{ResourceId, ResourceMeta};
