//! gRPC status polling

use super::channel::{ChannelOptions, GrpcChannelBuilder};
use futures::future::join_all;
use nix_bench_common::{RunResult, StatusCode, TlsConfig};
use nix_bench_proto::LogStreamClient;
use std::collections::HashMap;
use tracing::debug;

/// Instance status from gRPC GetStatus RPC
#[derive(Debug, Clone, Default)]
pub struct GrpcInstanceStatus {
    /// Status code from proto enum
    pub status: Option<StatusCode>,
    /// Current run progress (completed runs)
    pub run_progress: Option<u32>,
    /// Total number of runs
    pub total_runs: Option<u32>,
    /// Number of dropped log messages (for monitoring)
    pub dropped_log_count: u64,
    /// Detailed run results with success/failure
    pub run_results: Vec<RunResult>,
    /// Nix attribute being built
    pub attr: Option<String>,
    /// System architecture
    pub system: Option<String>,
}

/// Polls status from multiple gRPC agents with mTLS
pub struct GrpcStatusPoller {
    /// Map of instance_type to (public_ip, port)
    instances: Vec<(String, String, u16)>,
    /// TLS configuration (required)
    tls_config: TlsConfig,
}

impl GrpcStatusPoller {
    /// Create a new status poller for the given instances with TLS
    pub fn new(instances: &[(String, String)], port: u16, tls_config: TlsConfig) -> Self {
        Self {
            instances: instances
                .iter()
                .map(|(t, ip)| (t.clone(), ip.clone(), port))
                .collect(),
            tls_config,
        }
    }

    /// Poll status from all instances in parallel
    ///
    /// Returns a map of instance_type to status. Failed connections are logged
    /// but don't fail the overall poll. All instances are polled concurrently
    /// to minimize total latency.
    pub async fn poll_status(&self) -> HashMap<String, GrpcInstanceStatus> {
        let futures: Vec<_> = self
            .instances
            .iter()
            .map(|(instance_type, public_ip, port)| {
                let instance_type = instance_type.clone();
                let public_ip = public_ip.clone();
                let port = *port;
                let tls_config = self.tls_config.clone();

                async move {
                    Self::poll_single_instance(&instance_type, &public_ip, port, &tls_config).await
                }
            })
            .collect();

        join_all(futures).await.into_iter().flatten().collect()
    }

    /// Poll a single instance for status
    async fn poll_single_instance(
        instance_type: &str,
        public_ip: &str,
        port: u16,
        tls_config: &TlsConfig,
    ) -> Option<(String, GrpcInstanceStatus)> {
        let builder = GrpcChannelBuilder::new(public_ip, port, tls_config)
            .with_options(ChannelOptions::for_polling());

        let channel = match builder.try_connect().await {
            Some(ch) => ch,
            None => {
                debug!(
                    instance_type = %instance_type,
                    "Failed to connect for status poll"
                );
                return None;
            }
        };

        let mut client = LogStreamClient::new(channel);
        match client.get_status(nix_bench_proto::StatusRequest {}).await {
            Ok(response) => {
                let status = response.into_inner();
                let status_code = StatusCode::from_i32(status.status_code);

                let run_results: Vec<RunResult> = status
                    .run_results
                    .iter()
                    .map(|r| RunResult {
                        run_number: r.run_number,
                        duration_secs: r.duration_secs,
                        success: r.success,
                    })
                    .collect();

                Some((
                    instance_type.to_string(),
                    GrpcInstanceStatus {
                        status: status_code,
                        run_progress: Some(status.run_progress),
                        total_runs: Some(status.total_runs),
                        dropped_log_count: status.dropped_log_count,
                        run_results,
                        attr: if status.attr.is_empty() {
                            None
                        } else {
                            Some(status.attr)
                        },
                        system: if status.system.is_empty() {
                            None
                        } else {
                            Some(status.system)
                        },
                    },
                ))
            }
            Err(e) => {
                debug!(
                    instance_type = %instance_type,
                    error = %e,
                    "GetStatus RPC failed"
                );
                None
            }
        }
    }
}

/// Send an AcknowledgeComplete RPC to an agent.
///
/// This tells the agent that the coordinator has received its final status
/// and it can shut down promptly instead of waiting for a fallback timeout.
pub async fn send_ack_complete(ip: &str, port: u16, run_id: &str, tls_config: &TlsConfig) {
    let channel = GrpcChannelBuilder::new(ip, port, tls_config)
        .with_options(ChannelOptions {
            connect_timeout: std::time::Duration::from_secs(5),
            ..Default::default()
        })
        .connect()
        .await;

    let channel = match channel {
        Ok(c) => c,
        Err(e) => {
            debug!(ip = %ip, error = %e, "Failed to connect for ack");
            return;
        }
    };

    let mut client = LogStreamClient::new(channel);
    let request = nix_bench_proto::AckCompleteRequest {
        run_id: run_id.to_string(),
    };

    match client.acknowledge_complete(request).await {
        Ok(_) => debug!(ip = %ip, "Sent completion acknowledgment"),
        Err(e) => {
            debug!(ip = %ip, error = %e, "Failed to send ack (agent may have already shut down)")
        }
    }
}
