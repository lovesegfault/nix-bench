//! Benchmark execution functions
//!
//! This module contains the main benchmark execution logic for both
//! TUI and non-TUI modes, with shared infrastructure extracted into
//! `BenchmarkInitializer`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::progress::Reporter;
use super::results::{print_results_summary, write_results};
use super::types::InstanceState;
use crate::config::RunConfig;
use crate::tui::{self, LogCapture, TuiMessage};
use nix_bench_common::RunId;

// ─── TUI Mode ───────────────────────────────────────────────────────────────

/// Run benchmarks with TUI - starts TUI immediately, runs init in background
pub async fn run_benchmarks_with_tui(
    config: RunConfig,
    run_id: RunId,
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

    // Setup terminal FIRST
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state in loading mode
    let mut app = tui::App::new_loading(&config.instances.instance_types, config.benchmark.runs);

    // Create cancellation token shared between TUI and init task
    let cancel_token = CancellationToken::new();
    let cancel_for_tui = cancel_token.clone();

    // Spawn background initialization task
    let config_clone = config.clone();
    let run_id_clone = run_id.clone();
    let tx_clone = tx;

    let init_handle = tokio::spawn(async move {
        run_init_task(InitTaskConfig {
            config: config_clone,
            run_id: run_id_clone,
            bucket_name: bucket_name.clone(),
            agent_x86_64,
            agent_aarch64,
            tx: tx_clone,
        })
        .await
    });

    // Run the TUI event loop (gRPC status polling happens via engine inside)
    let tui_result = app
        .run_with_channel(&mut terminal, &mut rx, cancel_for_tui)
        .await;

    // If user cancelled, abort init task early
    if cancel_token.is_cancelled() {
        info!("User cancelled, aborting initialization task");
        init_handle.abort();
        let _ = tokio::time::timeout(Duration::from_millis(500), init_handle).await;
    } else if let Err(e) = init_handle.await? {
        error!(error = ?e, "Initialization failed");
    }

    // Take engine out of app (we need ownership for spawning cleanup)
    if let Some(engine) = app.engine.take() {
        let cleanup_join =
            tokio::spawn(async move { engine.cleanup(|p| info!("{}", p.current_step)).await });

        // Show cleanup phase TUI while cleanup runs
        app.run_cleanup_phase(&mut terminal, cleanup_join, rx)
            .await?;
    }

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
    // (engine was consumed by cleanup, use app.instances which has synced state)
    print_results_summary(&app.instances.data);
    write_results(&config, run_id.as_str(), &app.instances.data).await?;

    tui_result
}

/// Parameters for the background initialization task.
struct InitTaskConfig {
    config: RunConfig,
    run_id: RunId,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
    tx: mpsc::Sender<TuiMessage>,
}

/// Background task that runs initialization and sends updates to TUI.
///
/// When initialization succeeds, sends `TuiMessage::InitComplete(engine)`.
/// gRPC log streaming is started before sending the engine, so logs
/// flow to the TUI via `ConsoleOutputAppend` messages.
async fn run_init_task(params: InitTaskConfig) -> Result<()> {
    let InitTaskConfig {
        config,
        run_id,
        bucket_name,
        agent_x86_64,
        agent_aarch64,
        tx,
    } = params;

    let reporter = Reporter::channel(tx.clone());
    let engine = super::init::initialize(
        &config,
        run_id,
        bucket_name,
        agent_x86_64,
        agent_aarch64,
        &reporter,
    )
    .await?;

    // Start gRPC log streaming (sends ConsoleOutputAppend to TUI channel)
    engine.start_streaming(Some(tx.clone()));

    // Send the engine to the TUI (this transitions to Running phase)
    let _ = tx.send(TuiMessage::InitComplete(Box::new(engine))).await;

    Ok(())
}

// ─── CLI (no-TUI) Mode ─────────────────────────────────────────────────────

/// Run benchmarks without TUI (--no-tui mode)
pub async fn run_benchmarks_no_tui(
    config: RunConfig,
    run_id: RunId,
    bucket_name: String,
    agent_x86_64: Option<String>,
    agent_aarch64: Option<String>,
) -> Result<()> {
    use super::events::RunEvent;

    let reporter = Reporter::Log;
    let mut engine = super::init::initialize(
        &config,
        run_id.clone(),
        bucket_name.clone(),
        agent_x86_64,
        agent_aarch64,
        &reporter,
    )
    .await?;
    let has_streaming = !engine.instances_with_ips().is_empty();
    engine.start_streaming(None);

    let start_time = chrono::Utc::now();

    print_no_tui_header(&config, &run_id, has_streaming, start_time);

    // Polling loop — driven by RunEngine
    loop {
        // Check timeout
        let timeout_events = engine.check_timeout();
        for event in &timeout_events {
            if let RunEvent::Timeout { instance_type } = event {
                println!("\n  TIMEOUT: {} exceeded limit", instance_type);
            }
        }
        if !timeout_events.is_empty() {
            break;
        }

        // Poll gRPC status
        if let Some(req) = engine.prepare_poll() {
            let status_map = req.execute().await;
            let events = engine.apply_poll_results(status_map).await;
            for event in &events {
                match event {
                    RunEvent::InstanceTerminal { instance_type } => {
                        info!(instance_type = %instance_type, "Instance completed, auto-terminating");
                    }
                    RunEvent::BootstrapFailure {
                        instance_type,
                        pattern,
                    } => {
                        println!("\n  Bootstrap failure on {}: {}", instance_type, pattern);
                    }
                    _ => {}
                }
            }
        }

        // Check bootstrap failures for instances not yet responding to gRPC
        let bootstrap_events = engine.check_bootstrap_failures().await;
        for event in &bootstrap_events {
            if let RunEvent::BootstrapFailure {
                instance_type,
                pattern,
            } = event
            {
                println!("\n  Bootstrap failure on {}: {}", instance_type, pattern);
            }
        }

        print_no_tui_progress(engine.instances(), start_time);

        if engine.is_all_complete() {
            break;
        }

        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }

    // Cleanup
    engine.cleanup(|p| info!("{}", p.current_step)).await?;

    // Print results and write output
    engine.report_results().await?;

    info!("Benchmark run complete");
    Ok(())
}

fn print_no_tui_header(
    config: &RunConfig,
    run_id: &RunId,
    has_streaming: bool,
    start_time: chrono::DateTime<chrono::Utc>,
) {
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
    println!(
        "Log streaming: {}",
        if has_streaming {
            "gRPC (real-time)"
        } else {
            "unavailable (no instances with public IPs)"
        }
    );
    println!("Started: {}\n", start_time.format("%Y-%m-%d %H:%M:%S UTC"));
}

fn print_no_tui_progress(
    instances: &HashMap<String, InstanceState>,
    start_time: chrono::DateTime<chrono::Utc>,
) {
    let total_runs: u32 = instances.values().map(|s| s.total_runs).sum();
    let completed_runs: u32 = instances.values().map(|s| s.run_progress).sum();
    let elapsed = chrono::Utc::now() - start_time;
    let pct = if total_runs > 0 {
        completed_runs as f64 / total_runs as f64 * 100.0
    } else {
        0.0
    };
    println!(
        "[{:02}:{:02}:{:02}] Progress: {}/{} runs ({:.1}%)",
        elapsed.num_hours(),
        elapsed.num_minutes() % 60,
        elapsed.num_seconds() % 60,
        completed_runs,
        total_runs,
        pct
    );
    for (instance_type, state) in instances.iter() {
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
            "  {:>10} {}: {}/{}{}",
            state.status.as_ref(),
            instance_type,
            state.run_progress,
            state.total_runs,
            avg
        );
    }
    println!();
}
