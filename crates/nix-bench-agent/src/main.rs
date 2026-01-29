//! nix-bench-agent: Benchmark agent that runs on EC2 instances
//!
//! This agent handles all host setup (NVMe, Nix installation) and runs benchmarks.
//! All output is streamed via gRPC to the coordinator TUI.

use anyhow::Result;
use clap::Parser;
use grpc::StatusCode;
use nix_bench_agent::{benchmark, bootstrap, gc, grpc, logging, results};
use nix_bench_common::defaults::{DEFAULT_ACK_TIMEOUT_SECS, DEFAULT_BROADCAST_CAPACITY};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

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

#[tokio::main]
async fn main() -> Result<()> {
    nix_bench_common::init_rustls();
    nix_bench_common::init_tracing_default();

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
        ..Default::default()
    }));
    let shutdown_token = CancellationToken::new();
    let ack_flag = grpc::new_ack_flag();

    // Run main agent logic, capturing any error
    let result = run_agent(&args, &broadcaster, &status, &shutdown_token, &ack_flag).await;

    // On failure, update status and give clients time to observe
    if let Err(ref e) = result {
        error!(error = %e, "Agent failed");
        let mut s = status.write().await;
        if s.status != StatusCode::Failed {
            s.status = StatusCode::Failed;
            s.error_message = e.to_string();
        }
        drop(s);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        shutdown_token.cancel();
    }

    // Wait for coordinator acknowledgment, then shutdown
    info!("Waiting for coordinator acknowledgment...");
    match grpc::wait_for_ack(
        &ack_flag,
        std::time::Duration::from_secs(DEFAULT_ACK_TIMEOUT_SECS),
    )
    .await
    {
        Ok(()) => info!("Coordinator acknowledged completion"),
        Err(_) => warn!("Timed out waiting for coordinator ack, shutting down anyway"),
    }

    shutdown_token.cancel();
    result
}

/// Core agent logic: bootstrap -> config -> gRPC server -> warmup -> benchmark
async fn run_agent(
    args: &Args,
    broadcaster: &Arc<grpc::LogBroadcaster>,
    status: &Arc<RwLock<grpc::AgentStatus>>,
    shutdown_token: &CancellationToken,
    ack_flag: &grpc::AckFlag,
) -> Result<()> {
    // === Bootstrap Phase ===
    info!("Starting bootstrap phase");
    bootstrap::run_bootstrap(broadcaster).await?;

    // === Load Config from S3 (with TLS polling) ===
    info!("Waiting for config with TLS certificates from S3");
    let logger = logging::GrpcLogger::new(broadcaster.clone());
    logger.write_line("Waiting for benchmark config with TLS certificates from S3...");

    let tls_timeout =
        std::time::Duration::from_secs(nix_bench_common::defaults::DEFAULT_TLS_CONFIG_TIMEOUT_SECS);
    let config = results::download_config_with_tls(
        &args.bucket,
        &args.run_id,
        &args.instance_type,
        tls_timeout,
        shutdown_token,
    )
    .await?;

    info!(
        attr = %config.attr,
        flake_ref = %config.flake_ref,
        runs = config.runs,
        "Config loaded"
    );

    // Get TLS config (required)
    let tls_config = nix_bench_agent::config::get_tls_config(&config)
        .map_err(|e| anyhow::anyhow!("TLS configuration is required: {}", e))?;

    // === Start gRPC Server with mTLS ===
    info!("Starting gRPC server with mTLS");
    logger.write_line("Starting gRPC server with mTLS...");

    let grpc_handle = {
        let broadcaster = broadcaster.clone();
        let run_id = args.run_id.clone();
        let instance_type = args.instance_type.clone();
        let status = status.clone();
        let shutdown_token = shutdown_token.clone();
        let ack_flag = ack_flag.clone();
        let port = args.grpc_port;

        tokio::spawn(async move {
            if let Err(e) = grpc::run_grpc_server(grpc::GrpcServerConfig {
                port,
                broadcaster,
                run_id,
                instance_type,
                status,
                tls_config,
                shutdown_token,
                ack_flag,
            })
            .await
            {
                error!(error = %e, "gRPC server error");
            }
        })
    };

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

    benchmark::run_nix_build(
        &config.flake_ref,
        &config.attr,
        &logger,
        Some(config.build_timeout),
    )
    .await?;

    info!("Cache warmup complete");
    logger.write_line("Cache warmup complete");

    // Run GC after warmup if enabled
    if config.gc_between_runs {
        if let Err(e) = gc::run_gc(&logger, Some(600)).await {
            warn!(error = %e, "Garbage collection failed, continuing");
            logger.write_line(&format!("Warning: GC failed: {}. Continuing...", e));
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

    let run_results =
        benchmark::run_benchmarks_with_retry(&config, &logger, status, shutdown_token).await?;

    {
        let mut s = status.write().await;
        s.status = StatusCode::Complete;
    }
    logger.write_line(&format!(
        "Benchmark complete: {} successful runs",
        run_results.len()
    ));
    info!("Benchmark complete");

    // Gracefully shut down gRPC server
    shutdown_token.cancel();
    match tokio::time::timeout(std::time::Duration::from_secs(5), grpc_handle).await {
        Ok(Ok(())) => info!("gRPC server shut down cleanly"),
        Ok(Err(e)) if e.is_panic() => error!("gRPC server task panicked: {:?}", e),
        Ok(Err(_)) => warn!("gRPC server task was cancelled"),
        Err(_) => warn!("Timed out waiting for gRPC server shutdown"),
    }

    Ok(())
}
