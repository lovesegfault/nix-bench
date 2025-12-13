//! nix-bench-agent: Benchmark agent that runs on EC2 instances
//!
//! This agent is deployed to EC2 instances via user-data script.
//! It runs nix-bench benchmarks and reports progress to CloudWatch.

use anyhow::Result;
use clap::Parser;
use nix_bench::agent::{benchmark, config, logs, metrics, nvme, results};
use nix_bench::metrics::Status;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "nix-bench-agent")]
#[command(about = "Benchmark agent for EC2 instances")]
struct Args {
    /// Path to config JSON file
    #[arg(short, long, default_value = "/etc/nix-bench/config.json")]
    config: PathBuf,
}

/// Run benchmarks with smart retry logic.
///
/// On failure, schedules a replacement run. Gives up after max_failures total failures.
async fn run_benchmarks_with_retry(
    config: &config::Config,
    cloudwatch: &metrics::CloudWatchClient,
    logs_client: &Arc<logs::LogsClient>,
) -> Result<Vec<results::RunResult>> {
    let mut successful_runs: Vec<results::RunResult> = Vec::new();
    let mut failure_count: u32 = 0;
    let mut current_run: u32 = 1;

    while successful_runs.len() < config.runs as usize {
        // Check if we've exceeded max failures
        if failure_count >= config.max_failures {
            anyhow::bail!(
                "Exceeded maximum failures ({}). Completed {}/{} runs successfully.",
                config.max_failures,
                successful_runs.len(),
                config.runs
            );
        }

        let slot = successful_runs.len() + 1;
        info!(
            run = current_run,
            slot,
            total_slots = config.runs,
            failures = failure_count,
            max_failures = config.max_failures,
            "Starting benchmark run"
        );

        // Report progress (slot number, not run number)
        cloudwatch.put_progress(slot as u32).await?;
        logs_client
            .write_line(&format!(
                "=== Run {} (slot {}/{}, {} failures so far) ===",
                current_run, slot, config.runs, failure_count
            ))
            .await?;

        // Clean nix store for consistent baseline
        if let Err(e) = benchmark::nix_collect_garbage() {
            warn!(?e, "Garbage collection failed, continuing anyway");
            logs_client
                .write_line(&format!("Warning: garbage collection failed: {}", e))
                .await?;
        }

        // Run benchmark with timing and streaming output
        let start = Instant::now();
        let logging_process = logs::LoggingProcess::new(logs_client.clone());

        match benchmark::run_nix_build_with_logging(
            &config.flake_ref,
            &config.attr,
            &logging_process,
            Some(config.build_timeout),
        )
        .await
        {
            Ok(()) => {
                let duration = start.elapsed();
                info!(
                    run = current_run,
                    slot,
                    duration_secs = duration.as_secs_f64(),
                    "Run completed successfully"
                );
                logs_client
                    .write_line(&format!(
                        "Run {} completed in {:.1}s",
                        current_run,
                        duration.as_secs_f64()
                    ))
                    .await?;

                successful_runs.push(results::RunResult {
                    run: slot as u32,
                    duration_secs: duration.as_secs_f64(),
                    success: true,
                });

                cloudwatch.put_duration(duration.as_secs_f64()).await?;
            }
            Err(e) => {
                failure_count += 1;
                let remaining_attempts = config.max_failures - failure_count;
                warn!(
                    run = current_run,
                    failure_count,
                    remaining_attempts,
                    error = %e,
                    "Build failed, scheduling replacement run"
                );

                logs_client
                    .write_line(&format!(
                        "Run {} FAILED ({}/{} failures): {}. {} attempts remaining.",
                        current_run, failure_count, config.max_failures, e, remaining_attempts
                    ))
                    .await?;

                // Report the failure as a metric (but don't mark overall status as failed yet)
                if let Err(metric_err) = cloudwatch.put_metric("FailedRuns", 1.0).await {
                    warn!(error = %metric_err, "Failed to report failure metric");
                }
            }
        }

        current_run += 1;
    }

    info!(
        successful = successful_runs.len(),
        total_runs = current_run - 1,
        failures = failure_count,
        "Benchmark runs complete"
    );

    Ok(successful_runs)
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
        flake_ref = %config.flake_ref,
        runs = config.runs,
        build_timeout = config.build_timeout,
        max_failures = config.max_failures,
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
    let logs_client = Arc::new(logs::LogsClient::new(&config).await?);

    // Signal that we're running
    cloudwatch.put_status(Status::Running).await?;
    logs_client
        .write_line(&format!(
            "Starting benchmark: {} runs of {} (from {}), timeout {}s, max {} failures",
            config.runs, config.attr, config.flake_ref, config.build_timeout, config.max_failures
        ))
        .await?;

    // Run benchmarks with retry logic
    let run_results = run_benchmarks_with_retry(&config, &cloudwatch, &logs_client).await?;

    // Upload final results
    info!("Uploading results to S3");
    s3.upload_results(&run_results).await?;

    // Signal completion
    cloudwatch.put_status(Status::Complete).await?;
    logs_client
        .write_line(&format!(
            "Benchmark complete: {} successful runs",
            run_results.len()
        ))
        .await?;
    info!("Benchmark complete");

    Ok(())
}
