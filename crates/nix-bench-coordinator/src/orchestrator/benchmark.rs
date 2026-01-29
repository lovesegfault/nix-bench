//! Benchmark execution functions
//!
//! This module contains the main benchmark execution logic for both
//! TUI and non-TUI modes, with shared infrastructure extracted into
//! `BenchmarkRunner`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::GRPC_PORT;
use super::cleanup::{CleanupRequest, cleanup_executor, cleanup_resources_no_tui};
use super::init::{BenchmarkInitializer, InitContext};
use super::monitoring::poll_bootstrap_status;
use super::progress::{ChannelReporter, InitProgressReporter, LogReporter};
use super::results::{print_results_summary, write_results};
use super::types::{InstanceState, InstanceStatus};
use super::user_data::detect_bootstrap_failure;
use crate::aws::{
    GrpcStatusPoller, LogStreamingOptions, send_ack_complete, start_log_streaming_unified,
};
use crate::config::RunConfig;
use crate::tui::{self, InitPhase, LogCapture, TuiMessage};
use nix_bench_common::{StatusCode, TlsConfig};

// ─── Shared Infrastructure ──────────────────────────────────────────────────

/// Shared benchmark runner that encapsulates common logic between TUI and CLI modes.
///
/// Both `run_benchmarks_with_tui` and `run_benchmarks_no_tui` use this struct
/// for initialization, gRPC streaming setup, and results output.
pub struct BenchmarkRunner<'a> {
    pub config: &'a RunConfig,
    pub run_id: String,
    pub bucket_name: String,
    pub agent_x86_64: Option<String>,
    pub agent_aarch64: Option<String>,
}

impl<'a> BenchmarkRunner<'a> {
    pub fn new(
        config: &'a RunConfig,
        run_id: String,
        bucket_name: String,
        agent_x86_64: Option<String>,
        agent_aarch64: Option<String>,
    ) -> Self {
        Self {
            config,
            run_id,
            bucket_name,
            agent_x86_64,
            agent_aarch64,
        }
    }

    /// Run initialization using the given progress reporter.
    pub async fn initialize<R: InitProgressReporter>(&self, reporter: &R) -> Result<InitContext> {
        let initializer = BenchmarkInitializer::new(
            self.config,
            self.run_id.clone(),
            self.bucket_name.clone(),
            self.agent_x86_64.clone(),
            self.agent_aarch64.clone(),
        );
        initializer.initialize(reporter).await
    }

    /// Start gRPC log streaming for instances with public IPs.
    ///
    /// If `tui_tx` is provided, logs are forwarded to the TUI channel.
    /// Otherwise, logs are printed to stdout.
    pub fn start_streaming(
        &self,
        instances_with_ips: &[(String, String)],
        tls_config: TlsConfig,
        tui_tx: Option<mpsc::Sender<TuiMessage>>,
    ) {
        if instances_with_ips.is_empty() {
            warn!("No instances have public IPs, gRPC streaming not available");
            return;
        }

        info!(
            count = instances_with_ips.len(),
            "Starting gRPC log streaming with mTLS"
        );

        let mut options =
            LogStreamingOptions::new(instances_with_ips, &self.run_id, GRPC_PORT, tls_config);
        if let Some(tx) = tui_tx {
            options = options.with_channel(tx);
        }
        let grpc_handles = start_log_streaming_unified(options);

        // Spawn a monitor task that logs streaming errors/panics
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
    }

    /// Start bootstrap failure monitoring via EC2 console output.
    pub fn start_bootstrap_monitoring(
        &self,
        instances: &HashMap<String, InstanceState>,
        tx: mpsc::Sender<TuiMessage>,
        cancel: CancellationToken,
    ) {
        let instances_for_logs = instances.clone();
        let region = self.config.aws.region.clone();
        let timeout_secs = self.config.flags.timeout;
        let start_time = Instant::now();
        tokio::spawn(async move {
            poll_bootstrap_status(
                instances_for_logs,
                tx,
                region,
                timeout_secs,
                start_time,
                cancel,
            )
            .await;
        });
    }

    /// Extract instance IP pairs from the context.
    pub fn instances_with_ips(ctx: &InitContext) -> Vec<(String, String)> {
        ctx.instances
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect()
    }

    /// Print results and write output file.
    pub async fn report_results(
        config: &RunConfig,
        run_id: &str,
        instances: &HashMap<String, InstanceState>,
    ) -> Result<()> {
        print_results_summary(instances);
        write_results(config, run_id, instances).await
    }
}

// ─── TUI Mode ───────────────────────────────────────────────────────────────

/// Run benchmarks with TUI - starts TUI immediately, runs init in background
pub async fn run_benchmarks_with_tui(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
    log_capture: Option<LogCapture>,
) -> Result<()> {
    use crossterm::{
        event::{DisableMouseCapture, EnableMouseCapture},
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
    };
    use ratatui::prelude::*;
    use std::io;

    // Create channel for TUI updates
    let (tx, mut rx) = mpsc::channel::<TuiMessage>(100);

    // Create channel for cleanup requests (TUI -> cleanup executor)
    let (cleanup_tx, cleanup_rx) = mpsc::channel::<CleanupRequest>(100);

    // Setup terminal FIRST
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create empty instances map - will be filled by background task
    let mut instances: HashMap<String, InstanceState> = HashMap::new();

    // Create app state in loading mode (TLS config is sent via channel later)
    let mut app = tui::App::new_loading(
        &config.instances.instance_types,
        config.benchmark.runs,
        None,
    );

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
            cleanup_rx,
        )
        .await
    });

    // Run the TUI with channel (gRPC status polling happens inside the TUI)
    let tui_result = app
        .run_with_channel(
            &mut terminal,
            &mut rx,
            cancel_for_tui,
            Some(cleanup_tx.clone()),
        )
        .await;

    // If user cancelled, abort init task early instead of waiting for it
    let init_result = if cancel_token.is_cancelled() {
        info!("User cancelled, aborting initialization task");
        init_handle.abort();
        // Brief wait for abort to take effect, then continue to cleanup
        match tokio::time::timeout(Duration::from_millis(500), init_handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Ok(InitResult {
                cleanup_handle: tokio::spawn(async {}),
            }),
            Err(_) => Ok(InitResult {
                cleanup_handle: tokio::spawn(async {}),
            }),
        }
    } else {
        init_handle.await?
    };

    // Update instances from app state (TUI may have more recent data)
    for (instance_type, state) in &app.instances.data {
        instances.insert(instance_type.clone(), state.clone());
    }

    // Log any initialization errors
    if let Err(e) = &init_result {
        error!(error = ?e, "Initialization failed");
    }

    // Get the cleanup handle (consuming init_result)
    let cleanup_handle = match init_result {
        Ok(result) => result.cleanup_handle,
        Err(_) => tokio::spawn(async {}),
    };

    // Send full cleanup request to the cleanup executor
    let _ = cleanup_tx
        .send(CleanupRequest::FullCleanup {
            region: config.aws.region.clone(),
            keep: config.flags.keep,
            run_id: run_id.clone(),
            bucket_name: bucket_name.clone(),
            instances: instances.clone(),
            security_group_id: None,
            iam_role_name: None,
            sg_rules: Vec::new(),
        })
        .await;

    // Drop the sender so the cleanup executor knows no more requests are coming
    drop(cleanup_tx);

    // Wrap the cleanup handle to match expected signature
    let cleanup_handle_wrapped = tokio::spawn(async move {
        let _ = cleanup_handle.await;
        Ok::<(), anyhow::Error>(())
    });

    // Run cleanup phase TUI loop (shows progress until cleanup completes)
    app.run_cleanup_phase(&mut terminal, cleanup_handle_wrapped, rx)
        .await?;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Print any captured errors/warnings to stderr
    if let Some(ref capture) = log_capture {
        capture.print_to_stderr();
    }

    // Print results and write output
    BenchmarkRunner::report_results(&config, &run_id, &instances).await?;

    tui_result
}

/// Result from initialization task
pub struct InitResult {
    pub cleanup_handle: tokio::task::JoinHandle<()>,
}

/// Background task that runs initialization and sends updates to TUI
#[allow(clippy::too_many_arguments)]
pub async fn run_init_task(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
    tx: mpsc::Sender<TuiMessage>,
    cancel: CancellationToken,
    cleanup_rx: mpsc::Receiver<CleanupRequest>,
) -> Result<InitResult> {
    let cancel_for_monitoring = cancel.clone();
    let reporter = ChannelReporter::new(tx.clone(), cancel);
    let runner = BenchmarkRunner::new(
        &config,
        run_id.clone(),
        bucket_name,
        agent_x86_64,
        agent_aarch64,
    );

    let ctx = runner.initialize(&reporter).await?;

    // Extract IP pairs before moving fields out of ctx
    let instances_with_ips = BenchmarkRunner::instances_with_ips(&ctx);
    let instances = ctx.instances;
    let coordinator_tls_config = ctx.coordinator_tls_config;

    // Send TLS config to TUI for status polling
    let _ = tx
        .send(TuiMessage::TlsConfig {
            config: coordinator_tls_config.clone(),
        })
        .await;

    // Switch to running phase
    let _ = tx.send(TuiMessage::Phase(InitPhase::Running)).await;

    // Clone TLS config for cleanup executor before moving into streaming
    let tls_for_cleanup = Some(coordinator_tls_config.clone());

    // Start gRPC log streaming via shared runner
    runner.start_streaming(
        &instances_with_ips,
        coordinator_tls_config,
        Some(tx.clone()),
    );

    // Start bootstrap failure monitoring via shared runner
    runner.start_bootstrap_monitoring(&instances, tx.clone(), cancel_for_monitoring);

    // Spawn cleanup executor to handle cleanup requests from TUI
    let region_for_cleanup = config.aws.region.clone();
    let run_id_for_cleanup = run_id.clone();
    let tx_for_cleanup = tx.clone();
    let cleanup_handle = tokio::spawn(async move {
        cleanup_executor(
            cleanup_rx,
            region_for_cleanup,
            tx_for_cleanup,
            run_id_for_cleanup,
            tls_for_cleanup,
        )
        .await;
    });

    Ok(InitResult { cleanup_handle })
}

// ─── CLI (no-TUI) Mode ─────────────────────────────────────────────────────

/// Run benchmarks without TUI (--no-tui mode)
pub async fn run_benchmarks_no_tui(
    config: RunConfig,
    run_id: String,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
) -> Result<()> {
    let reporter = LogReporter::new();
    let runner = BenchmarkRunner::new(
        &config,
        run_id.clone(),
        bucket_name.clone(),
        agent_x86_64,
        agent_aarch64,
    );

    let ctx = runner.initialize(&reporter).await?;

    // Extract IP pairs before moving fields out of ctx
    let instances_with_ips = BenchmarkRunner::instances_with_ips(&ctx);

    // Extract what we need from the context
    let mut instances = ctx.instances;
    let coordinator_tls_config = ctx.coordinator_tls_config;
    let ec2 = ctx.ec2;
    let security_group_id = ctx.security_group_id;
    let iam_role_name = ctx.iam_role_name;
    let sg_rules = ctx.sg_rules;
    let has_streaming = !instances_with_ips.is_empty();
    runner.start_streaming(&instances_with_ips, coordinator_tls_config.clone(), None);

    // Track start time for reporting and timeout
    let start_time = chrono::Utc::now();
    let start_instant = Instant::now();

    // Track which instances we've sent ack to
    let mut acked_instances: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Print header
    println!("\n=== nix-bench-ec2 ===");
    println!("Run ID: {}", run_id);
    println!("Instances: {}", config.instances.instance_types.join(", "));
    println!(
        "Benchmark: {} ({} runs each)",
        config.benchmark.attr, config.benchmark.runs
    );
    if config.flags.timeout > 0 {
        println!("Timeout: {}s", config.flags.timeout);
    }
    if has_streaming {
        println!("Log streaming: gRPC (real-time)");
    } else {
        println!("Log streaming: unavailable (no instances with public IPs)");
    }
    println!("Started: {}\n", start_time.format("%Y-%m-%d %H:%M:%S UTC"));

    // Polling loop
    loop {
        // Check for timeout
        let elapsed_secs = start_instant.elapsed().as_secs();
        if config.flags.timeout > 0 && elapsed_secs > config.flags.timeout {
            warn!(
                elapsed_secs = elapsed_secs,
                timeout = config.flags.timeout,
                "Run timeout exceeded"
            );
            println!("\n  TIMEOUT: Run exceeded {}s limit", config.flags.timeout);
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
        let poll_ips: Vec<(String, String)> = instances
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect();

        let status_map = if !poll_ips.is_empty() {
            let poller =
                GrpcStatusPoller::new(&poll_ips, GRPC_PORT, coordinator_tls_config.clone());
            poller.poll_status().await
        } else {
            std::collections::HashMap::new()
        };

        let mut all_complete = true;
        let mut total_runs = 0u32;
        let mut completed_runs = 0u32;

        for (instance_type, state) in instances.iter_mut() {
            // Skip instances already in a terminal state that have been acked
            if (state.status == InstanceStatus::Failed || state.status == InstanceStatus::Complete)
                && acked_instances.contains(instance_type)
            {
                total_runs += state.total_runs;
                completed_runs += state.run_progress;
                continue;
            }

            if let Some(grpc_status) = status_map.get(instance_type) {
                let was_failed = state.status == InstanceStatus::Failed;

                if let Some(status_code) = grpc_status.status {
                    state.status = match status_code {
                        StatusCode::Complete => InstanceStatus::Complete,
                        StatusCode::Failed => InstanceStatus::Failed,
                        StatusCode::Running | StatusCode::Bootstrap | StatusCode::Warmup => {
                            InstanceStatus::Running
                        }
                        StatusCode::Pending => InstanceStatus::Pending,
                    };
                }
                if let Some(progress) = grpc_status.run_progress {
                    state.run_progress = progress;
                }
                state.run_results = grpc_status.run_results.clone();

                // Print error message when agent reports failure
                if state.status == InstanceStatus::Failed && !was_failed {
                    if let Some(ref msg) = grpc_status.error_message {
                        println!("\n  {} FAILED: {}", instance_type, msg);
                    }
                }

                // Send ack when instance first transitions to Complete/Failed
                if (state.status == InstanceStatus::Complete
                    || state.status == InstanceStatus::Failed)
                    && !acked_instances.contains(instance_type)
                {
                    if let Some(ip) = &state.public_ip {
                        acked_instances.insert(instance_type.clone());
                        send_ack_complete(ip, GRPC_PORT, &run_id, &coordinator_tls_config).await;
                    }
                }
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
                InstanceStatus::Starting => "  starting",
                InstanceStatus::Running => "  running",
                InstanceStatus::Complete => "  complete",
                InstanceStatus::Failed => "  failed",
                InstanceStatus::Terminated => "  terminated",
            };
            let durations = state.durations();
            let avg = if !durations.is_empty() {
                format!(
                    " (avg: {:.1}s)",
                    durations.iter().sum::<f64>() / durations.len() as f64
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

    // Cleanup
    cleanup_resources_no_tui(
        &config,
        &run_id,
        &bucket_name,
        &instances,
        security_group_id.as_deref(),
        iam_role_name.as_deref(),
        &sg_rules,
    )
    .await?;

    // Print results and write output
    BenchmarkRunner::report_results(&config, &run_id, &instances).await?;

    info!("Benchmark run complete");
    Ok(())
}
