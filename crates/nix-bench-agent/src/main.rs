//! nix-bench-agent: Benchmark agent that runs on EC2 instances
//!
//! This agent is deployed to EC2 instances via user-data script.
//! It runs nix-bench benchmarks and reports progress to CloudWatch.

use anyhow::Result;
use clap::Parser;
use nix_bench_agent::{benchmark, config, grpc, logs, metrics, results};
use nix_bench_common::metrics::Status;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

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
    broadcaster: &Arc<grpc::LogBroadcaster>,
    status: &Arc<RwLock<grpc::AgentStatus>>,
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

        // Update gRPC status
        {
            let mut s = status.write().await;
            s.run_progress = slot as u32;
            s.total_runs = config.runs;
            s.status = "running".to_string();
        }

        logs_client
            .write_line(&format!(
                "=== Run {} (slot {}/{}, {} failures so far) ===",
                current_run, slot, config.runs, failure_count
            ))
            .await?;

        // Run benchmark with timing and streaming output
        let start = Instant::now();
        let logging_process =
            logs::LoggingProcess::new_with_grpc(logs_client.clone(), broadcaster.clone());

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

                // Update gRPC status with new duration
                {
                    let mut s = status.write().await;
                    s.durations.push(duration.as_secs_f64());
                }

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
    // Install rustls crypto provider before any TLS operations
    // Required for rustls 0.23+ when using tonic with TLS
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("Failed to install rustls crypto provider");

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
        grpc_port = config.grpc_port,
        "Loaded configuration"
    );

    // Initialize AWS clients
    // Note: NVMe instance store setup is handled in user-data before agent starts
    let cloudwatch = metrics::CloudWatchClient::new(&config).await?;
    let s3 = results::S3Client::new(&config).await?;
    let logs_client = Arc::new(logs::LogsClient::new(&config).await?);

    // Create gRPC broadcaster and status (use configurable capacity)
    let broadcaster = Arc::new(grpc::LogBroadcaster::new(config.broadcast_capacity));
    let status = Arc::new(RwLock::new(grpc::AgentStatus {
        status: "starting".to_string(),
        run_progress: 0,
        total_runs: config.runs,
        durations: Vec::new(),
    }));

    // Connect broadcaster to logs client so ALL log lines are streamed to gRPC clients
    logs_client.set_broadcaster(broadcaster.clone()).await;

    // Create shutdown token for graceful gRPC server shutdown
    let shutdown_token = CancellationToken::new();

    // Spawn gRPC server (port 50051) with TLS if configured
    let config_arc = Arc::new(config.clone());
    let grpc_broadcaster = broadcaster.clone();
    let grpc_status = status.clone();
    let grpc_shutdown = shutdown_token.clone();
    let tls_config = config.tls_config();

    if tls_config.is_some() {
        info!("gRPC server will use mTLS");
    } else {
        warn!("gRPC server running WITHOUT TLS - this is insecure!");
    }

    let grpc_port = config.grpc_port;
    let grpc_handle = tokio::spawn(async move {
        if let Err(e) = grpc::run_grpc_server(
            grpc_port,
            grpc_broadcaster,
            config_arc,
            grpc_status,
            tls_config,
            grpc_shutdown,
        )
        .await
        {
            error!(error = %e, "gRPC server error");
        }
    });

    // Signal that we're running
    cloudwatch.put_status(Status::Running).await?;
    logs_client
        .write_line(&format!(
            "Starting benchmark: {} runs of {} (from {}), timeout {}s, max {} failures",
            config.runs, config.attr, config.flake_ref, config.build_timeout, config.max_failures
        ))
        .await?;

    // === Cache warmup phase ===
    // Run a throwaway build to warm the Nix cache before timed runs
    info!("Starting cache warmup build (not timed)");
    logs_client
        .write_line("=== Cache Warmup (not timed) ===")
        .await?;
    logs_client
        .write_line("Running throwaway build to warm Nix cache...")
        .await?;

    // Update status to show warmup
    {
        let mut s = status.write().await;
        s.status = "warmup".to_string();
    }

    // Run warmup build (same command as benchmark, just not timed)
    let warmup_logging =
        logs::LoggingProcess::new_with_grpc(logs_client.clone(), broadcaster.clone());
    match benchmark::run_nix_build_with_logging(
        &config.flake_ref,
        &config.attr,
        &warmup_logging,
        Some(config.build_timeout),
    )
    .await
    {
        Ok(()) => {
            info!("Cache warmup complete");
            logs_client
                .write_line("Cache warmup complete, starting timed runs...")
                .await?;
        }
        Err(e) => {
            // Warmup failure is not fatal - log and continue
            warn!(error = %e, "Cache warmup build failed, continuing with timed runs");
            logs_client
                .write_line(&format!(
                    "Warning: warmup build failed: {}. Continuing with timed runs...",
                    e
                ))
                .await?;
        }
    }

    // Run benchmarks with retry logic
    let run_results =
        run_benchmarks_with_retry(&config, &cloudwatch, &logs_client, &broadcaster, &status)
            .await?;

    // Upload final results
    info!("Uploading results to S3");
    s3.upload_results(&run_results).await?;

    // Signal completion
    cloudwatch.put_status(Status::Complete).await?;

    // Update gRPC status
    {
        let mut s = status.write().await;
        s.status = "complete".to_string();
    }

    logs_client
        .write_line(&format!(
            "Benchmark complete: {} successful runs",
            run_results.len()
        ))
        .await?;
    info!("Benchmark complete");

    // Gracefully shut down gRPC server
    info!("Shutting down gRPC server...");
    shutdown_token.cancel();

    // Wait for gRPC server to shut down (with timeout)
    match tokio::time::timeout(std::time::Duration::from_secs(5), grpc_handle).await {
        Ok(Ok(())) => info!("gRPC server shut down cleanly"),
        Ok(Err(e)) => {
            if e.is_panic() {
                error!("gRPC server task panicked: {:?}", e);
            } else {
                warn!("gRPC server task was cancelled");
            }
        }
        Err(_) => warn!("Timed out waiting for gRPC server shutdown"),
    }

    Ok(())
}
