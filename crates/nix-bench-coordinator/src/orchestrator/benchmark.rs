//! Benchmark execution functions
//!
//! This module contains the main benchmark execution logic for both
//! TUI and non-TUI modes.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::aws::{GrpcStatusPoller, LogStreamingOptions, start_log_streaming_unified};
use crate::config::RunConfig;
use nix_bench_common::StatusCode;
use crate::tui::{self, InitPhase, TuiMessage};
use super::init::BenchmarkInitializer;
use super::progress::{ChannelReporter, LogReporter};
use super::types::{InstanceState, InstanceStatus};
use super::cleanup::cleanup_resources;
use super::monitoring::{poll_bootstrap_status, watch_and_terminate_completed};
use super::results::{print_results_summary, write_results};
use super::user_data::detect_bootstrap_failure;
use super::GRPC_PORT;

/// Run benchmarks with TUI - starts TUI immediately, runs init in background
pub async fn run_benchmarks_with_tui(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
) -> Result<()> {
    use crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture},
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };
    use ratatui::prelude::*;
    use std::io;

    // Create channel for TUI updates
    let (tx, mut rx) = mpsc::channel::<TuiMessage>(100);

    // Setup terminal FIRST
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create empty instances map - will be filled by background task
    let mut instances: HashMap<String, InstanceState> = HashMap::new();

    // Create app state in loading mode (TLS config is sent via channel later)
    let mut app = tui::App::new_loading(&config.instance_types, config.runs, None);

    // Create cancellation token shared between TUI and init task
    let cancel_token = CancellationToken::new();
    let cancel_for_init = cancel_token.clone();
    let cancel_for_tui = cancel_token.clone();

    // Clone what we need for the background task
    let config_clone = config.clone();
    let run_id_clone = run_id.clone();
    let bucket_name_clone = bucket_name.clone();
    let tx_clone = tx;

    // Spawn background initialization task
    let init_handle = tokio::spawn(async move {
        run_init_task(
            config_clone,
            run_id_clone,
            bucket_name_clone,
            agent_x86_64,
            agent_aarch64,
            tx_clone,
            cancel_for_init,
        )
        .await
    });

    // Run the TUI with channel (gRPC status polling happens inside the TUI)
    let tui_result = app
        .run_with_channel(&mut terminal, &mut rx, cancel_for_tui)
        .await;

    // If user cancelled, abort init task early instead of waiting for it
    let init_result = if cancel_token.is_cancelled() {
        info!("User cancelled, aborting initialization task");
        init_handle.abort();
        // Brief wait for abort to take effect, then continue to cleanup
        match tokio::time::timeout(Duration::from_millis(500), init_handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => {
                // JoinError (task was aborted) - this is expected
                Ok(HashMap::new())
            }
            Err(_) => {
                // Timeout - task didn't respond, continue with empty state
                Ok(HashMap::new())
            }
        }
    } else {
        init_handle.await?
    };

    // Update instances from app state
    for (instance_type, state) in &app.instances.data {
        instances.insert(instance_type.clone(), state.clone());
    }

    // Handle cleanup and results
    if let Err(e) = &init_result {
        error!(error = ?e, "Initialization failed");
    }

    // Do cleanup BEFORE restoring terminal so progress is visible in TUI
    // Create a new channel for cleanup progress
    let (cleanup_tx, cleanup_rx) = mpsc::channel::<TuiMessage>(100);

    let cleanup_config = config.clone();
    let cleanup_run_id = run_id.clone();
    let cleanup_bucket = bucket_name.clone();
    let cleanup_instances = instances.clone();

    let cleanup_handle = tokio::spawn(async move {
        cleanup_resources(
            &cleanup_config,
            &cleanup_run_id,
            &cleanup_bucket,
            &cleanup_instances,
            Some(cleanup_tx),
        )
        .await
    });

    // Run cleanup phase TUI loop (shows progress until cleanup completes)
    app.run_cleanup_phase(&mut terminal, cleanup_handle, cleanup_rx).await?;

    // Now restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Print any captured errors/warnings to stderr
    if let Some(ref log_capture) = config.log_capture {
        log_capture.print_to_stderr();
    }

    // Print results summary to stdout
    print_results_summary(&instances);

    // Write output file if requested
    write_results(&config, &run_id, &instances).await?;

    tui_result.and(init_result.map(|_| ()))
}

/// Background task that runs initialization and sends updates to TUI
pub async fn run_init_task(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
    tx: mpsc::Sender<TuiMessage>,
    cancel: CancellationToken,
) -> Result<HashMap<String, InstanceState>> {
    // Create reporter for TUI updates
    let reporter = ChannelReporter::new(tx.clone(), cancel);

    // Run initialization using the BenchmarkInitializer
    let initializer = BenchmarkInitializer::new(
        &config,
        run_id.clone(),
        bucket_name,
        agent_x86_64,
        agent_aarch64,
    );

    let ctx = initializer.initialize(&reporter).await?;

    // Extract what we need from the context
    let instances = ctx.instances;
    let coordinator_tls_config = ctx.coordinator_tls_config;

    // Send TLS config to TUI for status polling
    let _ = tx.send(TuiMessage::TlsConfig { config: coordinator_tls_config.clone() }).await;

    // Switch to running phase
    let _ = tx.send(TuiMessage::Phase(InitPhase::Running)).await;

    // Start gRPC log streaming for instances with public IPs (using mTLS)
    let instances_with_ips: Vec<(String, String)> = instances
        .iter()
        .filter_map(|(instance_type, state)| {
            state
                .public_ip
                .as_ref()
                .map(|ip| (instance_type.clone(), ip.clone()))
        })
        .collect();

    // Clone TLS config for termination watcher before it's moved
    let tls_config_for_watcher = coordinator_tls_config.clone();

    if !instances_with_ips.is_empty() {
        info!(
            count = instances_with_ips.len(),
            "Starting gRPC log streaming for instances with mTLS"
        );

        // Use unified log streaming with options
        let options = LogStreamingOptions::new(&instances_with_ips, &run_id, GRPC_PORT, coordinator_tls_config)
            .with_channel(tx.clone());
        let grpc_handles = start_log_streaming_unified(options);

        // Spawn a monitor task to log any gRPC streaming errors/panics
        tokio::spawn(async move {
            for handle in grpc_handles {
                match handle.await {
                    Ok(Ok(())) => {
                        debug!("gRPC streaming task completed successfully");
                    }
                    Ok(Err(e)) => {
                        warn!(error = ?e, "gRPC streaming task failed");
                    }
                    Err(e) => {
                        if e.is_panic() {
                            error!(error = ?e, "gRPC streaming task panicked");
                        } else {
                            debug!(error = ?e, "gRPC streaming task cancelled");
                        }
                    }
                }
            }
        });
    } else {
        warn!("No instances have public IPs, gRPC streaming not available");
    }

    // Spawn EC2 console output polling for bootstrap failure detection
    // This catches failures before the agent starts (and its gRPC server)
    let instances_for_logs = instances.clone();
    let tx_logs = tx.clone();
    let region = config.region.clone();
    let timeout_secs = config.timeout;
    let start_time = Instant::now();
    tokio::spawn(async move {
        poll_bootstrap_status(instances_for_logs, tx_logs, region, timeout_secs, start_time).await;
    });

    // Spawn termination watcher to terminate instances immediately when they complete
    let instances_for_termination = instances.clone();
    let region_for_termination = config.region.clone();
    let run_id_for_termination = run_id.clone();
    tokio::spawn(async move {
        watch_and_terminate_completed(
            instances_for_termination,
            region_for_termination,
            run_id_for_termination,
            tls_config_for_watcher,
        ).await;
    });

    Ok(instances)
}

/// Run benchmarks without TUI (--no-tui mode)
pub async fn run_benchmarks_no_tui(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
) -> Result<()> {
    // Use LogReporter for non-TUI mode (logs to tracing)
    let reporter = LogReporter::new();

    // Run initialization using the BenchmarkInitializer
    let initializer = BenchmarkInitializer::new(
        &config,
        run_id.clone(),
        bucket_name.clone(),
        agent_x86_64,
        agent_aarch64,
    );

    let ctx = initializer.initialize(&reporter).await?;

    // Extract what we need from the context
    let mut instances = ctx.instances;
    let coordinator_tls_config = ctx.coordinator_tls_config;
    let _db = ctx.db; // Keep the connection alive for the duration of the run
    let ec2 = ctx.ec2;

    // Start gRPC log streaming for instances with public IPs (with TLS if available)
    let instances_with_ips: Vec<(String, String)> = instances
        .iter()
        .filter_map(|(instance_type, state)| {
            state
                .public_ip
                .as_ref()
                .map(|ip| (instance_type.clone(), ip.clone()))
        })
        .collect();

    let grpc_handles = if !instances_with_ips.is_empty() {
        info!(
            count = instances_with_ips.len(),
            "Starting gRPC log streaming to stdout with mTLS"
        );

        // Use unified log streaming with stdout output
        let options = LogStreamingOptions::new(&instances_with_ips, &run_id, GRPC_PORT, coordinator_tls_config.clone());
        let handles = start_log_streaming_unified(options);

        // Spawn a monitor task to log any gRPC streaming errors/panics
        let monitor_handles: Vec<_> = handles.into_iter().collect();
        tokio::spawn(async move {
            for handle in monitor_handles {
                match handle.await {
                    Ok(Ok(())) => {
                        debug!("gRPC streaming task completed successfully");
                    }
                    Ok(Err(e)) => {
                        warn!(error = ?e, "gRPC streaming task failed");
                    }
                    Err(e) => {
                        if e.is_panic() {
                            error!(error = ?e, "gRPC streaming task panicked");
                        } else {
                            debug!(error = ?e, "gRPC streaming task cancelled");
                        }
                    }
                }
            }
        });
        true
    } else {
        warn!("No instances have public IPs, gRPC streaming not available");
        false
    };

    // Track start time for reporting and timeout
    let start_time = chrono::Utc::now();
    let start_instant = Instant::now();

    // Simple polling mode (with gRPC streaming in background if available)
    println!("\n=== nix-bench-ec2 ===");
    println!("Run ID: {}", run_id);
    println!("Instances: {}", config.instance_types.join(", "));
    println!("Benchmark: {} ({} runs each)", config.attr, config.runs);
    if config.timeout > 0 {
        println!("Timeout: {}s", config.timeout);
    }
    if grpc_handles {
        println!("Log streaming: gRPC (real-time)");
    } else {
        println!("Log streaming: unavailable (no instances with public IPs)");
    }
    println!("Started: {}\n", start_time.format("%Y-%m-%d %H:%M:%S UTC"));

    loop {
        // Check for timeout
        let elapsed_secs = start_instant.elapsed().as_secs();
        if config.timeout > 0 && elapsed_secs > config.timeout {
            warn!(
                elapsed_secs = elapsed_secs,
                timeout = config.timeout,
                "Run timeout exceeded"
            );
            println!("\n  TIMEOUT: Run exceeded {}s limit", config.timeout);
            for (instance_type, state) in instances.iter_mut() {
                if state.status != InstanceStatus::Complete
                    && state.status != InstanceStatus::Failed
                {
                    error!(instance_type = %instance_type, "Instance timed out");
                    state.status = InstanceStatus::Failed;
                }
            }
            break;
        }

        // Poll gRPC GetStatus from agents with IPs
        let instances_with_ips: Vec<(String, String)> = instances
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect();

        let status_map = if !instances_with_ips.is_empty() {
            let poller = GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT, coordinator_tls_config.clone());
            poller.poll_status().await
        } else {
            std::collections::HashMap::new()
        };

        let mut all_complete = true;
        let mut total_runs = 0u32;
        let mut completed_runs = 0u32;

        for (instance_type, state) in instances.iter_mut() {
            // Skip already failed instances
            if state.status == InstanceStatus::Failed {
                total_runs += state.total_runs;
                continue;
            }

            if let Some(grpc_status) = status_map.get(instance_type) {
                if let Some(status_code) = grpc_status.status {
                    state.status = match status_code {
                        StatusCode::Complete => InstanceStatus::Complete,
                        StatusCode::Failed => InstanceStatus::Failed,
                        StatusCode::Running | StatusCode::Bootstrap | StatusCode::Warmup => InstanceStatus::Running,
                        StatusCode::Pending => InstanceStatus::Pending,
                    };
                }
                if let Some(progress) = grpc_status.run_progress {
                    state.run_progress = progress;
                }
                state.durations = grpc_status.durations.clone();
            } else {
                // No gRPC status yet - check console output for bootstrap failures
                if !state.instance_id.is_empty() && state.status == InstanceStatus::Running {
                    if let Ok(Some(console_output)) =
                        ec2.get_console_output(&state.instance_id).await
                    {
                        if let Some(failure_pattern) = detect_bootstrap_failure(&console_output) {
                            error!(
                                instance_type = %instance_type,
                                instance_id = %state.instance_id,
                                pattern = %failure_pattern,
                                "Bootstrap failure detected"
                            );
                            println!(
                                "\n  Bootstrap failure on {}: {}",
                                instance_type, failure_pattern
                            );
                            state.status = InstanceStatus::Failed;
                        }
                    }
                }
            }

            if state.status != InstanceStatus::Complete && state.status != InstanceStatus::Failed {
                all_complete = false;
            }

            total_runs += state.total_runs;
            completed_runs += state.run_progress;
        }

        // Print progress update
        let elapsed = chrono::Utc::now() - start_time;
        let elapsed_str = format!(
            "{:02}:{:02}:{:02}",
            elapsed.num_hours(),
            elapsed.num_minutes() % 60,
            elapsed.num_seconds() % 60
        );

        println!(
            "[{}] Progress: {}/{} runs ({:.1}%)",
            elapsed_str,
            completed_runs,
            total_runs,
            if total_runs > 0 {
                completed_runs as f64 / total_runs as f64 * 100.0
            } else {
                0.0
            }
        );

        for (instance_type, state) in instances.iter() {
            let status_str = match state.status {
                InstanceStatus::Pending => "  pending",
                InstanceStatus::Launching => "  launching",
                InstanceStatus::Running => "  running",
                InstanceStatus::Complete => "  complete",
                InstanceStatus::Failed => "  failed",
            };
            let avg = if !state.durations.is_empty() {
                format!(
                    " (avg: {:.1}s)",
                    state.durations.iter().sum::<f64>() / state.durations.len() as f64
                )
            } else {
                String::new()
            };
            println!(
                "  {} {}: {}/{}{}",
                status_str, instance_type, state.run_progress, state.total_runs, avg
            );
        }
        println!();

        if all_complete {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }

    // Cleanup and results
    cleanup_resources(&config, &run_id, &bucket_name, &instances, None).await?;

    // Print results summary to stdout
    print_results_summary(&instances);

    // Write output file if requested
    write_results(&config, &run_id, &instances).await?;

    info!("Benchmark run complete");
    Ok(())
}
