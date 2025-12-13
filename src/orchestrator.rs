//! Main orchestration logic for benchmark runs

use crate::aws::{CloudWatchClient, Ec2Client, IamClient, S3Client};
use crate::config::{detect_system, AgentConfig, RunConfig};
use crate::state::{self, ResourceType, RunStatus};
use crate::tui::{self, InitPhase, TuiMessage};
use anyhow::Result;
use std::collections::HashMap;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

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

/// Instance state during a run
#[derive(Debug, Clone)]
pub struct InstanceState {
    pub instance_id: String,
    pub instance_type: String,
    pub system: String,
    pub status: InstanceStatus,
    pub run_progress: u32,
    pub total_runs: u32,
    pub durations: Vec<f64>,
    pub public_ip: Option<String>,
    pub console_output: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum InstanceStatus {
    Pending,
    Launching,
    Running,
    Complete,
    Failed,
}

/// Generate user-data script for an instance
fn generate_user_data(bucket: &str, run_id: &str) -> String {
    format!(
        r#"#!/bin/bash
set -euo pipefail

exec > >(tee /var/log/nix-bench-bootstrap.log) 2>&1

echo "Starting nix-bench bootstrap"

BUCKET="{bucket}"
RUN_ID="{run_id}"
ARCH=$(uname -m)

echo "Installing Nix via Determinate Systems installer..."
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | \
    sh -s -- install --no-confirm --nix-package-url "https://releases.nixos.org/nix/nix-2.24.10/nix-2.24.10-${{ARCH}}-linux.tar.xz"

echo "Sourcing nix profile..."
export HOME=/root
. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh

echo "Fetching agent from S3..."
mkdir -p /etc/nix-bench
aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/agent-${{ARCH}}" /usr/local/bin/nix-bench-agent
chmod +x /usr/local/bin/nix-bench-agent

aws s3 cp "s3://${{BUCKET}}/${{RUN_ID}}/config-${{ARCH}}.json" /etc/nix-bench/config.json

echo "Starting nix-bench-agent..."
exec /usr/local/bin/nix-bench-agent --config /etc/nix-bench/config.json
"#,
        bucket = bucket,
        run_id = run_id,
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
        run_benchmarks_with_tui(
            config,
            run_id,
            bucket_name,
            agent_x86_64,
            agent_aarch64,
        )
        .await
    } else {
        run_benchmarks_no_tui(
            config,
            run_id,
            bucket_name,
            agent_x86_64,
            agent_aarch64,
        )
        .await
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
    let (tx, rx) = mpsc::channel::<TuiMessage>(100);

    // Setup terminal FIRST
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Build instance_type -> system mapping for CloudWatch metrics
    let instance_systems: HashMap<String, String> = config
        .instance_types
        .iter()
        .map(|it| (it.clone(), detect_system(it).to_string()))
        .collect();

    // Create CloudWatch client with system mapping
    let cloudwatch = CloudWatchClient::new(&config.region, &run_id, instance_systems).await?;

    // Create empty instances map - will be filled by background task
    let mut instances: HashMap<String, InstanceState> = HashMap::new();

    // Create app state in loading mode
    let mut app = tui::App::new_loading(&config.instance_types, config.runs);

    // Clone what we need for the background task
    let config_clone = config.clone();
    let run_id_clone = run_id.clone();
    let bucket_name_clone = bucket_name.clone();
    let tx_clone = tx.clone();

    // Spawn background initialization task
    let init_handle = tokio::spawn(async move {
        run_init_task(
            config_clone,
            run_id_clone,
            bucket_name_clone,
            agent_x86_64,
            agent_aarch64,
            tx_clone,
        )
        .await
    });

    // Run the TUI with channel
    let tui_result = app.run_with_channel(&mut terminal, &cloudwatch, &config, &mut tokio::sync::mpsc::Receiver::from(rx)).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Wait for init task and get result
    let init_result = init_handle.await?;

    // Update instances from app state
    for (instance_type, state) in &app.instances {
        instances.insert(instance_type.clone(), state.clone());
    }

    // Handle cleanup and results
    if let Err(e) = &init_result {
        error!(error = ?e, "Initialization failed");
    }

    // Do cleanup
    cleanup_resources(&config, &run_id, &bucket_name, &instances).await?;

    // Write output if requested
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
) -> Result<HashMap<String, InstanceState>> {
    // Send initial phase
    let _ = tx.send(TuiMessage::Phase(InitPhase::Starting)).await;
    let _ = tx
        .send(TuiMessage::RunInfo {
            run_id: run_id.clone(),
            bucket_name: bucket_name.clone(),
        })
        .await;

    info!(run_id = %run_id, bucket = %bucket_name, "Starting benchmark run");

    // Open state database
    let db = state::open_db()?;
    state::insert_run(&db, &run_id, &config.region, &config.instance_types, &config.attr)?;

    // Initialize AWS clients
    let ec2 = Ec2Client::new(&config.region).await?;
    let s3 = S3Client::new(&config.region).await?;

    // Create S3 bucket
    let _ = tx.send(TuiMessage::Phase(InitPhase::CreatingBucket)).await;
    info!("Creating S3 bucket for run artifacts");
    s3.create_bucket(&bucket_name).await?;
    state::insert_resource(&db, &run_id, ResourceType::S3Bucket, &bucket_name, &config.region)?;

    // Create IAM role and instance profile (if not provided)
    let instance_profile_name = if config.instance_profile.is_some() {
        config.instance_profile.clone()
    } else {
        let _ = tx.send(TuiMessage::Phase(InitPhase::CreatingIamRole)).await;
        info!("Creating IAM role and instance profile for agent");
        let iam = IamClient::new(&config.region).await?;
        let (role_name, profile_name) = iam.create_benchmark_role(&run_id, &bucket_name).await?;

        // Track the IAM resources
        state::insert_resource(&db, &run_id, ResourceType::IamRole, &role_name, &config.region)?;
        state::insert_resource(&db, &run_id, ResourceType::IamInstanceProfile, &profile_name, &config.region)?;

        Some(profile_name)
    };

    // Upload agent binaries
    let _ = tx.send(TuiMessage::Phase(InitPhase::UploadingAgents)).await;
    if let Some(agent_path) = &agent_x86_64 {
        let key = format!("{}/agent-x86_64", run_id);
        info!(path = %agent_path, key = %key, "Uploading x86_64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }
    if let Some(agent_path) = &agent_aarch64 {
        let key = format!("{}/agent-aarch64", run_id);
        info!(path = %agent_path, key = %key, "Uploading aarch64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }

    // Upload configs
    let mut uploaded_configs = std::collections::HashSet::new();
    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
        let arch = if system == "aarch64-linux" { "aarch64" } else { "x86_64" };

        if uploaded_configs.contains(arch) {
            continue;
        }
        uploaded_configs.insert(arch.to_string());

        let agent_config = AgentConfig {
            run_id: run_id.clone(),
            bucket: bucket_name.clone(),
            region: config.region.clone(),
            attr: config.attr.clone(),
            runs: config.runs,
            instance_type: instance_type.clone(),
            system: system.to_string(),
            flake_ref: config.flake_ref.clone(),
            build_timeout: config.build_timeout,
            max_failures: config.max_failures,
        };

        let config_json = serde_json::to_string_pretty(&agent_config)?;
        let key = format!("{}/config-{}.json", run_id, arch);
        s3.upload_bytes(&bucket_name, &key, config_json.into_bytes(), "application/json")
            .await?;
    }

    // Launch instances
    let _ = tx.send(TuiMessage::Phase(InitPhase::LaunchingInstances)).await;
    let mut instances: HashMap<String, InstanceState> = HashMap::new();

    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
        let user_data = generate_user_data(&bucket_name, &run_id);

        match ec2
            .launch_instance(
                &run_id,
                instance_type,
                system,
                &user_data,
                config.subnet_id.as_deref(),
                config.security_group_id.as_deref(),
                instance_profile_name.as_deref(),
            )
            .await
        {
            Ok(launched) => {
                state::insert_resource(
                    &db,
                    &run_id,
                    ResourceType::Ec2Instance,
                    &launched.instance_id,
                    &config.region,
                )?;

                let _ = tx
                    .send(TuiMessage::InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: launched.instance_id.clone(),
                        status: InstanceStatus::Launching,
                        public_ip: None,

                    })
                    .await;

                instances.insert(
                    instance_type.clone(),
                    InstanceState {
                        instance_id: launched.instance_id,
                        instance_type: instance_type.clone(),
                        system: system.to_string(),
                        status: InstanceStatus::Launching,
                        run_progress: 0,
                        total_runs: config.runs,
                        durations: Vec::new(),
                        public_ip: None,
                        console_output: None,
                    },
                );
            }
            Err(e) => {
                error!(instance_type = %instance_type, error = ?e, "Failed to launch instance");
                let _ = tx
                    .send(TuiMessage::InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: String::new(),
                        status: InstanceStatus::Failed,
                        public_ip: None,

                    })
                    .await;
            }
        }
    }

    if instances.is_empty() {
        let _ = tx
            .send(TuiMessage::Phase(InitPhase::Failed(
                "No instances launched".to_string(),
            )))
            .await;
        anyhow::bail!("No instances were launched successfully");
    }

    // Wait for instances to be running
    let _ = tx.send(TuiMessage::Phase(InitPhase::WaitingForInstances)).await;
    for (instance_type, state) in instances.iter_mut() {
        match ec2.wait_for_running(&state.instance_id, None).await {
            Ok(public_ip) => {
                state.public_ip = public_ip.clone();
                state.status = InstanceStatus::Running;
                let _ = tx
                    .send(TuiMessage::InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: state.instance_id.clone(),
                        status: InstanceStatus::Running,
                        public_ip,
                    })
                    .await;
            }
            Err(e) => {
                error!(instance_type = %instance_type, error = ?e, "Failed waiting for instance");
                state.status = InstanceStatus::Failed;
                let _ = tx
                    .send(TuiMessage::InstanceUpdate {
                        instance_type: instance_type.clone(),
                        instance_id: state.instance_id.clone(),
                        status: InstanceStatus::Failed,
                        public_ip: None,

                    })
                    .await;
            }
        }
    }

    // Switch to running phase
    let _ = tx.send(TuiMessage::Phase(InitPhase::Running)).await;

    // Spawn a task to poll CloudWatch Logs and detect bootstrap failures
    let instances_for_logs = instances.clone();
    let tx_logs = tx.clone();
    let region = config.region.clone();
    let run_id_for_logs = run_id.clone();
    let timeout_secs = config.timeout;
    let start_time = Instant::now();
    tokio::spawn(async move {
        poll_instance_output(instances_for_logs, tx_logs, region, run_id_for_logs, timeout_secs, start_time).await;
    });

    Ok(instances)
}

/// Poll CloudWatch Logs and EC2 console output for all instances
/// Detects bootstrap failures from console output
async fn poll_instance_output(
    instances: HashMap<String, InstanceState>,
    tx: mpsc::Sender<TuiMessage>,
    region: String,
    run_id: String,
    timeout_secs: u64,
    start_time: Instant,
) {
    use crate::aws::logs::LogsError;
    use crate::aws::LogsClient;
    use std::collections::HashSet;
    use std::time::Duration;
    use tracing::debug;

    let logs = match LogsClient::new(&region, &run_id).await {
        Ok(c) => c,
        Err(_) => return,
    };

    let ec2 = match Ec2Client::new(&region).await {
        Ok(c) => c,
        Err(_) => return,
    };

    // Track which instances have already been marked as failed
    let mut failed_instances: HashSet<String> = HashSet::new();

    // Exponential backoff for rate limiting
    let mut backoff = Duration::from_secs(5);
    const MIN_BACKOFF: Duration = Duration::from_secs(5);
    const MAX_BACKOFF: Duration = Duration::from_secs(60);

    loop {
        // Check for timeout
        let elapsed = start_time.elapsed().as_secs();
        if timeout_secs > 0 && elapsed > timeout_secs {
            warn!(elapsed_secs = elapsed, timeout_secs = timeout_secs, "Run timeout exceeded");
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

        let mut any_rate_limited = false;

        for (instance_type, state) in &instances {
            // Skip already failed instances
            if failed_instances.contains(instance_type) {
                continue;
            }

            // Try CloudWatch Logs first (agent is running)
            match logs.get_recent_logs(instance_type, 50).await {
                Ok(output) if !output.is_empty() => {
                    // Success - reset backoff and send update
                    backoff = MIN_BACKOFF;
                    let _ = tx
                        .send(TuiMessage::ConsoleOutput {
                            instance_type: instance_type.clone(),
                            output,
                        })
                        .await;
                    continue; // Agent is running, skip console check
                }
                Ok(_) => {
                    // Empty logs - likely agent hasn't started yet
                }
                Err(LogsError::NotFound) => {
                    // Expected during bootstrap - agent hasn't created log stream yet
                    debug!(instance_type = %instance_type, "Log stream not found, checking console output");
                }
                Err(LogsError::RateLimited) => {
                    // Rate limited - increase backoff
                    any_rate_limited = true;
                    debug!(instance_type = %instance_type, backoff = ?backoff, "CloudWatch Logs rate limited");
                }
                Err(LogsError::Sdk(e)) => {
                    // Other SDK error - log but continue
                    warn!(instance_type = %instance_type, error = ?e, "CloudWatch Logs SDK error");
                }
            }

            // Fall back to EC2 console output (for bootstrap phase)
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

        // Adjust backoff based on rate limiting
        if any_rate_limited {
            backoff = std::cmp::min(backoff * 2, MAX_BACKOFF);
            info!(backoff = ?backoff, "Rate limited, increasing backoff");
        } else {
            backoff = MIN_BACKOFF;
        }

        tokio::time::sleep(backoff).await;
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
    info!(run_id = %run_id, bucket = %bucket_name, "Starting benchmark run");

    // Open state database
    let db = state::open_db()?;
    state::insert_run(&db, &run_id, &config.region, &config.instance_types, &config.attr)?;

    // Initialize AWS clients
    let ec2 = Ec2Client::new(&config.region).await?;
    let s3 = S3Client::new(&config.region).await?;

    // Build instance_type -> system mapping for CloudWatch metrics
    let instance_systems: HashMap<String, String> = config
        .instance_types
        .iter()
        .map(|it| (it.clone(), detect_system(it).to_string()))
        .collect();
    let cloudwatch = CloudWatchClient::new(&config.region, &run_id, instance_systems).await?;

    // Create S3 bucket
    info!("Creating S3 bucket for run artifacts");
    s3.create_bucket(&bucket_name).await?;
    state::insert_resource(&db, &run_id, ResourceType::S3Bucket, &bucket_name, &config.region)?;

    // Create IAM role and instance profile (if not provided)
    let instance_profile_name = if config.instance_profile.is_some() {
        config.instance_profile.clone()
    } else {
        info!("Creating IAM role and instance profile for agent");
        let iam = IamClient::new(&config.region).await?;
        let (role_name, profile_name) = iam.create_benchmark_role(&run_id, &bucket_name).await?;

        // Track the IAM resources
        state::insert_resource(&db, &run_id, ResourceType::IamRole, &role_name, &config.region)?;
        state::insert_resource(&db, &run_id, ResourceType::IamInstanceProfile, &profile_name, &config.region)?;

        Some(profile_name)
    };

    // Upload agent binaries
    if let Some(agent_path) = &agent_x86_64 {
        let key = format!("{}/agent-x86_64", run_id);
        info!(path = %agent_path, key = %key, "Uploading x86_64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }
    if let Some(agent_path) = &agent_aarch64 {
        let key = format!("{}/agent-aarch64", run_id);
        info!(path = %agent_path, key = %key, "Uploading aarch64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }

    // Upload configs
    let mut uploaded_configs = std::collections::HashSet::new();
    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
        let arch = if system == "aarch64-linux" { "aarch64" } else { "x86_64" };

        if uploaded_configs.contains(arch) {
            continue;
        }
        uploaded_configs.insert(arch.to_string());

        let agent_config = AgentConfig {
            run_id: run_id.clone(),
            bucket: bucket_name.clone(),
            region: config.region.clone(),
            attr: config.attr.clone(),
            runs: config.runs,
            instance_type: instance_type.clone(),
            system: system.to_string(),
            flake_ref: config.flake_ref.clone(),
            build_timeout: config.build_timeout,
            max_failures: config.max_failures,
        };

        let config_json = serde_json::to_string_pretty(&agent_config)?;
        let key = format!("{}/config-{}.json", run_id, arch);
        s3.upload_bytes(&bucket_name, &key, config_json.into_bytes(), "application/json")
            .await?;
    }

    // Launch instances
    let mut instances: HashMap<String, InstanceState> = HashMap::new();

    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
        let user_data = generate_user_data(&bucket_name, &run_id);

        match ec2
            .launch_instance(
                &run_id,
                instance_type,
                system,
                &user_data,
                config.subnet_id.as_deref(),
                config.security_group_id.as_deref(),
                instance_profile_name.as_deref(),
            )
            .await
        {
            Ok(launched) => {
                state::insert_resource(
                    &db,
                    &run_id,
                    ResourceType::Ec2Instance,
                    &launched.instance_id,
                    &config.region,
                )?;

                instances.insert(
                    instance_type.clone(),
                    InstanceState {
                        instance_id: launched.instance_id,
                        instance_type: instance_type.clone(),
                        system: system.to_string(),
                        status: InstanceStatus::Launching,
                        run_progress: 0,
                        total_runs: config.runs,
                        durations: Vec::new(),
                        public_ip: None,
                        console_output: None,
                    },
                );
            }
            Err(e) => {
                error!(instance_type = %instance_type, error = ?e, "Failed to launch instance");
            }
        }
    }

    if instances.is_empty() {
        anyhow::bail!("No instances were launched successfully");
    }

    // Wait for instances to be running
    for (instance_type, state) in instances.iter_mut() {
        match ec2.wait_for_running(&state.instance_id, None).await {
            Ok(public_ip) => {
                state.public_ip = public_ip;
                state.status = InstanceStatus::Running;
            }
            Err(e) => {
                error!(instance_type = %instance_type, error = ?e, "Failed waiting for instance");
                state.status = InstanceStatus::Failed;
            }
        }
    }

    // Track start time for reporting and timeout
    let start_time = chrono::Utc::now();
    let start_instant = Instant::now();

    // Simple polling mode
    println!("\n=== nix-bench-ec2 ===");
    println!("Run ID: {}", run_id);
    println!("Instances: {}", config.instance_types.join(", "));
    println!("Benchmark: {} ({} runs each)", config.attr, config.runs);
    if config.timeout > 0 {
        println!("Timeout: {}s", config.timeout);
    }
    println!("Started: {}\n", start_time.format("%Y-%m-%d %H:%M:%S UTC"));

    loop {
        // Check for timeout
        let elapsed_secs = start_instant.elapsed().as_secs();
        if config.timeout > 0 && elapsed_secs > config.timeout {
            warn!(elapsed_secs = elapsed_secs, timeout = config.timeout, "Run timeout exceeded");
            println!("\n⚠️  TIMEOUT: Run exceeded {}s limit", config.timeout);
            for (instance_type, state) in instances.iter_mut() {
                if state.status != InstanceStatus::Complete && state.status != InstanceStatus::Failed {
                    error!(instance_type = %instance_type, "Instance timed out");
                    state.status = InstanceStatus::Failed;
                }
            }
            break;
        }

        let metrics = cloudwatch.poll_metrics(&config.instance_types).await?;

        let mut all_complete = true;
        let mut total_runs = 0u32;
        let mut completed_runs = 0u32;

        for (instance_type, state) in instances.iter_mut() {
            // Skip already failed instances
            if state.status == InstanceStatus::Failed {
                total_runs += state.total_runs;
                continue;
            }

            if let Some(m) = metrics.get(instance_type) {
                if let Some(status) = m.status {
                    match status {
                        2 => state.status = InstanceStatus::Complete,
                        -1 => state.status = InstanceStatus::Failed,
                        1 => state.status = InstanceStatus::Running,
                        _ => {}
                    }
                }
                if let Some(progress) = m.run_progress {
                    state.run_progress = progress;
                }
                state.durations = m.durations.clone();
            } else {
                // No metrics yet - check console output for bootstrap failures
                if !state.instance_id.is_empty() && state.status == InstanceStatus::Running {
                    if let Ok(Some(console_output)) = ec2.get_console_output(&state.instance_id).await {
                        if let Some(failure_pattern) = detect_bootstrap_failure(&console_output) {
                            error!(
                                instance_type = %instance_type,
                                instance_id = %state.instance_id,
                                pattern = %failure_pattern,
                                "Bootstrap failure detected"
                            );
                            println!("\n❌ Bootstrap failure on {}: {}", instance_type, failure_pattern);
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
    cleanup_resources(&config, &run_id, &bucket_name, &instances).await?;
    write_results(&config, &run_id, &instances).await?;

    info!("Benchmark run complete");
    Ok(())
}

/// Cleanup resources (terminate instances, delete bucket)
async fn cleanup_resources(
    config: &RunConfig,
    run_id: &str,
    bucket_name: &str,
    instances: &HashMap<String, InstanceState>,
) -> Result<()> {
    let db = state::open_db()?;
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
        if let Ok(db_resources) = state::get_run_resources(&db, run_id) {
            for resource in db_resources {
                if resource.resource_type == ResourceType::Ec2Instance {
                    instance_ids.insert(resource.resource_id);
                }
            }
        }

        info!(count = instance_ids.len(), "Terminating instances...");
        for instance_id in &instance_ids {
            match ec2.terminate_instance(instance_id).await {
                Ok(()) => {
                    info!(instance_id = %instance_id, "Instance terminated");
                    let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id);
                }
                Err(e) => {
                    let error_str = format!("{:?}", e);
                    if error_str.contains("InvalidInstanceID.NotFound") {
                        // Already terminated
                        let _ = state::mark_resource_deleted(&db, ResourceType::Ec2Instance, instance_id);
                    } else {
                        warn!(instance_id = %instance_id, error = ?e, "Failed to terminate instance");
                    }
                }
            }
        }

        info!("Deleting S3 bucket...");
        match s3.delete_bucket(bucket_name).await {
            Ok(()) => {
                let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name);
            }
            Err(e) => {
                let error_str = format!("{:?}", e);
                if error_str.contains("NoSuchBucket") {
                    let _ = state::mark_resource_deleted(&db, ResourceType::S3Bucket, bucket_name);
                } else {
                    warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
                }
            }
        }

        // Delete IAM resources if we created them
        if let Ok(db_resources) = state::get_run_resources(&db, run_id) {
            let iam_roles: Vec<_> = db_resources
                .iter()
                .filter(|r| r.resource_type == ResourceType::IamRole && r.deleted_at.is_none())
                .collect();

            if !iam_roles.is_empty() {
                info!("Deleting IAM resources...");
                let iam = IamClient::new(&config.region).await?;
                for resource in iam_roles {
                    if let Err(e) = iam.delete_benchmark_role(&resource.resource_id).await {
                        warn!(role = %resource.resource_id, error = ?e, "Failed to delete IAM role");
                    } else {
                        let _ = state::mark_resource_deleted(&db, ResourceType::IamRole, &resource.resource_id);
                        // Instance profile has the same name as the role
                        let _ = state::mark_resource_deleted(&db, ResourceType::IamInstanceProfile, &resource.resource_id);
                    }
                }
            }
        }
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
    )?;

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
        assert!(result.unwrap().contains("nix-bench-agent: command not found"));
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
}
