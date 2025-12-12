//! nix-bench-agent: Benchmark agent that runs on EC2 instances
//!
//! This agent is deployed to EC2 instances via user-data script.
//! It runs nix-bench benchmarks and reports progress to CloudWatch.

mod benchmark;
mod config;
mod logs;
mod metrics;
mod nvme;
mod results;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::time::Instant;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(name = "nix-bench-agent")]
#[command(about = "Benchmark agent for EC2 instances")]
struct Args {
    /// Path to config JSON file
    #[arg(short, long, default_value = "/etc/nix-bench/config.json")]
    config: PathBuf,
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
    info!("Starting nix-bench-agent");

    // Load configuration
    let config = config::Config::load(&args.config)?;
    info!(
        run_id = %config.run_id,
        instance_type = %config.instance_type,
        attr = %config.attr,
        runs = config.runs,
        "Loaded configuration"
    );

    // Setup NVMe if available
    if let Some(devices) = nvme::detect_instance_store()? {
        info!(count = devices.len(), "Detected NVMe instance store devices");
        nvme::setup_raid_and_mount(&devices)?;
        info!("NVMe instance store configured");
    } else {
        info!("No NVMe instance store detected, using root volume");
    }

    // Initialize AWS clients
    let cloudwatch = metrics::CloudWatchClient::new(&config).await?;
    let s3 = results::S3Client::new(&config).await?;
    let logs_client = std::sync::Arc::new(logs::LogsClient::new(&config).await?);

    // Signal that we're running
    cloudwatch.put_status(metrics::Status::Running).await?;
    logs_client.write_line(&format!("Starting benchmark: {} runs of {}", config.runs, config.attr)).await?;

    let mut run_results = Vec::new();

    for run in 1..=config.runs {
        info!(run, total = config.runs, "Starting benchmark run");
        cloudwatch.put_progress(run).await?;
        logs_client.write_line(&format!("=== Run {}/{} ===", run, config.runs)).await?;

        // Clean nix store for consistent baseline
        if let Err(e) = benchmark::nix_collect_garbage() {
            error!(?e, "Failed to collect garbage, continuing anyway");
            logs_client.write_line(&format!("Warning: garbage collection failed: {}", e)).await?;
        }

        // Run benchmark with timing and streaming output
        let start = Instant::now();
        let logging_process = logs::LoggingProcess::new(logs_client.clone());

        match benchmark::run_nix_build_with_logging(&config.attr, &logging_process).await {
            Ok(()) => {
                let duration = start.elapsed();
                info!(run, duration_secs = duration.as_secs_f64(), "Run completed");
                logs_client.write_line(&format!("Run {} completed in {:.1}s", run, duration.as_secs_f64())).await?;

                run_results.push(results::RunResult {
                    run,
                    duration_secs: duration.as_secs_f64(),
                    success: true,
                });

                cloudwatch.put_duration(duration.as_secs_f64()).await?;
            }
            Err(e) => {
                error!(?e, run, "Build failed");
                logs_client.write_line(&format!("Run {} FAILED: {}", run, e)).await?;
                cloudwatch.put_status(metrics::Status::Failed).await?;
                return Err(e);
            }
        }
    }

    // Upload final results
    info!("Uploading results to S3");
    s3.upload_results(&run_results).await?;

    // Signal completion
    cloudwatch.put_status(metrics::Status::Complete).await?;
    info!("Benchmark complete");

    Ok(())
}
