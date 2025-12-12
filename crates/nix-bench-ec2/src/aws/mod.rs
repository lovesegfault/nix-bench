//! AWS service clients

pub mod cloudwatch;
pub mod ec2;
pub mod s3;

pub use cloudwatch::CloudWatchClient;
pub use ec2::Ec2Client;
pub use s3::S3Client;
