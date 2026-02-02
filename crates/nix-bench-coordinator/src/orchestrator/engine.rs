//! Unified orchestration engine for benchmark runs
//!
//! `RunEngine` owns all instance state and orchestration logic, providing a
//! single source of truth for both TUI and CLI modes. Each mode drives the
//! engine at its own cadence via `prepare_poll()` / `apply_poll_results()`.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{Context as _, Result};
use nix_bench_common::defaults::DEFAULT_GRPC_PORT;
use nix_bench_common::{RunId, TlsConfig};
use tracing::{error, info, warn};

use super::cleanup::{FullCleanupConfig, full_cleanup};
use super::events::RunEvent;
use super::init::InitContext;
use super::progress::Reporter;
use super::results::{print_results_summary, write_results};
use super::types::{InstanceState, InstanceStatus, instances_with_ips};
use super::user_data::detect_bootstrap_failure;
use crate::aws::{Ec2Client, GrpcInstanceStatus, GrpcStatusPoller, send_ack_complete};
use crate::config::RunConfig;
use crate::tui::CleanupProgress;

/// gRPC port for agent communication
const GRPC_PORT: u16 = DEFAULT_GRPC_PORT;

/// Snapshot of data needed to poll gRPC status.
///
/// Created by `RunEngine::prepare_poll()` so the actual I/O can happen
/// in a background task without borrowing the engine.
pub struct PollRequest {
    instances: Vec<(String, String)>,
    port: u16,
    tls_config: TlsConfig,
}

impl PollRequest {
    /// Execute the gRPC status poll (can be called from any task).
    pub async fn execute(&self) -> HashMap<String, GrpcInstanceStatus> {
        GrpcStatusPoller::new(&self.instances, self.port, self.tls_config.clone())
            .poll_status()
            .await
    }
}

/// Unified orchestration engine for a benchmark run.
///
/// Owns all instance state and provides the polling/update API that both
/// TUI and CLI modes drive.
pub struct RunEngine {
    // Configuration
    config: RunConfig,
    run_id: RunId,
    bucket_name: String,

    // Instance state (the single source of truth)
    instances: HashMap<String, InstanceState>,

    // Networking
    tls_config: TlsConfig,
    ec2: Ec2Client,

    // Lifecycle
    start_time: Instant,
    acked: HashSet<String>,
    terminated: HashSet<String>,

    // Resources for cleanup
    security_group_id: Option<String>,
    iam_role_name: Option<String>,
    sg_rules: Vec<String>,
}

impl std::fmt::Debug for RunEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunEngine")
            .field("run_id", &self.run_id)
            .field("instances", &self.instances.len())
            .finish()
    }
}

impl RunEngine {
    /// Create from an `InitContext` after initialization completes.
    pub(super) fn from_init_context(config: &RunConfig, ctx: InitContext) -> Self {
        Self {
            config: config.clone(),
            run_id: ctx.run_id,
            bucket_name: ctx.bucket_name,
            instances: ctx.instances,
            tls_config: ctx.coordinator_tls_config,
            ec2: ctx.ec2,
            start_time: Instant::now(),
            acked: HashSet::new(),
            terminated: HashSet::new(),
            security_group_id: ctx.security_group_id,
            iam_role_name: ctx.iam_role_name,
            sg_rules: ctx.sg_rules,
        }
    }

    // ── Read-only accessors ─────────────────────────────────────────────

    /// Read-only access to instance state (for rendering and reporting).
    pub fn instances(&self) -> &HashMap<String, InstanceState> {
        &self.instances
    }

    /// Mutable access to instance state (for TUI-side updates like console output).
    pub fn instances_mut(&mut self) -> &mut HashMap<String, InstanceState> {
        &mut self.instances
    }

    /// Collect instance types paired with their public IPs.
    pub fn instances_with_ips(&self) -> Vec<(String, String)> {
        instances_with_ips(&self.instances)
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    pub fn bucket_name(&self) -> &str {
        &self.bucket_name
    }

    pub fn tls_config(&self) -> &TlsConfig {
        &self.tls_config
    }

    pub fn config(&self) -> &RunConfig {
        &self.config
    }

    pub fn start_time(&self) -> Instant {
        self.start_time
    }

    /// True when all instances are in a terminal state.
    pub fn is_all_complete(&self) -> bool {
        self.instances.values().all(|s| {
            matches!(
                s.status,
                InstanceStatus::Complete | InstanceStatus::Failed | InstanceStatus::Terminated
            )
        })
    }

    /// True when all complete instances have their full duration results.
    ///
    /// Guards against the race where we see `Complete` status before the
    /// final duration is polled.
    pub fn all_results_captured(&self) -> bool {
        self.instances.values().all(|s| match s.status {
            InstanceStatus::Complete => s.durations().len() as u32 >= s.run_progress,
            InstanceStatus::Failed | InstanceStatus::Terminated => true,
            _ => false,
        })
    }

    // ── Streaming & Polling ────────────────────────────────────────────

    /// Start gRPC log streaming for instances with public IPs.
    ///
    /// If `tui_tx` is provided, logs are forwarded to the TUI channel.
    /// Otherwise, logs are printed to stdout.
    pub fn start_streaming(
        &self,
        tui_tx: Option<tokio::sync::mpsc::Sender<crate::tui::TuiMessage>>,
    ) {
        use crate::aws::{LogStreamingOptions, start_log_streaming_unified};
        use tracing::debug;

        let ips = self.instances_with_ips();
        if ips.is_empty() {
            warn!("No instances have public IPs, gRPC streaming not available");
            return;
        }

        info!(count = ips.len(), "Starting gRPC log streaming with mTLS");

        let mut options = LogStreamingOptions::new(
            &ips,
            self.run_id.as_str(),
            GRPC_PORT,
            self.tls_config.clone(),
        );
        if let Some(tx) = tui_tx {
            options = options.with_channel(tx);
        }
        let grpc_handles = start_log_streaming_unified(options);

        tokio::spawn(async move {
            for handle in grpc_handles {
                match handle.await {
                    Ok(Ok(())) => debug!("gRPC streaming task completed successfully"),
                    Ok(Err(e)) => warn!(error = ?e, "gRPC streaming task failed"),
                    Err(e) if e.is_panic() => error!(error = ?e, "gRPC streaming task panicked"),
                    Err(e) => debug!(error = ?e, "gRPC streaming task cancelled"),
                }
            }
        });
    }

    /// Prepare a poll request that can be executed in a background task.
    ///
    /// Returns `None` if there are no instances with public IPs to poll.
    pub fn prepare_poll(&self) -> Option<PollRequest> {
        let ips = self.instances_with_ips();
        if ips.is_empty() {
            return None;
        }
        Some(PollRequest {
            instances: ips,
            port: GRPC_PORT,
            tls_config: self.tls_config.clone(),
        })
    }

    /// Apply results from a completed gRPC status poll.
    ///
    /// Updates instance state, sends acks for newly-terminal instances,
    /// and spawns background termination tasks. Returns events describing
    /// what changed.
    pub async fn apply_poll_results(
        &mut self,
        status_map: HashMap<String, GrpcInstanceStatus>,
    ) -> Vec<RunEvent> {
        let mut events = Vec::new();

        // First pass: collect which instances need ack/termination
        let mut needs_ack: Vec<(String, String)> = Vec::new(); // (instance_type, ip)
        let mut needs_terminate: Vec<(String, String)> = Vec::new(); // (instance_type, instance_id)

        for (instance_type, grpc_status) in &status_map {
            let Some(state) = self.instances.get_mut(instance_type) else {
                continue;
            };

            // Skip already-acked terminal instances
            if state.is_terminal() && self.acked.contains(instance_type) {
                continue;
            }

            let was_failed = state.status == InstanceStatus::Failed;
            let was_terminal = state.is_terminal();

            // Update status
            if let Some(status_code) = grpc_status.status {
                if state.status != InstanceStatus::Terminated {
                    state.status = InstanceStatus::from_status_code(status_code);
                }
            }
            if let Some(progress) = grpc_status.run_progress {
                state.run_progress = progress;
            }
            state.run_results = grpc_status.run_results.clone();

            // Append error message on newly-failed
            if state.status == InstanceStatus::Failed && !was_failed {
                if let Some(ref msg) = grpc_status.error_message {
                    state
                        .console_output
                        .push_line(format!("=== Agent Error: {} ===", msg));
                }
            }

            events.push(RunEvent::InstanceUpdated {
                instance_type: instance_type.clone(),
            });

            // Collect ack/terminate requests (processed after mutable borrow ends)
            if state.is_terminal() && !self.acked.contains(instance_type) {
                if let Some(ip) = &state.public_ip {
                    needs_ack.push((instance_type.clone(), ip.clone()));
                }
                self.acked.insert(instance_type.clone());
            }

            let is_terminal = state.is_terminal();
            if is_terminal && !was_terminal && !self.terminated.contains(instance_type) {
                self.terminated.insert(instance_type.clone());
                needs_terminate.push((instance_type.clone(), state.instance_id.clone()));
                events.push(RunEvent::InstanceTerminal {
                    instance_type: instance_type.clone(),
                });
            }
        }

        // Second pass: send acks (async, uses self.tls_config)
        for (_, ip) in &needs_ack {
            send_ack_complete(ip, GRPC_PORT, self.run_id.as_str(), &self.tls_config).await;
        }

        // Third pass: spawn termination tasks (uses raw EC2 client, no self borrow)
        for (instance_type, instance_id) in needs_terminate {
            self.spawn_terminate(instance_type, instance_id);
        }

        events
    }

    /// Spawn a background task to terminate an instance.
    fn spawn_terminate(&self, instance_type: String, instance_id: String) {
        if instance_id.is_empty() {
            return;
        }
        // Clone the raw AWS SDK client (cheap Arc clone)
        let client = self.ec2.client.clone();
        info!(
            instance_type = %instance_type,
            instance_id = %instance_id,
            "Auto-terminating completed instance"
        );
        tokio::spawn(async move {
            match client
                .terminate_instances()
                .instance_ids(&instance_id)
                .send()
                .await
                .context("Failed to terminate instance")
            {
                Ok(_) => {
                    info!(instance_id = %instance_id, "Instance terminated");
                }
                Err(e) => {
                    warn!(
                        instance_id = %instance_id,
                        error = ?e,
                        "Failed to terminate instance"
                    );
                }
            }
        });
    }

    // ── Timeout & Bootstrap Monitoring ──────────────────────────────────

    /// Check for run timeout. Returns events for any timed-out instances.
    pub fn check_timeout(&mut self) -> Vec<RunEvent> {
        let timeout_secs = self.config.flags.timeout;
        if timeout_secs == 0 {
            return Vec::new();
        }

        let elapsed = self.start_time.elapsed().as_secs();
        if elapsed <= timeout_secs {
            return Vec::new();
        }

        let mut events = Vec::new();
        warn!(
            elapsed_secs = elapsed,
            timeout = timeout_secs,
            "Run timeout exceeded"
        );

        for (instance_type, state) in self.instances.iter_mut() {
            if !state.is_terminal() {
                error!(instance_type = %instance_type, "Instance timed out");
                state.status = InstanceStatus::Failed;
                events.push(RunEvent::Timeout {
                    instance_type: instance_type.clone(),
                });
            }
        }

        events
    }

    /// Check for bootstrap failures via EC2 console output.
    ///
    /// Looks at instances that are in a non-terminal, non-acked state.
    /// Checks their EC2 console output for known failure patterns.
    pub async fn check_bootstrap_failures(&mut self) -> Vec<RunEvent> {
        let mut events = Vec::new();

        // Collect instances to check (avoid borrow conflict)
        let to_check: Vec<(String, String)> = self
            .instances
            .iter()
            .filter(|(instance_type, state)| {
                !state.instance_id.is_empty()
                    && !state.is_terminal()
                    && !self.acked.contains(*instance_type)
            })
            .map(|(t, s)| (t.clone(), s.instance_id.clone()))
            .collect();

        for (instance_type, instance_id) in to_check {
            if let Ok(Some(console_output)) = self.ec2.get_console_output(&instance_id).await {
                if let Some(pattern) = detect_bootstrap_failure(&console_output) {
                    error!(
                        instance_type = %instance_type,
                        instance_id = %instance_id,
                        pattern = %pattern,
                        "Bootstrap failure detected"
                    );

                    if let Some(state) = self.instances.get_mut(&instance_type) {
                        state.status = InstanceStatus::Failed;
                        state.console_output.replace(&console_output);
                    }

                    events.push(RunEvent::BootstrapFailure {
                        instance_type,
                        pattern,
                    });
                }
            }
        }

        events
    }

    // ── Cleanup & Results ───────────────────────────────────────────────

    /// Run full cleanup of remaining AWS resources.
    pub async fn cleanup(&self, on_progress: impl Fn(&CleanupProgress)) -> Result<()> {
        let reporter = Reporter::Log;

        full_cleanup(FullCleanupConfig {
            region: &self.config.aws.region,
            keep: self.config.flags.keep,
            bucket_name: &self.bucket_name,
            instances: &self.instances,
            security_group_id: self.security_group_id.as_deref(),
            iam_role_name: self.iam_role_name.as_deref(),
            sg_rules: &self.sg_rules,
            reporter: &reporter,
        })
        .await?;

        let progress = CleanupProgress {
            current_step: "Cleanup complete".to_string(),
            ..Default::default()
        };
        on_progress(&progress);

        Ok(())
    }

    /// Write results file and print summary table.
    pub async fn report_results(&self) -> Result<()> {
        print_results_summary(&self.instances);
        write_results(&self.config, self.run_id.as_str(), &self.instances).await
    }
}
