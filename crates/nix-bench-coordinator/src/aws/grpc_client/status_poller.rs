//! gRPC status polling

use super::channel::{ChannelOptions, GrpcChannelBuilder};
use nix_bench_common::{RunResult, TlsConfig};
use nix_bench_proto::LogStreamClient;
use std::collections::HashMap;
use tracing::debug;

/// Instance status from gRPC GetStatus RPC
#[derive(Debug, Clone, Default)]
pub struct GrpcInstanceStatus {
    /// Status: 1=running, 2=complete, -1=failed
    pub status: Option<i32>,
    /// Current run progress (completed runs)
    pub run_progress: Option<u32>,
    /// Total number of runs
    pub total_runs: Option<u32>,
    /// Build durations in seconds for completed runs
    pub durations: Vec<f64>,
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

    /// Poll status from all instances
    ///
    /// Returns a map of instance_type to status. Failed connections are logged
    /// but don't fail the overall poll.
    pub async fn poll_status(&self) -> HashMap<String, GrpcInstanceStatus> {
        let mut results = HashMap::new();

        for (instance_type, public_ip, port) in &self.instances {
            let builder = GrpcChannelBuilder::new(public_ip, *port, &self.tls_config)
                .with_options(ChannelOptions::for_polling());

            let channel = match builder.try_connect().await {
                Some(ch) => ch,
                None => {
                    debug!(
                        instance_type = %instance_type,
                        "Failed to connect for status poll"
                    );
                    continue;
                }
            };

            let mut client = LogStreamClient::new(channel);
            match client
                .get_status(nix_bench_proto::StatusRequest {})
                .await
            {
                Ok(response) => {
                    let status = response.into_inner();
                    let status_code = if status.status_code != 0 {
                        Some(status.status_code)
                    } else {
                        nix_bench_common::StatusCode::parse(&status.status).map(|c| c.as_i32())
                    };

                    let run_results: Vec<RunResult> = status
                        .run_results
                        .iter()
                        .map(|r| RunResult {
                            run_number: r.run_number,
                            duration_secs: r.duration_secs,
                            success: r.success,
                        })
                        .collect();

                    results.insert(
                        instance_type.clone(),
                        GrpcInstanceStatus {
                            status: status_code,
                            run_progress: Some(status.run_progress),
                            total_runs: Some(status.total_runs),
                            durations: status.durations,
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
                    );
                }
                Err(e) => {
                    debug!(
                        instance_type = %instance_type,
                        error = %e,
                        "GetStatus RPC failed"
                    );
                }
            }
        }

        results
    }
}
