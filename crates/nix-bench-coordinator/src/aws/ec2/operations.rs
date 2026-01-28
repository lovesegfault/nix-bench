//! EC2 operations trait for testing

use super::types::{LaunchInstanceConfig, LaunchedInstance};
use super::Ec2Client;
use anyhow::Result;
use std::future::Future;

/// Trait for EC2 operations that can be mocked in tests.
///
/// This trait abstracts the EC2 client operations to enable unit testing
/// of orchestration logic without hitting real AWS.
pub trait Ec2Operations: Send + Sync {
    /// Launch an EC2 instance with the given configuration
    fn launch_instance(
        &self,
        config: LaunchInstanceConfig,
    ) -> impl Future<Output = Result<LaunchedInstance>> + Send;

    /// Wait for instance to be running and get its public IP
    fn wait_for_running(
        &self,
        instance_id: &str,
        timeout_secs: Option<u64>,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Terminate an instance
    fn terminate_instance(&self, instance_id: &str) -> impl Future<Output = Result<()>> + Send;

    /// Wait for instance to be fully terminated
    fn wait_for_terminated(&self, instance_id: &str) -> impl Future<Output = Result<()>> + Send;

    /// Get console output from an instance
    fn get_console_output(
        &self,
        instance_id: &str,
    ) -> impl Future<Output = Result<Option<String>>> + Send;

    /// Create a security group
    fn create_security_group(
        &self,
        run_id: &str,
        coordinator_cidr: &str,
        vpc_id: Option<String>,
    ) -> impl Future<Output = Result<String>> + Send;

    /// Delete a security group
    fn delete_security_group(
        &self,
        security_group_id: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Add gRPC ingress rule to security group
    fn add_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Remove gRPC ingress rule from security group
    fn remove_grpc_ingress_rule(
        &self,
        security_group_id: &str,
        cidr_ip: &str,
    ) -> impl Future<Output = Result<()>> + Send;
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

    async fn add_grpc_ingress_rule(&self, security_group_id: &str, cidr_ip: &str) -> Result<()> {
        Ec2Client::add_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }

    async fn remove_grpc_ingress_rule(&self, security_group_id: &str, cidr_ip: &str) -> Result<()> {
        Ec2Client::remove_grpc_ingress_rule(self, security_group_id, cidr_ip).await
    }
}
