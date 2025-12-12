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

    // Upload agent binaries (placeholder - in real implementation, these would be pre-built)
    // For now, we'll note that the agent needs to be cross-compiled and uploaded
    info!("Note: Agent binaries should be uploaded to s3://{}/{}/agent-x86_64 and agent-aarch64", bucket_name, run_id);

    // Upload configs for each instance type
    for instance_type in &config.instance_types {
        let system = detect_system(instance_type);
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
        let arch = if system == "aarch64-linux" {
            "aarch64"
        } else {
            "x86_64"
        };
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
            .launch_instance(&run_id, instance_type, system, &user_data, None, None, None)
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

    // Run TUI or poll mode
    if config.no_tui {
        // Simple polling mode
        info!("Running in non-TUI mode, polling for progress...");
        loop {
            let metrics = cloudwatch.poll_metrics(&config.instance_types).await?;

            let mut all_complete = true;
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

                println!(
                    "{}: {} ({}/{})",
                    instance_type,
                    match state.status {
                        InstanceStatus::Pending => "pending",
                        InstanceStatus::Launching => "launching",
                        InstanceStatus::Running => "running",
                        InstanceStatus::Complete => "complete",
                        InstanceStatus::Failed => "failed",
                    },
                    state.run_progress,
                    state.total_runs
                );
            }

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

    // Output results
    if let Some(output_path) = &config.output {
        let results: HashMap<String, serde_json::Value> = instances
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    serde_json::json!({
                        "status": format!("{:?}", v.status),
                        "runs_completed": v.run_progress,
                        "durations": v.durations,
                    }),
                )
            })
            .collect();

        let output = serde_json::json!({
            "run_id": run_id,
            "attr": config.attr,
            "total_runs": config.runs,
            "results": results,
        });

        std::fs::write(output_path, serde_json::to_string_pretty(&output)?)?;
        info!(path = %output_path, "Results written");
    }

    info!("Benchmark run complete");
    Ok(())
}
