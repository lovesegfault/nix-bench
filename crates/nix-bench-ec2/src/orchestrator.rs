//! Main orchestration logic for benchmark runs

use crate::aws::{CloudWatchClient, Ec2Client, S3Client};
use crate::config::{detect_system, AgentConfig, RunConfig};
use crate::state::{self, ResourceType, RunStatus};
use crate::tui;
use anyhow::Result;
use std::collections::HashMap;
use tracing::{error, info, warn};
use uuid::Uuid;

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

    // Validate agent binaries are provided (unless dry-run)
    if !config.dry_run {
        if needs_x86_64 && config.agent_x86_64.is_none() {
            anyhow::bail!(
                "x86_64 instance types specified but --agent-x86_64 not provided. \
                 Build with: nix build .#nix-bench-agent"
            );
        }
        if needs_aarch64 && config.agent_aarch64.is_none() {
            anyhow::bail!(
                "aarch64 instance types specified but --agent-aarch64 not provided. \
                 Cross-compile with: nix build .#nix-bench-agent --system aarch64-linux"
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
            if let Some(path) = &config.agent_x86_64 {
                println!("    - x86_64:  {}", path);
            } else {
                println!("    - x86_64:  NOT PROVIDED (required)");
            }
        }
        if needs_aarch64 {
            if let Some(path) = &config.agent_aarch64 {
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
    let bucket_name = format!("nix-bench-{}", &run_id[..8]);

    info!(run_id = %run_id, bucket = %bucket_name, "Starting benchmark run");

    // Open state database
    let db = state::open_db()?;

    // Record run in state
    state::insert_run(&db, &run_id, &config.region, &config.instance_types, &config.attr)?;

    // Initialize AWS clients
    let ec2 = Ec2Client::new(&config.region).await?;
    let s3 = S3Client::new(&config.region).await?;
    let cloudwatch = CloudWatchClient::new(&config.region, &run_id).await?;

    // Create S3 bucket
    info!("Creating S3 bucket for run artifacts");
    s3.create_bucket(&bucket_name).await?;
    state::insert_resource(&db, &run_id, ResourceType::S3Bucket, &bucket_name, &config.region)?;

    // Upload agent binaries
    if let Some(agent_path) = &config.agent_x86_64 {
        let key = format!("{}/agent-x86_64", run_id);
        info!(path = %agent_path, key = %key, "Uploading x86_64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }
    if let Some(agent_path) = &config.agent_aarch64 {
        let key = format!("{}/agent-aarch64", run_id);
        info!(path = %agent_path, key = %key, "Uploading aarch64 agent binary");
        s3.upload_file(&bucket_name, &key, std::path::Path::new(agent_path))
            .await?;
    }

    // Upload configs for each architecture (deduplicated)
    let mut uploaded_configs = std::collections::HashSet::new();
    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
        let arch = if system == "aarch64-linux" {
            "aarch64"
        } else {
            "x86_64"
        };

        // Skip if we already uploaded config for this arch
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
        };

        let config_json = serde_json::to_string_pretty(&agent_config)?;
        let key = format!("{}/config-{}.json", run_id, arch);

        info!(key = %key, "Uploading agent config");
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
                config.instance_profile.as_deref(),
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
        match ec2.wait_for_running(&state.instance_id).await {
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

    // Track start time for reporting
    let start_time = chrono::Utc::now();

    // Run TUI or poll mode
    if config.no_tui {
        // Simple polling mode with better formatting
        println!("\n=== nix-bench-ec2 ===");
        println!("Run ID: {}", run_id);
        println!("Instances: {}", config.instance_types.join(", "));
        println!("Benchmark: {} ({} runs each)", config.attr, config.runs);
        println!("Started: {}\n", start_time.format("%Y-%m-%d %H:%M:%S UTC"));

        loop {
            let metrics = cloudwatch.poll_metrics(&config.instance_types).await?;

            let mut all_complete = true;
            let mut total_runs = 0u32;
            let mut completed_runs = 0u32;

            for (instance_type, state) in instances.iter_mut() {
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
                }

                if state.status != InstanceStatus::Complete && state.status != InstanceStatus::Failed
                {
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
    } else {
        // TUI mode
        tui::run_tui(&mut instances, &cloudwatch, &config).await?;
    }

    // Cleanup
    if !config.keep {
        info!("Terminating instances...");
        for (instance_type, state) in &instances {
            if let Err(e) = ec2.terminate_instance(&state.instance_id).await {
                warn!(instance_type = %instance_type, error = ?e, "Failed to terminate instance");
            } else {
                state::mark_resource_deleted(&db, ResourceType::Ec2Instance, &state.instance_id)?;
            }
        }

        info!("Deleting S3 bucket...");
        if let Err(e) = s3.delete_bucket(&bucket_name).await {
            warn!(bucket = %bucket_name, error = ?e, "Failed to delete bucket");
        } else {
            state::mark_resource_deleted(&db, ResourceType::S3Bucket, &bucket_name)?;
        }
    } else {
        info!("Keeping instances and bucket (--keep specified)");
    }

    // Update run status
    let all_complete = instances
        .values()
        .all(|s| s.status == InstanceStatus::Complete);

    state::update_run_status(
        &db,
        &run_id,
        if all_complete {
            RunStatus::Completed
        } else {
            RunStatus::Failed
        },
    )?;

    // Calculate end time and duration
    let end_time = chrono::Utc::now();
    let duration = end_time - start_time;

    // Output results
    if let Some(output_path) = &config.output {
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
                    .min_by(|a, b| a.partial_cmp(b).unwrap())
                    .unwrap_or(0.0);
                let max = v
                    .durations
                    .iter()
                    .cloned()
                    .max_by(|a, b| a.partial_cmp(b).unwrap())
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

    info!("Benchmark run complete");
    Ok(())
}
