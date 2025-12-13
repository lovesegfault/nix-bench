//! AWS service clients

#[cfg(feature = "coordinator")]
pub mod account;
pub mod ec2;
pub mod error;
pub mod grpc_client;
pub mod iam;
pub mod s3;

#[cfg(feature = "coordinator")]
pub use account::{get_current_account_id, AccountId};
pub use ec2::{get_coordinator_public_ip, Ec2Client};
pub use error::{classify_anyhow_error, classify_aws_error, AwsError};
pub use grpc_client::{
    start_log_streaming_unified, wait_for_tcp_ready, ChannelOptions, GrpcChannelBuilder,
    GrpcInstanceStatus, GrpcLogClient, GrpcStatusPoller, LogOutput, LogStreamingOptions,
};

// Deprecated: Use start_log_streaming_unified with LogStreamingOptions instead
#[deprecated(since = "0.2.0", note = "Use start_log_streaming_unified with LogStreamingOptions")]
pub use grpc_client::start_log_streaming;
#[deprecated(since = "0.2.0", note = "Use start_log_streaming_unified with LogStreamingOptions")]
pub use grpc_client::start_log_streaming_stdout;
#[deprecated(since = "0.2.0", note = "Use start_log_streaming_unified with LogStreamingOptions")]
pub use grpc_client::start_log_streaming_with_tls;
#[deprecated(since = "0.2.0", note = "Use start_log_streaming_unified with LogStreamingOptions")]
pub use grpc_client::start_log_streaming_stdout_with_tls;
pub use iam::IamClient;
pub use s3::S3Client;
