//! nix-bench-ec2: EC2 instance benchmarking coordinator with TUI dashboard
//!
//! This tool launches EC2 instances to run nix-bench benchmarks and provides
//! a real-time TUI dashboard showing progress.

mod aws;
mod config;
mod orchestrator;
mod state;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "nix-bench-ec2")]
#[command(about = "EC2 instance benchmarking for Nix builds")]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run benchmarks on EC2 instances
    Run {
        /// Comma-separated EC2 instance types to benchmark
        #[arg(short, long)]
        instances: String,

        /// nix-bench attribute to build (e.g., "large-deep")
        #[arg(short, long, default_value = "large-deep")]
        attr: String,

        /// Number of benchmark runs per instance
        #[arg(short, long, default_value = "10")]
        runs: u32,

        /// AWS region
        #[arg(long, default_value = "us-east-2")]
        region: String,

        /// Output JSON file for results
        #[arg(short, long)]
        output: Option<String>,

        /// Don't terminate instances after benchmark
        #[arg(long)]
        keep: bool,

        /// Per-run timeout in seconds
        #[arg(long, default_value = "7200")]
        timeout: u64,

        /// Disable TUI, print progress to stdout
        #[arg(long)]
        no_tui: bool,

        /// Path to pre-built agent binary for x86_64-linux
        #[arg(long)]
        agent_x86_64: Option<String>,

        /// Path to pre-built agent binary for aarch64-linux
        #[arg(long)]
        agent_aarch64: Option<String>,

        /// VPC subnet ID for launching instances (uses default VPC if not specified)
        #[arg(long)]
        subnet_id: Option<String>,

        /// Security group ID for instances
        #[arg(long)]
        security_group_id: Option<String>,

        /// IAM instance profile name for EC2 instances
        #[arg(long)]
        instance_profile: Option<String>,

        /// Validate configuration without launching instances
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage local state and AWS resources
    State {
        #[command(subcommand)]
        action: StateAction,
    },
}

#[derive(Subcommand, Debug)]
enum StateAction {
    /// Show all tracked resources
    List,

    /// Find orphaned resources and delete them
    Cleanup,

    /// Remove stale entries from local database
    Prune,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let args = Args::parse();

    match args.command {
        Command::Run {
            instances,
            attr,
            runs,
            region,
            output,
            keep,
            timeout,
            no_tui,
            agent_x86_64,
            agent_aarch64,
            subnet_id,
            security_group_id,
            instance_profile,
            dry_run,
        } => {
            let instance_types: Vec<String> =
                instances.split(',').map(|s| s.trim().to_string()).collect();

            info!(
                instances = ?instance_types,
                attr = %attr,
                runs,
                region = %region,
                "Starting benchmark run"
            );

            let config = config::RunConfig {
                instance_types,
                attr,
                runs,
                region,
                output,
                keep,
                timeout,
                no_tui,
                agent_x86_64,
                agent_aarch64,
                subnet_id,
                security_group_id,
                instance_profile,
                dry_run,
            };

            orchestrator::run_benchmarks(config).await?;
        }

        Command::State { action } => match action {
            StateAction::List => {
                state::list_resources().await?;
            }
            StateAction::Cleanup => {
                state::cleanup_resources().await?;
            }
            StateAction::Prune => {
                state::prune_database()?;
            }
        },
    }

    Ok(())
}
