//! nix-bench-coordinator: EC2 instance benchmarking coordinator with TUI dashboard
//!
//! This tool launches EC2 instances to run nix-bench benchmarks and provides
//! a real-time TUI dashboard showing progress.

use anyhow::Result;
use chrono::Duration;
use clap::{Parser, Subcommand};
use nix_bench_common::defaults::{DEFAULT_BUILD_TIMEOUT, DEFAULT_FLAKE_REF, DEFAULT_MAX_FAILURES};
use nix_bench_coordinator::aws::FromAwsContext;
use nix_bench_coordinator::aws::cleanup::{CleanupConfig, TagBasedCleanup};
use nix_bench_coordinator::aws::scanner::{ResourceScanner, ScanConfig};
use nix_bench_coordinator::tui::{LogCapture, LogCaptureLayer};
use nix_bench_coordinator::{config, orchestrator};
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "nix-bench-coordinator")]
#[command(about = "EC2 instance benchmarking for Nix builds")]
#[command(version)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

/// Arguments for the run command (extracted to reduce enum size)
#[derive(clap::Args, Debug)]
struct RunArgs {
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
    #[arg(long, default_value = DEFAULT_FLAKE_REF)]
    flake_ref: String,

    /// Build timeout in seconds per run
    #[arg(long, default_value_t = DEFAULT_BUILD_TIMEOUT)]
    build_timeout: u64,

    /// Maximum number of build failures before giving up
    #[arg(long, default_value_t = DEFAULT_MAX_FAILURES)]
    max_failures: u32,

    /// Run garbage collection between benchmark runs
    /// Preserves fixed-output derivations (fetched sources) but removes build outputs
    #[arg(long)]
    gc_between_runs: bool,
}

impl RunArgs {
    /// Parse instance types from the comma-separated string
    fn parse_instance_types(&self) -> Vec<String> {
        self.instances
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

impl From<RunArgs> for config::RunConfig {
    fn from(args: RunArgs) -> Self {
        let instance_types = args.parse_instance_types();
        Self {
            benchmark: config::BenchmarkConfig {
                attr: args.attr,
                runs: args.runs,
                flake_ref: args.flake_ref,
                build_timeout: args.build_timeout,
                max_failures: args.max_failures,
                gc_between_runs: args.gc_between_runs,
            },
            aws: config::AwsConfig {
                region: args.region,
                aws_profile: args.aws_profile,
                subnet_id: args.subnet_id,
                security_group_id: args.security_group_id,
                instance_profile: args.instance_profile,
            },
            instances: config::InstanceConfig {
                instance_types,
                agent_x86_64: args.agent_x86_64,
                agent_aarch64: args.agent_aarch64,
            },
            flags: config::RuntimeFlags {
                keep: args.keep,
                timeout: args.timeout,
                no_tui: args.no_tui,
                dry_run: args.dry_run,
                output: args.output,
            },
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run benchmarks on EC2 instances
    Run(Box<RunArgs>),

    /// Scan AWS for nix-bench resources using tags
    Scan {
        /// AWS region to scan
        #[arg(long, default_value = "us-east-2")]
        region: String,

        /// Only show resources older than N hours
        #[arg(long, default_value = "1")]
        min_age_hours: u64,

        /// Only show resources from a specific run ID
        #[arg(long)]
        run_id: Option<String>,

        /// Output format (table, json)
        #[arg(long, default_value = "table")]
        format: String,
    },

    /// Clean up orphaned AWS resources using tag-based discovery
    CleanupOrphans {
        /// AWS region to clean
        #[arg(long, default_value = "us-east-2")]
        region: String,

        /// Minimum age in hours before considering a resource orphaned
        #[arg(long, default_value = "1")]
        min_age_hours: u64,

        /// Only clean up resources from a specific run ID
        #[arg(long)]
        run_id: Option<String>,

        /// Actually delete resources (default is dry-run)
        #[arg(long)]
        execute: bool,

        /// Force delete even resources in "creating" status
        #[arg(long)]
        force: bool,
    },
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
    nix_bench_common::init_rustls();

    // Filter out bare "--" args that cargo-make may pass
    let filtered_args: Vec<String> = std::env::args().filter(|a| a != "--").collect();
    let args = Args::parse_from(filtered_args);

    // Check if we're in TUI mode (run command without --no-tui)
    let use_tui = matches!(&args.command, Command::Run(run_args) if !run_args.no_tui);

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
        Command::Run(run_args) => {
            if let Some(profile) = &run_args.aws_profile {
                info!(profile = %profile, "Using AWS profile");
            }

            // Validate instance types before launching TUI
            let instance_types = run_args.parse_instance_types();
            {
                use nix_bench_coordinator::aws::{Ec2Client, FromAwsContext, context::AwsContext};
                let aws =
                    AwsContext::with_profile(&run_args.region, run_args.aws_profile.as_deref())
                        .await;
                let ec2 = Ec2Client::from_context(&aws);
                ec2.validate_instance_types(&instance_types).await?;
            }

            info!(
                instances = ?instance_types,
                attr = %run_args.attr,
                runs = run_args.runs,
                region = %run_args.region,
                flake_ref = %run_args.flake_ref,
                build_timeout = run_args.build_timeout,
                max_failures = run_args.max_failures,
                "Starting benchmark run"
            );

            let config: config::RunConfig = (*run_args).into();
            orchestrator::run_benchmarks(config, log_capture).await?;
        }

        Command::Scan {
            region,
            min_age_hours,
            run_id,
            format,
        } => {
            handle_scan(region, min_age_hours, run_id, format).await?;
        }

        Command::CleanupOrphans {
            region,
            min_age_hours,
            run_id,
            execute,
            force,
        } => {
            handle_cleanup_orphans(region, min_age_hours, run_id, execute, force).await?;
        }
    }

    Ok(())
}

/// Handle the scan command
async fn handle_scan(
    region: String,
    min_age_hours: u64,
    run_id: Option<String>,
    format: String,
) -> Result<()> {
    info!(region = %region, min_age_hours, run_id = ?run_id, "Scanning for nix-bench resources");

    let scanner = ResourceScanner::new(&region).await?;
    let config = ScanConfig {
        min_age: Duration::hours(min_age_hours as i64),
        run_id,
        include_creating: false,
        ..Default::default()
    };

    let resources = scanner.scan_all(&config).await?;

    if resources.is_empty() {
        println!("No nix-bench resources found matching criteria.");
        return Ok(());
    }

    if format == "json" {
        let json_resources: Vec<_> = resources
            .iter()
            .map(|r| {
                serde_json::json!({
                    "type": format!("{:?}", r.resource_type),
                    "id": r.resource_id,
                    "region": r.region,
                    "run_id": r.run_id,
                    "created_at": r.created_at.to_rfc3339(),
                    "status": r.status,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_resources)?);
    } else {
        println!(
            "{:<15} {:<25} {:<15} {:<20} {:<10}",
            "TYPE", "ID", "RUN_ID", "CREATED_AT", "STATUS"
        );
        println!("{}", "-".repeat(85));
        for r in &resources {
            println!(
                "{:<15} {:<25} {:<15} {:<20} {:<10}",
                format!("{:?}", r.resource_type),
                if r.resource_id.len() > 24 {
                    format!("{}...", &r.resource_id[..21])
                } else {
                    r.resource_id.clone()
                },
                if r.run_id.len() > 14 {
                    format!("{}...", &r.run_id[..11])
                } else {
                    r.run_id.clone()
                },
                r.created_at.format("%Y-%m-%d %H:%M:%S"),
                r.status,
            );
        }
        println!("\nTotal: {} resources", resources.len());
    }

    Ok(())
}

/// Handle the cleanup-orphans command
async fn handle_cleanup_orphans(
    region: String,
    min_age_hours: u64,
    run_id: Option<String>,
    execute: bool,
    force: bool,
) -> Result<()> {
    let mode = if execute { "EXECUTE" } else { "DRY-RUN" };
    info!(
        region = %region,
        min_age_hours,
        run_id = ?run_id,
        mode,
        force,
        "Cleaning up orphaned resources"
    );

    let cleanup = TagBasedCleanup::new(&region).await?;
    let config = CleanupConfig {
        min_age: Duration::hours(min_age_hours as i64),
        run_id,
        dry_run: !execute,
        force,
    };

    let report = cleanup.cleanup(&config).await?;

    println!("\n=== Cleanup Report ===");
    println!("Mode: {}", mode);
    println!("Region: {}", region);
    println!();
    println!("Resources found: {}", report.total_found);
    println!("  EC2 Instances:    {}", report.ec2_instances);
    println!("  Security Groups:  {}", report.security_groups);
    println!("  IAM Roles:        {}", report.iam_roles);
    println!("  S3 Buckets:       {}", report.s3_buckets);
    println!();
    if execute {
        println!("Deleted: {}", report.deleted);
        println!("Failed:  {}", report.failed);
    } else {
        println!("Skipped: {} (dry-run mode)", report.skipped);
        println!();
        println!("Run with --execute to actually delete resources.");
    }

    Ok(())
}
