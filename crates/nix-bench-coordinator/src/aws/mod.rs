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
pub mod context;
pub mod ec2;
pub mod error;
pub mod grpc_client;
pub mod iam;
pub mod resource_guard;
pub mod s3;
pub mod scanner;

pub use account::{get_current_account_id, AccountId};
pub use context::AwsContext;
pub use ec2::{get_coordinator_public_ip, Ec2Client, Ec2Operations, LaunchedInstance};
pub use error::{classify_anyhow_error, classify_aws_error, AwsError};
pub use grpc_client::{
    start_log_streaming_unified, wait_for_tcp_ready, ChannelOptions, GrpcChannelBuilder,
    GrpcInstanceStatus, GrpcLogClient, GrpcStatusPoller, LogOutput, LogStreamingOptions,
};
pub use iam::{IamClient, IamOperations};
pub use s3::{S3Client, S3Operations};

// Re-export mock types for tests
#[cfg(test)]
pub use ec2::MockEc2Operations;
#[cfg(test)]
pub use iam::MockIamOperations;
#[cfg(test)]
pub use s3::MockS3Operations;


use aws_sdk_cloudwatch::types::Dimension;
use nix_bench_common::metrics::dimensions;

/// Build the dimension vector for CloudWatch metrics.
///
/// This function creates AWS SDK `Dimension` objects using the constants
/// from `nix_bench_common::metrics::dimensions`.
pub fn build_dimensions(run_id: &str, instance_type: &str, system: &str) -> Vec<Dimension> {
    vec![
        Dimension::builder()
            .name(dimensions::RUN_ID)
            .value(run_id)
            .build(),
        Dimension::builder()
            .name(dimensions::INSTANCE_TYPE)
            .value(instance_type)
            .build(),
        Dimension::builder()
            .name(dimensions::SYSTEM)
            .value(system)
            .build(),
    ]
}
