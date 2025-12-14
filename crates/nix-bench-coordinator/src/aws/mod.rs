//! AWS client modules for the coordinator
//!
//! This module provides wrappers around AWS SDK clients for:
//! - EC2: Instance management
//! - IAM: Role and instance profile management
//! - S3: Results storage and agent binary distribution
//! - STS: Account ID lookup
//! - gRPC: Client for communicating with agents
//! - resource_guard: RAII cleanup for AWS resources

pub mod account;
pub mod cleanup;
pub mod cleanup_util;
pub(crate) mod context;
pub mod ec2;
pub mod error;
pub mod grpc_client;
pub mod iam;
pub mod resource_guard;
pub mod s3;
pub mod scanner;

// Core clients
pub use account::{get_current_account_id, AccountId};
pub use ec2::{get_coordinator_public_ip, Ec2Client, Ec2Operations, LaunchInstanceConfig, LaunchedInstance};
pub use iam::{IamClient, IamOperations};
pub use s3::{S3Client, S3Operations};

// Error handling
pub use error::{classify_anyhow_error, classify_aws_error, AwsError};

// Cleanup utilities
pub use cleanup_util::{delete_resource, partition_resources_for_cleanup, CleanupResult};

// gRPC client
pub use grpc_client::{
    start_log_streaming_unified, wait_for_tcp_ready, GrpcInstanceStatus, GrpcLogClient,
    GrpcStatusPoller, LogStreamingOptions,
};
