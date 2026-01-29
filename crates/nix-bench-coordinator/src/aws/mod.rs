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
pub mod tags;

// Core clients
pub use account::{AccountId, get_current_account_id};
pub use ec2::{Ec2Client, LaunchInstanceConfig, LaunchedInstance, get_coordinator_public_ip};
pub use iam::IamClient;
pub use s3::S3Client;

// Error handling
pub use error::{AwsError, classify_anyhow_error, classify_aws_error, ignore_not_found};

// Cleanup utilities
pub use cleanup::{CleanupResult, delete_resource};

// gRPC client
pub use grpc_client::{
    GrpcInstanceStatus, GrpcLogClient, GrpcStatusPoller, LogStreamingOptions, send_ack_complete,
    start_log_streaming_unified, wait_for_tcp_ready,
};
