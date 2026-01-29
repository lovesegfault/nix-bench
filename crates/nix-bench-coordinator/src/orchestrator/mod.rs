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
pub use cleanup::{CleanupRequest, cleanup_executor, cleanup_resources_no_tui};
pub use init::BenchmarkInitializer;
pub use progress::{InstanceUpdate, Reporter};
pub use types::{InstanceState, InstanceStatus};

use crate::config::RunConfig;
use crate::tui::LogCapture;
use anyhow::Result;
use nix_bench_common::defaults::DEFAULT_GRPC_PORT;
use nix_bench_common::{Architecture, RunId};
use tracing::info;

/// gRPC port for agent communication (from common defaults)
pub(crate) const GRPC_PORT: u16 = DEFAULT_GRPC_PORT;

/// Run benchmarks on the specified instances
pub async fn run_benchmarks(config: RunConfig, log_capture: Option<LogCapture>) -> Result<()> {
    // Determine which architectures we need
    let needs_x86_64 = config
        .instances
        .instance_types
        .iter()
        .any(|t| Architecture::from_instance_type(t) == Architecture::X86_64);
    let needs_aarch64 = config
        .instances
        .instance_types
        .iter()
        .any(|t| Architecture::from_instance_type(t) == Architecture::Aarch64);

    // Try to auto-detect agent binaries if not provided
    let agent_x86_64 = config.instances.agent_x86_64.clone().or_else(|| {
        let found = user_data::find_agent_binary("x86_64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected x86_64 agent");
        }
        found
    });

    let agent_aarch64 = config.instances.agent_aarch64.clone().or_else(|| {
        let found = user_data::find_agent_binary("aarch64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected aarch64 agent");
        }
        found
    });

    // Validate agent binaries are provided (unless dry-run)
    if !config.flags.dry_run {
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
    if config.flags.dry_run {
        println!("\n=== DRY RUN ===\n");
        println!("This would launch the following benchmark:\n");
        println!("  Region:         {}", config.aws.region);
        println!("  Attribute:      {}", config.benchmark.attr);
        println!("  Runs/instance:  {}", config.benchmark.runs);
        println!();
        println!("  Instance types:");
        for instance_type in &config.instances.instance_types {
            let system = Architecture::from_instance_type(instance_type);
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
        println!("    - Keep instances:   {}", config.flags.keep);
        println!("    - TUI mode:         {}", !config.flags.no_tui);
        println!(
            "    - GC between runs:  {}",
            config.benchmark.gc_between_runs
        );
        if let Some(output) = &config.flags.output {
            println!("    - Output file:    {}", output);
        }
        if let Some(subnet) = &config.aws.subnet_id {
            println!("    - Subnet ID:      {}", subnet);
        }
        if let Some(sg) = &config.aws.security_group_id {
            println!("    - Security group: {}", sg);
        }
        if let Some(profile) = &config.aws.instance_profile {
            println!("    - IAM profile:    {}", profile);
        }
        println!();
        println!("To run for real, remove the --dry-run flag.");
        return Ok(());
    }

    // Generate run ID (UUIDv7)
    let run_id = RunId::new();
    let bucket_name = format!("nix-bench-{}", run_id);

    // For TUI mode, start TUI immediately and run init in background
    if !config.flags.no_tui {
        benchmark::run_benchmarks_with_tui(
            config,
            run_id,
            bucket_name,
            agent_x86_64,
            agent_aarch64,
            log_capture,
        )
        .await
    } else {
        benchmark::run_benchmarks_no_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64)
            .await
    }
}
