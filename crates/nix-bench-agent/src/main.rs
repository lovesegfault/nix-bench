//! nix-bench-agent: Benchmark agent that runs on EC2 instances
//!
//! This agent handles all host setup (NVMe, Nix installation) and runs benchmarks.
//! All output is streamed via gRPC to the coordinator TUI.

use anyhow::Result;
use clap::Parser;
use grpc::StatusCode;
use nix_bench_agent::{benchmark, bootstrap, gc, grpc, logging, results};
use nix_bench_common::RunResult;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Default broadcast channel capacity for gRPC log streaming
const DEFAULT_BROADCAST_CAPACITY: usize = 1024;

#[derive(Parser, Debug)]
#[command(name = "nix-bench-agent")]
#[command(about = "Benchmark agent for EC2 instances")]
struct Args {
    /// S3 bucket containing config and for results
    #[arg(long)]
    bucket: String,

    /// Unique run identifier
    #[arg(long)]
    run_id: String,

    /// EC2 instance type
    #[arg(long)]
    instance_type: String,

    /// gRPC server port
    #[arg(long, default_value = "50051")]
    grpc_port: u16,
}

/// Run benchmarks with smart retry logic.
///
/// On failure, schedules a replacement run. Gives up after max_failures total failures.
async fn run_benchmarks_with_retry(
    config: &nix_bench_agent::config::Config,
    logger: &logging::GrpcLogger,
    status: &Arc<RwLock<grpc::AgentStatus>>,
) -> Result<Vec<RunResult>> {
    let mut successful_runs: Vec<RunResult> = Vec::new();
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

        // Update gRPC status
        {
            let mut s = status.write().await;
            s.run_progress = slot as u32;
            s.total_runs = config.runs;
            s.status = StatusCode::Running;
        }

        logger.write_line(&format!(
            "=== Run {} (slot {}/{}, {} failures so far) ===",
            current_run, slot, config.runs, failure_count
        ));

        // Run benchmark with timing
        let start = Instant::now();

        match benchmark::run_nix_build(
            &config.flake_ref,
            &config.attr,
            logger,
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
                logger.write_line(&format!(
                    "Run {} completed in {:.1}s",
                    current_run,
                    duration.as_secs_f64()
                ));

                successful_runs.push(RunResult {
                    run_number: slot as u32,
                    duration_secs: duration.as_secs_f64(),
                    success: true,
                });

                // Update gRPC status with run result
                {
                    let mut s = status.write().await;
                    s.run_results.push(RunResult {
                        run_number: slot as u32,
                        duration_secs: duration.as_secs_f64(),
                        success: true,
                    });
                }

                // Run garbage collection between runs if enabled
                // Skip on last run since we're about to finish anyway
                if config.gc_between_runs && successful_runs.len() < config.runs as usize {
                    if let Err(e) = gc::run_gc(logger, Some(600)).await {
                        // GC failure is not fatal - log and continue
                        warn!(error = %e, "Garbage collection failed, continuing");
                        logger.write_line(&format!("Warning: GC failed: {}. Continuing...", e));
                    }
                }
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

                logger.write_line(&format!(
                    "Run {} FAILED ({}/{} failures): {}. {} attempts remaining.",
                    current_run, failure_count, config.max_failures, e, remaining_attempts
                ));
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
    info!(
        bucket = %args.bucket,
        run_id = %args.run_id,
        instance_type = %args.instance_type,
        grpc_port = args.grpc_port,
        "Starting nix-bench-agent"
    );

    // Create gRPC infrastructure - broadcaster for log streaming, status for queries
    let broadcaster = Arc::new(grpc::LogBroadcaster::new(DEFAULT_BROADCAST_CAPACITY));
    let status = Arc::new(RwLock::new(grpc::AgentStatus {
        status: StatusCode::Bootstrap,
        run_progress: 0,
        total_runs: 0,
        run_results: Vec::new(),
        attr: String::new(),
        system: String::new(),
    }));
    let shutdown_token = CancellationToken::new();

    // === Bootstrap Phase ===
    // All output streams to gRPC for TUI visibility
    info!("Starting bootstrap phase");

    if let Err(e) = bootstrap::run_bootstrap(&broadcaster).await {
        error!(error = %e, "Bootstrap failed");
        // Update status to failed
        {
            let mut s = status.write().await;
            s.status = StatusCode::Failed;
        }
        // Give clients a moment to see the failure status
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        shutdown_token.cancel();
        return Err(e);
    }

    // === Load Config from S3 (with TLS polling) ===
    // The coordinator generates TLS certificates after instances are running.
    // We need to poll until the config includes TLS certificates.
    info!("Waiting for config with TLS certificates from S3");
    let logger = logging::GrpcLogger::new(broadcaster.clone());
    logger.write_line("Waiting for benchmark config with TLS certificates from S3...");

    let tls_timeout = std::time::Duration::from_secs(300); // 5 minutes timeout
    let config = match results::download_config_with_tls(
        &args.bucket,
        &args.run_id,
        &args.instance_type,
        tls_timeout,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => {
            error!(error = %e, "Failed to download config with TLS");
            logger.write_line(&format!("ERROR: Failed to get TLS config: {}", e));
            {
                let mut s = status.write().await;
                s.status = StatusCode::Failed;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            shutdown_token.cancel();
            return Err(e);
        }
    };

    info!(
        attr = %config.attr,
        flake_ref = %config.flake_ref,
        runs = config.runs,
        build_timeout = config.build_timeout,
        max_failures = config.max_failures,
        "Config loaded"
    );

    // Get TLS config (required - will fail if missing)
    let tls_config = nix_bench_agent::config::get_tls_config(&config).map_err(|e| {
        error!(error = %e, "TLS configuration is required but missing");
        anyhow::anyhow!("TLS configuration is required: {}", e)
    })?;

    // === Start gRPC Server with mTLS ===
    info!("Starting gRPC server with mTLS");
    logger.write_line("Starting gRPC server with mTLS...");

    let grpc_broadcaster = broadcaster.clone();
    let grpc_run_id = args.run_id.clone();
    let grpc_instance_type = args.instance_type.clone();
    let grpc_status = status.clone();
    let grpc_shutdown = shutdown_token.clone();
    let grpc_port = args.grpc_port;
    let ack_flag = grpc::new_ack_flag();
    let ack_flag_for_grpc = ack_flag.clone();

    let grpc_handle = tokio::spawn(async move {
        if let Err(e) = grpc::run_grpc_server(
            grpc_port,
            grpc_broadcaster,
            grpc_run_id,
            grpc_instance_type,
            grpc_status,
            tls_config,
            grpc_shutdown,
            ack_flag_for_grpc,
        )
        .await
        {
            error!(error = %e, "gRPC server error");
        }
    });

    // Give gRPC server a moment to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Update status with config info
    {
        let mut s = status.write().await;
        s.total_runs = config.runs;
        s.attr = config.attr.clone();
        s.system = config.system.clone();
        s.status = StatusCode::Warmup;
    }

    // === Cache Warmup Phase ===
    info!("Starting cache warmup build (not timed)");
    logger.write_line("=== Cache Warmup (not timed) ===");
    logger.write_line("Running throwaway build to warm Nix cache...");

    match benchmark::run_nix_build(
        &config.flake_ref,
        &config.attr,
        &logger,
        Some(config.build_timeout),
    )
    .await
    {
        Ok(()) => {
            info!("Cache warmup complete");
            logger.write_line("Cache warmup complete");

            // Run GC after warmup to start benchmarks from clean state
            if config.gc_between_runs {
                if let Err(e) = gc::run_gc(&logger, Some(600)).await {
                    warn!(error = %e, "Garbage collection failed, continuing");
                    logger.write_line(&format!("Warning: GC failed: {}. Continuing...", e));
                }
            }

            logger.write_line("Starting timed runs...");
        }
        Err(e) => {
            error!(error = %e, "Cache warmup build failed");
            logger.write_line(&format!("Error: warmup build failed: {}", e));
            {
                let mut s = status.write().await;
                s.status = StatusCode::Failed;
            }
            return Err(e);
        }
    }

    // === Benchmark Phase ===
    {
        let mut s = status.write().await;
        s.status = StatusCode::Running;
    }

    logger.write_line(&format!(
        "Starting benchmark: {} runs of {} (from {}), timeout {}s, max {} failures",
        config.runs, config.attr, config.flake_ref, config.build_timeout, config.max_failures
    ));

    let run_results = run_benchmarks_with_retry(&config, &logger, &status).await?;

    // Signal completion (results are now available via gRPC GetStatus)
    {
        let mut s = status.write().await;
        s.status = StatusCode::Complete;
    }

    logger.write_line(&format!(
        "Benchmark complete: {} successful runs",
        run_results.len()
    ));
    info!("Benchmark complete");

    // Wait for coordinator to acknowledge completion, with fallback timeout
    info!("Waiting for coordinator acknowledgment...");
    match grpc::wait_for_ack(&ack_flag, std::time::Duration::from_secs(120)).await {
        Ok(()) => info!("Coordinator acknowledged completion"),
        Err(_) => warn!("Timed out waiting for coordinator ack, shutting down anyway"),
    }

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
