//! Main orchestration logic for benchmark runs
//!
//! This module contains the orchestrator for running benchmarks on EC2 instances.
//! It handles both TUI and non-TUI modes, managing instance lifecycle, log streaming,
//! and result collection.

// Submodules
mod benchmark;
mod cleanup;
mod monitoring;
mod results;
mod user_data;

pub mod init;
pub mod progress;
pub mod types;

// Re-export core types
pub use init::{BenchmarkInitializer, InitContext};
pub use progress::{ChannelReporter, InitProgressReporter, InstanceUpdate, LogReporter};
pub use types::{InstanceState, InstanceStatus};

use crate::config::{detect_system, RunConfig};
use anyhow::Result;
use tracing::info;
use uuid::Uuid;

/// Default gRPC port for agent communication
pub(crate) const GRPC_PORT: u16 = 50051;

/// Run benchmarks on the specified instances
pub async fn run_benchmarks(config: RunConfig) -> Result<()> {
    // Determine which architectures we need
    let needs_x86_64 = config
        .instance_types
        .iter()
        .any(|t| detect_system(t) == "x86_64-linux");
    let needs_aarch64 = config
        .instance_types
        .iter()
        .any(|t| detect_system(t) == "aarch64-linux");

    // Try to auto-detect agent binaries if not provided
    let agent_x86_64 = config.agent_x86_64.clone().or_else(|| {
        let found = user_data::find_agent_binary("x86_64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected x86_64 agent");
        }
        found
    });

    let agent_aarch64 = config.agent_aarch64.clone().or_else(|| {
        let found = user_data::find_agent_binary("aarch64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected aarch64 agent");
        }
        found
    });

    // Validate agent binaries are provided (unless dry-run)
    if !config.dry_run {
        if needs_x86_64 && agent_x86_64.is_none() {
            anyhow::bail!(
                "x86_64 instance types specified but agent not found.\n\
                 Build with: cargo agent\n\
                 Or use nix: nix build .#nix-bench-agent"
            );
        }
        if needs_aarch64 && agent_aarch64.is_none() {
            anyhow::bail!(
                "aarch64 instance types specified but agent not found.\n\
                 Cross-compile with: nix build .#nix-bench-agent-aarch64"
            );
        }
    }

    // Dry-run mode: validate and print what would happen
    if config.dry_run {
        println!("\n=== DRY RUN ===\n");
        println!("This would launch the following benchmark:\n");
        println!("  Region:         {}", config.region);
        println!("  Attribute:      {}", config.attr);
        println!("  Runs/instance:  {}", config.runs);
        println!();
        println!("  Instance types:");
        for instance_type in &config.instance_types {
            let system = detect_system(instance_type);
            println!("    - {} ({})", instance_type, system);
        }
        println!();
        println!("  Agent binaries:");
        if needs_x86_64 {
            if let Some(path) = &agent_x86_64 {
                println!("    - x86_64:  {}", path);
            } else {
                println!("    - x86_64:  NOT PROVIDED (required)");
            }
        }
        if needs_aarch64 {
            if let Some(path) = &agent_aarch64 {
                println!("    - aarch64: {}", path);
            } else {
                println!("    - aarch64: NOT PROVIDED (required)");
            }
        }
        println!();
        println!("  Options:");
        println!("    - Keep instances:   {}", config.keep);
        println!("    - TUI mode:         {}", !config.no_tui);
        println!("    - GC between runs:  {}", config.gc_between_runs);
        if let Some(output) = &config.output {
            println!("    - Output file:    {}", output);
        }
        if let Some(subnet) = &config.subnet_id {
            println!("    - Subnet ID:      {}", subnet);
        }
        if let Some(sg) = &config.security_group_id {
            println!("    - Security group: {}", sg);
        }
        if let Some(profile) = &config.instance_profile {
            println!("    - IAM profile:    {}", profile);
        }
        println!();
        println!("To run for real, remove the --dry-run flag.");
        return Ok(());
    }

    // Generate run ID
    let run_id = Uuid::now_v7().to_string();
    let bucket_name = format!("nix-bench-{}", run_id);

    // For TUI mode, start TUI immediately and run init in background
    if !config.no_tui {
        benchmark::run_benchmarks_with_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64).await
    } else {
        benchmark::run_benchmarks_no_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64).await
    }
}

#[cfg(test)]
mod tests {
    use super::user_data::detect_bootstrap_failure;
    use crate::aws::{Ec2Operations, LaunchedInstance, MockEc2Operations, MockS3Operations, S3Operations};
    use mockall::predicate::eq;

    #[tokio::test]
    async fn test_mock_ec2_terminate_is_called() {
        let mut mock = MockEc2Operations::new();

        // Expect terminate to be called with specific instance ID
        mock.expect_terminate_instance()
            .with(eq("i-1234567890abcdef0"))
            .times(1)
            .returning(|_| Ok(()));

        // Call the mock
        let result = mock.terminate_instance("i-1234567890abcdef0").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_ec2_launch_instance() {
        let mut mock = MockEc2Operations::new();

        mock.expect_launch_instance()
            .returning(|config| {
                Ok(LaunchedInstance {
                    instance_id: format!("i-{}", config.run_id),
                    instance_type: config.instance_type,
                    system: config.system,
                    public_ip: None,
                })
            });

        let config = crate::aws::ec2::LaunchInstanceConfig::new(
            "test-run",
            "c6i.xlarge",
            "x86_64-linux",
            "#!/bin/bash\necho test",
        );
        let result = mock.launch_instance(config).await;

        assert!(result.is_ok());
        let instance = result.unwrap();
        assert_eq!(instance.instance_id, "i-test-run");
        assert_eq!(instance.instance_type, "c6i.xlarge");
    }

    #[tokio::test]
    async fn test_mock_ec2_handles_launch_failure() {
        let mut mock = MockEc2Operations::new();

        mock.expect_launch_instance()
            .returning(|_| Err(anyhow::anyhow!("InsufficientInstanceCapacity")));

        let config = crate::aws::ec2::LaunchInstanceConfig::new(
            "test-run",
            "c6i.xlarge",
            "x86_64-linux",
            "#!/bin/bash",
        );
        let result = mock.launch_instance(config).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("InsufficientInstanceCapacity"));
    }

    #[tokio::test]
    async fn test_mock_s3_delete_bucket() {
        let mut mock = MockS3Operations::new();

        mock.expect_delete_bucket()
            .with(eq("nix-bench-test-bucket"))
            .times(1)
            .returning(|_| Ok(()));

        let result = mock.delete_bucket("nix-bench-test-bucket").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_mock_ec2_console_output_with_failure() {
        let mut mock = MockEc2Operations::new();

        mock.expect_get_console_output()
            .returning(|_| Ok(Some("nix-bench-agent: command not found".to_string())));

        let result = mock.get_console_output("i-failing").await;
        assert!(result.is_ok());
        let output = result.unwrap().unwrap();

        // Verify that detect_bootstrap_failure would catch this
        let failure = detect_bootstrap_failure(&output);
        assert!(failure.is_some());
    }

    #[tokio::test]
    async fn test_mock_ec2_wait_for_running() {
        let mut mock = MockEc2Operations::new();

        mock.expect_wait_for_running()
            .with(eq("i-test"), eq(Some(300u64)))
            .returning(|_, _| Ok(Some("1.2.3.4".to_string())));

        let result = mock.wait_for_running("i-test", Some(300)).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("1.2.3.4".to_string()));
    }

    #[tokio::test]
    async fn test_mock_ec2_security_group_lifecycle() {
        let mut mock = MockEc2Operations::new();

        // Create security group
        mock.expect_create_security_group()
            .returning(|_, _, _| Ok("sg-12345".to_string()));

        // Add ingress rule
        mock.expect_add_grpc_ingress_rule()
            .with(eq("sg-12345"), eq("10.0.0.1/32"))
            .returning(|_, _| Ok(()));

        // Remove ingress rule
        mock.expect_remove_grpc_ingress_rule()
            .with(eq("sg-12345"), eq("10.0.0.1/32"))
            .returning(|_, _| Ok(()));

        // Delete security group
        mock.expect_delete_security_group()
            .with(eq("sg-12345"))
            .returning(|_| Ok(()));

        // Execute the lifecycle
        let sg_id = mock.create_security_group("run-123", "10.0.0.1/32", None).await.unwrap();
        assert_eq!(sg_id, "sg-12345");

        mock.add_grpc_ingress_rule(&sg_id, "10.0.0.1/32").await.unwrap();
        mock.remove_grpc_ingress_rule(&sg_id, "10.0.0.1/32").await.unwrap();
        mock.delete_security_group(&sg_id).await.unwrap();
    }
}
