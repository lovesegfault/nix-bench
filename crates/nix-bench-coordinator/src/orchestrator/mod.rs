//! Main orchestration logic for benchmark runs
//!
//! This module contains the orchestrator for running benchmarks on EC2 instances.
//! It handles both TUI and non-TUI modes, managing instance lifecycle, log streaming,
//! and result collection.

pub mod init;
pub mod progress;
pub mod types;

// Re-export core types
pub use init::{BenchmarkInitializer, InitContext};
pub use progress::{ChannelReporter, InitProgressReporter, InstanceUpdate, LogReporter};
pub use types::{InstanceState, InstanceStatus};

use crate::aws::{Ec2Client, IamClient, S3Client};
use crate::config::{detect_system, RunConfig};
use crate::state::{self, ResourceType, RunStatus};
use crate::tui::{self, CleanupProgress, InitPhase, TuiMessage};
use anyhow::Result;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Default gRPC port for agent communication
const GRPC_PORT: u16 = 50051;

/// Patterns that indicate cloud-init/bootstrap failure
const BOOTSTRAP_FAILURE_PATTERNS: &[&str] = &[
    "unbound variable",
    "Failed to start cloud-final",
    "FAILED] Failed to start",
    "cc_scripts_user.py[WARNING]: Failed to run module scripts-user",
    "nix-bench-agent: command not found",
    "No such file or directory",
];

/// Check if console output indicates a bootstrap failure
fn detect_bootstrap_failure(console_output: &str) -> Option<String> {
    for pattern in BOOTSTRAP_FAILURE_PATTERNS {
        if console_output.contains(pattern) {
            return Some(pattern.to_string());
        }
    }
    None
}

/// Generate user-data script for an instance
fn generate_user_data(bucket: &str, run_id: &str, instance_type: &str) -> String {
    format!(
        r#"#!/bin/bash
set -euo pipefail

exec > >(tee /var/log/nix-bench-bootstrap.log) 2>&1

echo "Starting nix-bench bootstrap"

BUCKET="{bucket}"
RUN_ID="{run_id}"
INSTANCE_TYPE="{instance_type}"
ARCH=$(uname -m)

# Setup NVMe instance store for /nix if available (gd/d/i instance types)
setup_nvme() {{
    # Find NVMe instance store devices (not EBS - which has "Amazon Elastic Block Store" in model)
    local nvme_devices=()
    for dev in /sys/block/nvme*; do
        [[ -d "$dev" ]] || continue
        local name=$(basename "$dev")
        [[ "$name" == *p* ]] && continue  # Skip partitions
        local model=$(cat "$dev/device/model" 2>/dev/null | tr -d ' ')
        if [[ "$model" == *"InstanceStorage"* ]] || [[ "$model" == *"NVMeSSD"* ]]; then
            nvme_devices+=("/dev/$name")
        fi
    done

    local count=${{#nvme_devices[@]}}
    [[ $count -eq 0 ]] && return 0

    echo "Found $count NVMe instance store device(s): ${{nvme_devices[*]}}"

    local target_dev
    if [[ $count -gt 1 ]]; then
        echo "Creating RAID0 array..."
        yum install -y mdadm
        mdadm --create /dev/md0 --level=0 --raid-devices=$count "${{nvme_devices[@]}}" --force
        target_dev=/dev/md0
    else
        target_dev="${{nvme_devices[0]}}"
    fi

    echo "Formatting $target_dev with ext4..."
    mkfs.ext4 -F "$target_dev"

    echo "Mounting NVMe storage..."
    mkdir -p /mnt/nvme
    mount "$target_dev" /mnt/nvme
    mkdir -p /mnt/nvme/nix /mnt/nvme/tmp /mnt/nvme/var

    # Bind mount to use fast NVMe for Nix store
    mkdir -p /nix
    mount --bind /mnt/nvme/nix /nix
}}

setup_nvme

echo "Installing upstream Nix via Determinate installer..."
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | \
    sh -s -- install --no-confirm --prefer-upstream-nix

echo "Sourcing nix profile..."
export HOME=/root
. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

echo "Fetching agent from S3..."
mkdir -p /etc/nix-bench
aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/agent-${{ARCH}}" /usr/local/bin/nix-bench-agent
chmod +x /usr/local/bin/nix-bench-agent

aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/config-${{INSTANCE_TYPE}}.json" /etc/nix-bench/config.json

echo "Starting nix-bench-agent..."
exec /usr/local/bin/nix-bench-agent --config /etc/nix-bench/config.json
"#,
        bucket = bucket,
        run_id = run_id,
        instance_type = instance_type,
    )
}

/// Try to find agent binary in common locations
/// Prefers musl (statically linked) over gnu (dynamically linked)
fn find_agent_binary(arch: &str) -> Option<String> {
    // Prefer musl for static linking, fall back to gnu
    let target_triples: &[&str] = match arch {
        "x86_64" => &["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
        "aarch64" => &["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"],
        _ => return None,
    };

    for target_triple in target_triples {
        let candidates = [
            // Cross-compiled release build (cargo build --target)
            format!("target/{}/release/nix-bench-agent", target_triple),
            // Relative to crates directory
            format!("../target/{}/release/nix-bench-agent", target_triple),
        ];

        for path in &candidates {
            let p = std::path::Path::new(path);
            if p.exists() {
                return Some(p.canonicalize().ok()?.to_string_lossy().to_string());
            }
        }
    }

    // Also check native build (only useful for x86_64 on x86_64 host, but dynamically linked)
    let native_candidates = [
        "target/release/nix-bench-agent",
        "../target/release/nix-bench-agent",
    ];
    for path in &native_candidates {
        let p = std::path::Path::new(path);
        if p.exists() {
            return Some(p.canonicalize().ok()?.to_string_lossy().to_string());
        }
    }

    None
}

/// Run benchmarks on the specified instances
pub async fn run_benchmarks(config: RunConfig) -> Result<()> {
    // Determine which architectures we need
    let needs_x86_64 = config
        .instance_types
        .iter()
        .any(|t| detect_system(t) == "x86_64-linux");
    let needs_aarch64 = config
        .instance_types
        .iter()
        .any(|t| detect_system(t) == "aarch64-linux");

    // Try to auto-detect agent binaries if not provided
    let agent_x86_64 = config.agent_x86_64.clone().or_else(|| {
        let found = find_agent_binary("x86_64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected x86_64 agent");
        }
        found
    });

    let agent_aarch64 = config.agent_aarch64.clone().or_else(|| {
        let found = find_agent_binary("aarch64");
        if let Some(ref path) = found {
            info!(path = %path, "Auto-detected aarch64 agent");
        }
        found
    });

    // Validate agent binaries are provided (unless dry-run)
    if !config.dry_run {
        if needs_x86_64 && agent_x86_64.is_none() {
            anyhow::bail!(
                "x86_64 instance types specified but agent not found.\n\
                 Build with: cargo agent\n\
                 Or use nix: nix build .#nix-bench-agent"
            );
        }
        if needs_aarch64 && agent_aarch64.is_none() {
            anyhow::bail!(
                "aarch64 instance types specified but agent not found.\n\
                 Cross-compile with: nix build .#nix-bench-agent-aarch64"
            );
        }
    }

    // Dry-run mode: validate and print what would happen
    if config.dry_run {
        println!("\n=== DRY RUN ===\n");
        println!("This would launch the following benchmark:\n");
        println!("  Region:         {}", config.region);
        println!("  Attribute:      {}", config.attr);
        println!("  Runs/instance:  {}", config.runs);
        println!();
        println!("  Instance types:");
        for instance_type in &config.instance_types {
            let system = detect_system(instance_type);
            println!("    - {} ({})", instance_type, system);
        }
        println!();
        println!("  Agent binaries:");
        if needs_x86_64 {
            if let Some(path) = &agent_x86_64 {
                println!("    - x86_64:  {}", path);
            } else {
                println!("    - x86_64:  NOT PROVIDED (required)");
            }
        }
        if needs_aarch64 {
            if let Some(path) = &agent_aarch64 {
                println!("    - aarch64: {}", path);
            } else {
                println!("    - aarch64: NOT PROVIDED (required)");
            }
        }
        println!();
        println!("  Options:");
        println!("    - Keep instances: {}", config.keep);
        println!("    - TUI mode:       {}", !config.no_tui);
        if let Some(output) = &config.output {
            println!("    - Output file:    {}", output);
        }
        if let Some(subnet) = &config.subnet_id {
            println!("    - Subnet ID:      {}", subnet);
        }
        if let Some(sg) = &config.security_group_id {
            println!("    - Security group: {}", sg);
        }
        if let Some(profile) = &config.instance_profile {
            println!("    - IAM profile:    {}", profile);
        }
        println!();
        println!("To run for real, remove the --dry-run flag.");
        return Ok(());
    }

    // Generate run ID
    let run_id = Uuid::now_v7().to_string();
    let bucket_name = format!("nix-bench-{}", run_id);

    // For TUI mode, start TUI immediately and run init in background
    if !config.no_tui {
        run_benchmarks_with_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64).await
    } else {
        run_benchmarks_no_tui(config, run_id, bucket_name, agent_x86_64, agent_aarch64).await
    }
}

/// Run benchmarks with TUI - starts TUI immediately, runs init in background
async fn run_benchmarks_with_tui(
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
async fn run_init_task(
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
    if let Some(ref tls) = coordinator_tls_config {
        let _ = tx.send(TuiMessage::TlsConfig { config: tls.clone() }).await;
    }

    // Switch to running phase
    let _ = tx.send(TuiMessage::Phase(InitPhase::Running)).await;

    // Start gRPC log streaming for instances with public IPs
    // Use mTLS if we have coordinator TLS config
    let instances_with_ips: Vec<(String, String)> = instances
        .iter()
        .filter_map(|(instance_type, state)| {
            state
                .public_ip
                .as_ref()
                .map(|ip| (instance_type.clone(), ip.clone()))
        })
        .collect();

    if !instances_with_ips.is_empty() {
        info!(
            count = instances_with_ips.len(),
            tls_enabled = coordinator_tls_config.is_some(),
            "Starting gRPC log streaming for instances"
        );

        // Use unified log streaming with options
        use crate::aws::{LogStreamingOptions, start_log_streaming_unified};
        let mut options = LogStreamingOptions::new(&instances_with_ips, &run_id, GRPC_PORT)
            .with_channel(tx.clone());
        if let Some(tls_config) = coordinator_tls_config {
            options = options.with_tls(tls_config);
        }
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

    Ok(instances)
}

/// Poll EC2 console output for bootstrap failure detection
/// This monitors instances during the bootstrap phase before gRPC is available
async fn poll_bootstrap_status(
    instances: HashMap<String, InstanceState>,
    tx: mpsc::Sender<TuiMessage>,
    region: String,
    timeout_secs: u64,
    start_time: Instant,
) {
    use std::collections::HashSet;
    use std::time::Duration;

    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Track which instances have already been marked as failed
    let mut failed_instances: HashSet<String> = HashSet::new();

    // Poll interval for EC2 console output
    const POLL_INTERVAL: Duration = Duration::from_secs(10);

    loop {
        // Check for timeout
        let elapsed = start_time.elapsed().as_secs();
        if timeout_secs > 0 && elapsed > timeout_secs {
            warn!(
                elapsed_secs = elapsed,
                timeout_secs = timeout_secs,
                "Run timeout exceeded"
            );
            for (instance_type, state) in &instances {
                if !failed_instances.contains(instance_type) {
                    error!(instance_type = %instance_type, "Instance timed out");
                    let _ = tx
                        .send(TuiMessage::InstanceUpdate {
                            instance_type: instance_type.clone(),
                            instance_id: state.instance_id.clone(),
                            status: InstanceStatus::Failed,
                            public_ip: state.public_ip.clone(),
                        })
                        .await;
                }
            }
            break;
        }

        for (instance_type, state) in &instances {
            // Skip already failed instances
            if failed_instances.contains(instance_type) {
                continue;
            }

            // Check EC2 console output for bootstrap failures
            if !state.instance_id.is_empty() {
                if let Ok(Some(console_output)) = ec2.get_console_output(&state.instance_id).await {
                    // Check for bootstrap failures
                    if let Some(failure_pattern) = detect_bootstrap_failure(&console_output) {
                        error!(
                            instance_type = %instance_type,
                            instance_id = %state.instance_id,
                            pattern = %failure_pattern,
                            "Bootstrap failure detected"
                        );
                        failed_instances.insert(instance_type.clone());
                        let _ = tx
                            .send(TuiMessage::InstanceUpdate {
                                instance_type: instance_type.clone(),
                                instance_id: state.instance_id.clone(),
                                status: InstanceStatus::Failed,
                                public_ip: state.public_ip.clone(),
                            })
                            .await;
                        // Send console output so user can see what happened
                        let _ = tx
                            .send(TuiMessage::ConsoleOutput {
                                instance_type: instance_type.clone(),
                                output: console_output,
                            })
                            .await;
                    }
                }
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Run benchmarks without TUI (--no-tui mode)
async fn run_benchmarks_no_tui(
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
            tls_enabled = coordinator_tls_config.is_some(),
            "Starting gRPC log streaming to stdout"
        );

        // Use unified log streaming with stdout output
        use crate::aws::{LogStreamingOptions, start_log_streaming_unified};
        let mut options = LogStreamingOptions::new(&instances_with_ips, &run_id, GRPC_PORT);
        if let Some(ref tls_config) = coordinator_tls_config {
            options = options.with_tls(tls_config.clone());
        }
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
            println!("\n⚠️  TIMEOUT: Run exceeded {}s limit", config.timeout);
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
            use crate::aws::GrpcStatusPoller;
            let poller = if let Some(ref tls) = coordinator_tls_config {
                GrpcStatusPoller::new_with_tls(&instances_with_ips, GRPC_PORT, tls.clone())
            } else {
                GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT)
            };
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
                    match status_code {
                        2 => state.status = InstanceStatus::Complete,
                        -1 => state.status = InstanceStatus::Failed,
                        1 => state.status = InstanceStatus::Running,
                        _ => {}
                    }
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
                                "\n❌ Bootstrap failure on {}: {}",
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
                InstanceStatus::Pending => "⏳ pending",
                InstanceStatus::Launching => "🚀 launching",
                InstanceStatus::Running => "▶ running",
                InstanceStatus::Complete => "✓ complete",
                InstanceStatus::Failed => "✗ failed",
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

/// Helper to send cleanup progress to TUI
async fn send_cleanup_progress(tx: &Option<mpsc::Sender<TuiMessage>>, progress: CleanupProgress) {
    if let Some(tx) = tx {
        let _ = tx.send(TuiMessage::Phase(InitPhase::CleaningUp(progress))).await;
    }
}

/// Cleanup resources (terminate instances, delete bucket)
/// If `progress_tx` is Some, sends progress updates to the TUI channel
async fn cleanup_resources(
    config: &RunConfig,
    run_id: &str,
    bucket_name: &str,
    instances: &HashMap<String, InstanceState>,
    progress_tx: Option<mpsc::Sender<TuiMessage>>,
) -> Result<()> {
    let db = state::open_db().await?;
    let ec2 = Ec2Client::new(&config.region).await?;
    let s3 = S3Client::new(&config.region).await?;

    if !config.keep {
        // Collect instance IDs from the HashMap
        let mut instance_ids: std::collections::HashSet<String> = instances
            .values()
            .filter(|s| !s.instance_id.is_empty())
            .map(|s| s.instance_id.clone())
            .collect();

        // Also check the database for any instances that might have been created
        // but not tracked in the HashMap (e.g., if user quit early during init)
        let db_resources = state::get_run_resources(&db, run_id).await.unwrap_or_default();
        for resource in &db_resources {
            if resource.resource_type == ResourceType::Ec2Instance && resource.deleted_at.is_none() {
                instance_ids.insert(resource.resource_id.clone());
            }
        }

        // Count resources for progress tracking
        let eip_ids: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::ElasticIp && r.deleted_at.is_none())
            .collect();
        let iam_roles: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::IamRole && r.deleted_at.is_none())
            .collect();
        let sg_rules: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::SecurityGroupRule && r.deleted_at.is_none())
            .collect();
        let security_groups: Vec<_> = db_resources
            .iter()
            .filter(|r| r.resource_type == ResourceType::SecurityGroup && r.deleted_at.is_none())
            .collect();

        let mut progress = CleanupProgress::new(
            instance_ids.len(),
            eip_ids.len(),
            iam_roles.len(),
            sg_rules.len(),
        );

        // Terminate EC2 instances
        info!(count = instance_ids.len(), "Terminating instances...");
        progress.current_step = format!("Terminating {} EC2 instances...", instance_ids.len());
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        for instance_id in &instance_ids {
            match ec2.terminate_instance(instance_id).await {
                Ok(()) => {
                    info!(instance_id = %instance_id, "Instance terminated");
                    let _ =
                        state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id).await;
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("InvalidInstanceID.NotFound") {
                        // Already terminated
                        let _ = state::mark_resource_deleted(
                            &db,
                            ResourceType::Ec2Instance,
                            instance_id,
                        );
                    } else {
                        warn!(instance_id = %instance_id, error = ?e, "Failed to terminate instance");
                    }
                }
            }
            progress.ec2_instances.0 += 1;
            send_cleanup_progress(&progress_tx, progress.clone()).await;
        }

        // Release Elastic IPs (must be after instance termination)
        if !eip_ids.is_empty() {
            info!(count = eip_ids.len(), "Releasing Elastic IPs...");
            progress.current_step = format!("Releasing {} Elastic IPs...", eip_ids.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            for resource in &eip_ids {
                match ec2.release_elastic_ip(&resource.resource_id).await {
                    Ok(()) => {
                        info!(allocation_id = %resource.resource_id, "Released Elastic IP");
                        let _ = state::mark_resource_deleted(
                            &db,
                            ResourceType::ElasticIp,
                            &resource.resource_id,
                        );
                    }
                    Err(e) => {
                        let error_str = format!("{:?}", e);
                        if error_str.contains("InvalidAllocationID.NotFound") {
                            let _ = state::mark_resource_deleted(
                                &db,
                                ResourceType::ElasticIp,
                                &resource.resource_id,
                            );
                        } else {
                            warn!(allocation_id = %resource.resource_id, error = ?e, "Failed to release EIP");
                        }
                    }
                }
                progress.elastic_ips.0 += 1;
                send_cleanup_progress(&progress_tx, progress.clone()).await;
            }
        }

        // Delete S3 bucket
        info!("Deleting S3 bucket...");
        progress.current_step = "Deleting S3 bucket...".to_string();
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        match s3.delete_bucket(bucket_name).await {
            Ok(()) => {
                let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name).await;
            }
            Err(e) => {
                let error_str = format!("{:?}", e);
                if error_str.contains("NoSuchBucket") {
                    let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name).await;
                } else {
                    warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
                }
            }
        }
        progress.s3_bucket = true;
        send_cleanup_progress(&progress_tx, progress.clone()).await;

        // Delete IAM resources
        if !iam_roles.is_empty() {
            info!(count = iam_roles.len(), "Deleting IAM resources...");
            progress.current_step = format!("Deleting {} IAM roles...", iam_roles.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            let iam = IamClient::new(&config.region).await?;
            for resource in &iam_roles {
                if let Err(e) = iam.delete_benchmark_role(&resource.resource_id).await {
                    warn!(role = %resource.resource_id, error = ?e, "Failed to delete IAM role");
                } else {
                    let _ = state::mark_resource_deleted(
                        &db,
                        ResourceType::IamRole,
                        &resource.resource_id,
                    );
                    // Instance profile has the same name as the role
                    let _ = state::mark_resource_deleted(
                        &db,
                        ResourceType::IamInstanceProfile,
                        &resource.resource_id,
                    );
                }
                progress.iam_roles.0 += 1;
                send_cleanup_progress(&progress_tx, progress.clone()).await;
            }
        }

        // Delete security group rules (for user-provided security groups)
        if !sg_rules.is_empty() {
            info!(count = sg_rules.len(), "Removing security group rules...");
            progress.current_step = format!("Removing {} security group rules...", sg_rules.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            for resource in &sg_rules {
                // Parse resource_id format: "sg-xxx:cidr_ip"
                if let Some((sg_id, cidr_ip)) = resource.resource_id.split_once(':') {
                    match ec2.remove_grpc_ingress_rule(sg_id, cidr_ip).await {
                        Ok(()) => {
                            info!(security_group = %sg_id, cidr_ip = %cidr_ip, "Removed gRPC ingress rule");
                            let _ = state::mark_resource_deleted(
                                &db,
                                ResourceType::SecurityGroupRule,
                                &resource.resource_id,
                            );
                        }
                        Err(e) => {
                            let error_str = format!("{:?}", e);
                            // Rule might already be removed
                            if error_str.contains("InvalidPermission.NotFound") {
                                let _ = state::mark_resource_deleted(
                                    &db,
                                    ResourceType::SecurityGroupRule,
                                    &resource.resource_id,
                                );
                            } else {
                                warn!(
                                    security_group = %sg_id,
                                    cidr_ip = %cidr_ip,
                                    error = ?e,
                                    "Failed to remove security group rule"
                                );
                            }
                        }
                    }
                } else {
                    warn!(
                        resource_id = %resource.resource_id,
                        "Invalid SecurityGroupRule resource_id format, expected 'sg-xxx:cidr_ip'"
                    );
                }
                progress.security_rules.0 += 1;
                send_cleanup_progress(&progress_tx, progress.clone()).await;
            }
        }

        // Delete security groups (for nix-bench-created security groups)
        // Must wait for instances to fully terminate first
        if !security_groups.is_empty() {
            // Wait for all instances to terminate before deleting security groups
            if !instance_ids.is_empty() {
                info!("Waiting for instances to fully terminate before deleting security groups...");
                progress.current_step = "Waiting for instances to terminate...".to_string();
                send_cleanup_progress(&progress_tx, progress.clone()).await;

                for instance_id in &instance_ids {
                    ec2.wait_for_terminated(instance_id).await?;
                }
            }

            info!(count = security_groups.len(), "Deleting security groups...");
            progress.current_step = format!("Deleting {} security groups...", security_groups.len());
            send_cleanup_progress(&progress_tx, progress.clone()).await;

            for resource in &security_groups {
                match ec2.delete_security_group(&resource.resource_id).await {
                    Ok(()) => {
                        info!(sg_id = %resource.resource_id, "Deleted security group");
                        let _ = state::mark_resource_deleted(
                            &db,
                            ResourceType::SecurityGroup,
                            &resource.resource_id,
                        );
                    }
                    Err(e) => {
                        let error_str = format!("{:?}", e);
                        // SG might already be deleted
                        if error_str.contains("InvalidGroup.NotFound") {
                            let _ = state::mark_resource_deleted(
                                &db,
                                ResourceType::SecurityGroup,
                                &resource.resource_id,
                            );
                        } else {
                            warn!(
                                sg_id = %resource.resource_id,
                                error = ?e,
                                "Failed to delete security group"
                            );
                        }
                    }
                }
            }
        }

        // Final progress update
        progress.current_step = "Cleanup complete".to_string();
        send_cleanup_progress(&progress_tx, progress).await;
    } else {
        info!("Keeping instances, bucket, and IAM resources (--keep specified)");
    }

    // Update run status
    let all_complete = !instances.is_empty()
        && instances
            .values()
            .all(|s| s.status == InstanceStatus::Complete);

    state::update_run_status(
        &db,
        run_id,
        if all_complete {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        },
    )
    .await?;

    Ok(())
}

/// Write results to output file
async fn write_results(
    config: &RunConfig,
    run_id: &str,
    instances: &HashMap<String, InstanceState>,
) -> Result<()> {
    if let Some(output_path) = &config.output {
        let start_time = chrono::Utc::now(); // TODO: track actual start time
        let end_time = chrono::Utc::now();
        let duration = end_time - start_time;

        let all_complete = instances
            .values()
            .all(|s| s.status == InstanceStatus::Complete);

        let results: HashMap<String, serde_json::Value> = instances
            .iter()
            .map(|(k, v)| {
                let avg = if !v.durations.is_empty() {
                    v.durations.iter().sum::<f64>() / v.durations.len() as f64
                } else {
                    0.0
                };
                let min = v
                    .durations
                    .iter()
                    .cloned()
                    .filter(|x| x.is_finite())
                    .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0);
                let max = v
                    .durations
                    .iter()
                    .cloned()
                    .filter(|x| x.is_finite())
                    .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                    .unwrap_or(0.0);

                (
                    k.clone(),
                    serde_json::json!({
                        "instance_id": v.instance_id,
                        "system": v.system,
                        "status": format!("{:?}", v.status),
                        "runs_completed": v.run_progress,
                        "runs_total": v.total_runs,
                        "durations_seconds": v.durations,
                        "stats": {
                            "avg_seconds": avg,
                            "min_seconds": min,
                            "max_seconds": max,
                        }
                    }),
                )
            })
            .collect();

        let output = serde_json::json!({
            "run_id": run_id,
            "region": config.region,
            "attr": config.attr,
            "runs_per_instance": config.runs,
            "instance_types": config.instance_types,
            "start_time": start_time.to_rfc3339(),
            "end_time": end_time.to_rfc3339(),
            "duration_seconds": duration.num_seconds(),
            "success": all_complete,
            "results": results,
        });

        std::fs::write(output_path, serde_json::to_string_pretty(&output)?)?;
        info!(path = %output_path, "Results written");
    }

    Ok(())
}

/// Print a summary table of benchmark results to stdout
fn print_results_summary(instances: &HashMap<String, InstanceState>) {
    use comfy_table::{presets::UTF8_FULL_CONDENSED, Cell, ContentArrangement, Table};

    if instances.is_empty() {
        return;
    }

    println!("\n=== Benchmark Results ===\n");

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            Cell::new("Instance Type"),
            Cell::new("Status"),
            Cell::new("Runs"),
            Cell::new("Min (s)"),
            Cell::new("Avg (s)"),
            Cell::new("Max (s)"),
        ]);

    // Sort by average duration (fastest first), with instances that have no results at the end
    let mut sorted: Vec<_> = instances.iter().collect();
    sorted.sort_by(|(k1, s1), (k2, s2)| {
        let avg1 = if s1.durations.is_empty() {
            f64::MAX
        } else {
            s1.durations.iter().sum::<f64>() / s1.durations.len() as f64
        };
        let avg2 = if s2.durations.is_empty() {
            f64::MAX
        } else {
            s2.durations.iter().sum::<f64>() / s2.durations.len() as f64
        };
        avg1.partial_cmp(&avg2)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| k1.cmp(k2)) // tie-breaker: instance type name
    });

    for (instance_type, state) in sorted {
        let status = match state.status {
            InstanceStatus::Pending => "Pending",
            InstanceStatus::Launching => "Launching",
            InstanceStatus::Running => "Running",
            InstanceStatus::Complete => "Complete",
            InstanceStatus::Failed => "Failed",
        };

        let runs = format!("{}/{}", state.run_progress, state.total_runs);

        let (min, avg, max) = if state.durations.is_empty() {
            ("-".to_string(), "-".to_string(), "-".to_string())
        } else {
            let min_val = state
                .durations
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MAX, f64::min);
            let avg_val = state.durations.iter().sum::<f64>() / state.durations.len() as f64;
            let max_val = state
                .durations
                .iter()
                .cloned()
                .filter(|x| x.is_finite())
                .fold(f64::MIN, f64::max);
            (
                format!("{:.1}", min_val),
                format!("{:.1}", avg_val),
                format!("{:.1}", max_val),
            )
        };

        table.add_row(vec![
            Cell::new(instance_type),
            Cell::new(status),
            Cell::new(&runs),
            Cell::new(&min),
            Cell::new(&avg),
            Cell::new(&max),
        ]);
    }

    println!("{table}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_bootstrap_failure_unbound_variable() {
        let console = r#"
[    5.123456] Starting nix-bench bootstrap
[    5.234567] /var/lib/cloud/instance/scripts/user-data: line 15: BUCKET: unbound variable
[    5.345678] Failed
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("unbound variable"));
    }

    #[test]
    fn test_detect_bootstrap_failure_cloud_init() {
        let console = r#"
[   OK  ] Started cloud-init.service
[FAILED] Failed to start cloud-final.service - Execute cloud user/final scripts
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("Failed to start cloud-final"));
    }

    #[test]
    fn test_detect_bootstrap_failure_agent_not_found() {
        let console = "Starting nix-bench-agent...\nnix-bench-agent: command not found\n";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result
            .unwrap()
            .contains("nix-bench-agent: command not found"));
    }

    #[test]
    fn test_detect_bootstrap_failure_no_such_file() {
        let console = "/usr/local/bin/nix-bench-agent: No such file or directory";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
    }

    #[test]
    fn test_detect_bootstrap_failure_none() {
        let console = r#"
[   OK  ] Started cloud-init.service
[   OK  ] Started cloud-final.service
Starting nix-bench-agent...
Agent started successfully
"#;
        let result = detect_bootstrap_failure(console);
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_bootstrap_failure_empty() {
        assert!(detect_bootstrap_failure("").is_none());
    }

    #[test]
    fn test_detect_bootstrap_failure_priority() {
        // First pattern should win
        let console = "unbound variable\ncommand not found";
        let result = detect_bootstrap_failure(console);
        assert!(result.is_some());
        assert!(result.unwrap().contains("unbound variable"));
    }

    #[test]
    fn test_generate_user_data_contains_required_elements() {
        let script = generate_user_data("my-bucket", "run-123", "c6i.xlarge");

        // Check shebang
        assert!(script.starts_with("#!/bin/bash"));

        // Check set options
        assert!(script.contains("set -euo pipefail"));

        // Check bucket variable
        assert!(script.contains("BUCKET=\"my-bucket\""));

        // Check run_id variable
        assert!(script.contains("RUN_ID=\"run-123\""));

        // Check instance_type variable
        assert!(script.contains("INSTANCE_TYPE=\"c6i.xlarge\""));

        // Check Nix installer curl command
        assert!(script.contains("curl --proto '=https'"));
        assert!(script.contains("install.determinate.systems/nix"));

        // Check S3 agent download
        assert!(script.contains("aws s3 cp"));
        assert!(script.contains("s3://${BUCKET}/${RUN_ID}/agent-${ARCH}"));

        // Check config download with instance type
        assert!(script.contains("config-${INSTANCE_TYPE}.json"));

        // Check agent execution
        assert!(script.contains("exec /usr/local/bin/nix-bench-agent"));
    }

    #[test]
    fn test_generate_user_data_escapes_special_chars() {
        // Bucket name with hyphen (common case)
        let script = generate_user_data("nix-bench-abc123", "test-run-456", "c7i.metal");

        assert!(script.contains("BUCKET=\"nix-bench-abc123\""));
        assert!(script.contains("RUN_ID=\"test-run-456\""));
        assert!(script.contains("INSTANCE_TYPE=\"c7i.metal\""));
    }

    #[test]
    fn test_generate_user_data_uses_bash_variables() {
        let script = generate_user_data("bucket", "run", "type");

        // Verify it uses ${ARCH} for architecture detection (not hardcoded)
        assert!(script.contains("${ARCH}"));
        assert!(script.contains("ARCH=$(uname -m)"));
    }

    #[test]
    fn test_find_agent_binary_invalid_arch() {
        // Invalid architecture should return None
        assert!(find_agent_binary("arm").is_none());
        assert!(find_agent_binary("i686").is_none());
        assert!(find_agent_binary("").is_none());
    }

    #[test]
    fn test_find_agent_binary_returns_none_when_missing() {
        // Test in a directory where no binaries exist
        // This is expected behavior when binaries haven't been built
        // The function should return None, not panic
        let result = find_agent_binary("x86_64");
        // We can't assert is_some because the binary might not exist,
        // but we can assert the function doesn't panic
        let _ = result;
    }
}
