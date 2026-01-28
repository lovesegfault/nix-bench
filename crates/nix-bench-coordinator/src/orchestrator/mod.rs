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
pub use cleanup::{cleanup_executor, cleanup_resources_no_tui, CleanupRequest};
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
        benchmark::run_benchmarks_with_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64)
            .await
    } else {
        benchmark::run_benchmarks_no_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64)
            .await
    }
}
