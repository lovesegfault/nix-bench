//! Nix build execution and retry logic

use crate::grpc::{AgentStatus, StatusCode};
use crate::logging::GrpcLogger;
use crate::{config, gc};
use anyhow::Result;
use nix_bench_common::RunResult;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;
use tracing::{info, warn};

/// Run nix build with streaming output to gRPC clients
///
/// # Arguments
/// * `flake_base` - Base flake reference (e.g., "github:lovesegfault/nix-bench")
/// * `attr` - Attribute to build (e.g., "large-deep")
/// * `logger` - gRPC logger for streaming output
/// * `timeout_secs` - Optional timeout in seconds
pub async fn run_nix_build(
    flake_base: &str,
    attr: &str,
    logger: &GrpcLogger,
    timeout_secs: Option<u64>,
) -> Result<()> {
    info!(flake_base, attr, "Running nix build with log streaming");

    let flake_ref = format!("{}#{}", flake_base, attr);

    let success = logger
        .run_command(
            "nix",
            &[
                "build",
                &flake_ref,
                "--impure",
                "--no-link",
                "-L", // Print build logs
                "--log-format",
                "bar-with-logs", // Show progress bar + build output
            ],
            timeout_secs,
        )
        .await?;

    if !success {
        anyhow::bail!("nix build failed");
    }

    Ok(())
}

/// Run benchmarks with smart retry logic.
///
/// On failure, schedules a replacement run. Gives up after max_failures total failures.
pub async fn run_benchmarks_with_retry(
    config: &config::Config,
    logger: &GrpcLogger,
    status: &Arc<RwLock<AgentStatus>>,
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

        match run_nix_build(
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
