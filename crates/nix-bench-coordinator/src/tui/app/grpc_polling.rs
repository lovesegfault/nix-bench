//! gRPC status polling for the TUI application

use super::App;
use crate::aws::{GrpcInstanceStatus, GrpcStatusPoller};
use crate::orchestrator::{CleanupRequest, InstanceStatus};
use std::collections::HashMap;
use std::time::Instant;

/// gRPC port for agent communication (from common defaults)
const GRPC_PORT: u16 = nix_bench_common::defaults::DEFAULT_GRPC_PORT;

impl App {
    /// Update instance states from gRPC status polling results
    ///
    /// Returns a list of cleanup requests for instances that have newly completed.
    pub fn update_from_grpc_status(
        &mut self,
        status_map: &HashMap<String, GrpcInstanceStatus>,
    ) -> Vec<CleanupRequest> {
        use nix_bench_common::StatusCode;
        let mut to_cleanup = Vec::new();

        for (instance_type, status) in status_map {
            if let Some(state) = self.instances.data.get_mut(instance_type) {
                let was_complete = state.status == InstanceStatus::Complete;
                let was_failed = state.status == InstanceStatus::Failed;

                // Skip status updates for terminated instances (terminal state)
                if let Some(status_code) = status.status {
                    if state.status != InstanceStatus::Terminated {
                        state.status = match status_code {
                            StatusCode::Complete => InstanceStatus::Complete,
                            StatusCode::Failed => InstanceStatus::Failed,
                            StatusCode::Running | StatusCode::Bootstrap | StatusCode::Warmup => {
                                InstanceStatus::Running
                            }
                            StatusCode::Pending => InstanceStatus::Pending,
                        };
                    }
                }
                if let Some(progress) = status.run_progress {
                    state.run_progress = progress;
                }
                state.run_results = status.run_results.clone();

                // Append error message to console output when agent reports failure
                if state.status == InstanceStatus::Failed && !was_failed {
                    if let Some(ref msg) = status.error_message {
                        state
                            .console_output
                            .push_line(format!("=== Agent Error: {} ===", msg));
                    }
                }

                // Terminate instances that just reached a terminal state (Complete or Failed)
                let is_terminal = state.status == InstanceStatus::Complete
                    || state.status == InstanceStatus::Failed;
                let was_terminal = was_complete || was_failed;
                if is_terminal
                    && !was_terminal
                    && !self.context.cleanup_requested.contains(instance_type)
                {
                    self.context.cleanup_requested.insert(instance_type.clone());
                    to_cleanup.push(CleanupRequest::TerminateInstance {
                        instance_type: instance_type.clone(),
                        instance_id: state.instance_id.clone(),
                        public_ip: state.public_ip.clone(),
                    });
                }
            }
        }
        self.context.last_update = Instant::now();
        // Re-sort instances by average duration (fastest first)
        self.instances.sort_by_average_duration();
        to_cleanup
    }

    /// Get instances that have public IPs (for gRPC polling)
    pub(super) fn get_instances_with_ips(&self) -> Vec<(String, String)> {
        self.instances
            .data
            .iter()
            .filter_map(|(instance_type, state)| {
                state
                    .public_ip
                    .as_ref()
                    .map(|ip| (instance_type.clone(), ip.clone()))
            })
            .collect()
    }

    /// Spawn background gRPC status polling
    ///
    /// This spawns the polling as a background task and sends results via channel,
    /// ensuring the TUI event loop is never blocked by slow gRPC connections.
    pub(super) fn spawn_grpc_poll(
        &self,
        tx: tokio::sync::mpsc::Sender<HashMap<String, GrpcInstanceStatus>>,
    ) {
        let instances_with_ips = self.get_instances_with_ips();
        if instances_with_ips.is_empty() {
            return;
        }

        // TLS is required for gRPC polling - skip if not configured yet
        let tls_config = match &self.context.tls_config {
            Some(tls) => tls.clone(),
            None => return,
        };

        tokio::spawn(async move {
            let poller = GrpcStatusPoller::new(&instances_with_ips, GRPC_PORT, tls_config);
            let status_map = poller.poll_status().await;
            // Send results; ignore error if receiver is dropped
            let _ = tx.send(status_map).await;
        });
    }
}
