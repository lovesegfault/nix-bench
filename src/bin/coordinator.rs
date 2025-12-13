//! nix-bench-coordinator: EC2 instance benchmarking coordinator with TUI dashboard
//!
//! This tool launches EC2 instances to run nix-bench benchmarks and provides
//! a real-time TUI dashboard showing progress.

use anyhow::Result;
use clap::{Parser, Subcommand};
use nix_bench::tui::{LogCapture, LogCaptureLayer};
use nix_bench::{config, orchestrator, state};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "nix-bench-coordinator")]
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

        /// AWS profile to use (overrides AWS_PROFILE env var)
        #[arg(long)]
        aws_profile: Option<String>,

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
        /// (default: $NIX_BENCH_AGENT_X86_64)
        #[arg(long, env = "NIX_BENCH_AGENT_X86_64")]
        agent_x86_64: Option<String>,

        /// Path to pre-built agent binary for aarch64-linux
        /// (default: $NIX_BENCH_AGENT_AARCH64)
        #[arg(long, env = "NIX_BENCH_AGENT_AARCH64")]
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

        /// Flake reference base (e.g., "github:lovesegfault/nix-bench")
        #[arg(long, default_value = config::DEFAULT_FLAKE_REF)]
        flake_ref: String,

        /// Build timeout in seconds per run
        #[arg(long, default_value_t = config::DEFAULT_BUILD_TIMEOUT)]
        build_timeout: u64,

        /// Maximum number of build failures before giving up
        #[arg(long, default_value_t = config::DEFAULT_MAX_FAILURES)]
        max_failures: u32,
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
async fn main() {
    if let Err(e) = run().await {
        print_error(&e);
        std::process::exit(1);
    }
}

/// Print error in a user-friendly way
fn print_error(e: &anyhow::Error) {
    use std::io::Write;

    let mut stderr = std::io::stderr();

    // Print main error message
    let _ = writeln!(stderr, "\n\x1b[1;31mError:\x1b[0m {e}");

    // Print error chain (causes)
    let mut source = e.source();
    while let Some(cause) = source {
        let _ = writeln!(stderr, "  \x1b[33mCaused by:\x1b[0m {cause}");
        source = cause.source();
    }

    // Only print backtrace hint if not already showing
    if std::env::var("RUST_BACKTRACE").is_err() {
        let _ = writeln!(
            stderr,
            "\n\x1b[2mSet RUST_BACKTRACE=1 for a detailed backtrace\x1b[0m"
        );
    } else {
        // Print backtrace if available and requested
        let backtrace = e.backtrace();
        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            let _ = writeln!(stderr, "\n\x1b[2mBacktrace:\x1b[0m\n{backtrace}");
        }
    }
}

async fn run() -> Result<()> {
    // Install rustls crypto provider before any TLS operations
    // Required for rustls 0.23+ when using tonic with TLS
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

    // Filter out bare "--" args that cargo-make may pass
    let filtered_args: Vec<String> = std::env::args().filter(|a| a != "--").collect();
    let args = Args::parse_from(filtered_args);

    // Check if we're in TUI mode (run command without --no-tui)
    let use_tui = matches!(&args.command, Command::Run { no_tui, .. } if !no_tui);

    // Create log capture for TUI mode (to print errors/warnings after exit)
    let log_capture = if use_tui {
        Some(LogCapture::new(50))
    } else {
        None
    };

    if use_tui {
        // Initialize tui-logger for TUI mode
        tui_logger::init_logger(log::LevelFilter::Info)?;
        tui_logger::set_default_level(log::LevelFilter::Info);

        // Configure explicit targets to show coordinator logs
        tui_logger::set_level_for_target("nix_bench", log::LevelFilter::Info);
        tui_logger::set_level_for_target("nix_bench::orchestrator", log::LevelFilter::Info);
        tui_logger::set_level_for_target("nix_bench::aws", log::LevelFilter::Info);

        // Reduce noise from AWS SDK (show only warnings and errors)
        tui_logger::set_level_for_target("aws_config", log::LevelFilter::Warn);
        tui_logger::set_level_for_target("aws_sdk", log::LevelFilter::Warn);
        tui_logger::set_level_for_target("aws_smithy", log::LevelFilter::Warn);

        // Set up tracing to route to tui-logger and log capture
        use tracing_subscriber::prelude::*;
        tracing_subscriber::registry()
            .with(tui_logger::TuiTracingSubscriberLayer)
            .with(LogCaptureLayer::new(log_capture.clone().unwrap()))
            .init();
    } else {
        // Standard tracing for non-TUI mode
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::from_default_env()
                    .add_directive(tracing::Level::INFO.into()),
            )
            .init();
    }

    match args.command {
        Command::Run {
            instances,
            attr,
            runs,
            region,
            aws_profile,
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
            flake_ref,
            build_timeout,
            max_failures,
        } => {
            // Set AWS profile before any AWS SDK calls
            if let Some(profile) = &aws_profile {
                std::env::set_var("AWS_PROFILE", profile);
                info!(profile = %profile, "Using AWS profile");
            }

            let instance_types: Vec<String> =
                instances.split(',').map(|s| s.trim().to_string()).collect();

            info!(
                instances = ?instance_types,
                attr = %attr,
                runs,
                region = %region,
                flake_ref = %flake_ref,
                build_timeout,
                max_failures,
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
                flake_ref,
                build_timeout,
                max_failures,
                log_capture,
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
                state::prune_database().await?;
            }
        },
    }

    Ok(())
}
