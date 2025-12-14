//! EC2 operations trait for testing

use super::types::{LaunchInstanceConfig, LaunchedInstance};
use super::Ec2Client;
use anyhow::Result;

/// Trait for EC2 operations that can be mocked in tests.
///
/// This trait abstracts the EC2 client operations to enable unit testing
/// of orchestration logic without hitting real AWS.
#[allow(async_fn_in_trait)]
#[cfg_attr(test, mockall::automock)]
pub trait Ec2Operations: Send + Sync {
    /// Launch an EC2 instance with the given configuration
    async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance>;

    /// Wait for instance to be running and get its public IP
    async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>>;

    /// Terminate an instance
    async fn terminate_instance(&self, instance_id: &str) -> Result<()>;

    /// Wait for instance to be fully terminated
    async fn wait_for_terminated(&self, instance_id: &str) -> Result<()>;

    /// Get console output from an instance
    async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>>;

    /// Create a security group
    async fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<String>,
    ) -> Result<String>;

    /// Delete a security group
    async fn delete_security_group(&self, security_group_id: &str) -> Result<()>;

    /// Add gRPC ingress rule to security group
    async fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()>;

    /// Remove gRPC ingress rule from security group
    async fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()>;
}

impl Ec2Operations for Ec2Client {
    async fn launch_instance(&self, config: LaunchInstanceConfig) -> Result<LaunchedInstance> {
        Ec2Client::launch_instance(self, config).await
    }

    async fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> Result<Option<String>> {
        Ec2Client::wait_for_running(self, instance_id, timeout_secs).await
    }

    async fn terminate_instance(&self, instance_id: &str) -> Result<()> {
        Ec2Client::terminate_instance(self, instance_id).await
    }

    async fn wait_for_terminated(&self, instance_id: &str) -> Result<()> {
        Ec2Client::wait_for_terminated(self, instance_id).await
    }

    async fn get_console_output(&self, instance_id: &str) -> Result<Option<String>> {
        Ec2Client::get_console_output(self, instance_id).await
    }

    async fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<String>,
    ) -> Result<String> {
        Ec2Client::create_security_group(self, run_id, coordinator_cidr, vpc_id.as_deref()).await
    }

    async fn delete_security_group(&self, security_group_id: &str) -> Result<()> {
        Ec2Client::delete_security_group(self, security_group_id).await
    }

    async fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        Ec2Client::add_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }

    async fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> Result<()> {
        Ec2Client::remove_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }
}
