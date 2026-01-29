//! gRPC log streaming client

use super::channel::{ChannelOptions, GrpcChannelBuilder};
use crate::tui::TuiMessage;
use crate::wait::{WaitConfig, wait_for_resource};
use anyhow::{Context, Result};
use backon::{ExponentialBuilder, Retryable};
use nix_bench_common::TlsConfig;
use nix_bench_common::proto::{LogStreamClient, StatusRequest, StreamLogsRequest};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tracing::{debug, error, info};

/// gRPC log streaming client for connecting to benchmark agents with mTLS
#[derive(Debug, Clone)]
pub struct GrpcLogClient {
    /// Instance type identifier
    pub instance_type: String,
    /// Public IP of the agent
    pub public_ip: String,
    /// gRPC port (default 50051)
    pub port: u16,
    /// Run ID for filtering logs
    pub run_id: String,
    /// TLS configuration for mTLS (required)
    tls_config: TlsConfig,
}

impl GrpcLogClient {
    /// Create a new gRPC log client with TLS configuration (required)
    pub fn new(
        instance_type: &str,
        public_ip: &str,
        port: u16,
        run_id: &str,
        tls_config: TlsConfig,
    ) -> Self {
        Self {
            instance_type: instance_type.to_string(),
            public_ip: public_ip.to_string(),
            port,
            run_id: run_id.to_string(),
            tls_config,
        }
    }

    /// Create a channel builder for this client
    pub fn channel_builder(&self) -> GrpcChannelBuilder<'_> {
        GrpcChannelBuilder::new(&self.public_ip, self.port, &self.tls_config)
    }

    /// Connect to the agent with retry logic using exponential backoff
    pub async fn connect_with_retry(
        &self,
        max_retries: u32,
        initial_delay: Duration,
    ) -> Result<LogStreamClient<Channel>> {
        let endpoint = self.channel_builder().endpoint().to_string();
        let instance_type = self.instance_type.clone();

        let channel = (|| async {
            let builder = self.channel_builder();
            debug!(
                instance_type = %instance_type,
                endpoint = %endpoint,
                tls = true,
                "Attempting gRPC connection"
            );
            builder.connect().await
        })
        .retry(
            ExponentialBuilder::default()
                .with_min_delay(initial_delay)
                .with_max_delay(Duration::from_secs(30))
                .with_max_times(max_retries as usize),
        )
        .notify(|e, dur| {
            debug!(
                instance_type = %instance_type,
                endpoint = %endpoint,
                error = %e,
                delay = ?dur,
                "gRPC connection failed, retrying"
            );
        })
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to connect to {} after {} attempts: {}",
                endpoint,
                max_retries,
                e
            )
        })?;

        info!(
            instance_type = %self.instance_type,
            endpoint = %endpoint,
            tls = true,
            "gRPC connection established"
        );

        Ok(LogStreamClient::new(channel))
    }

    /// Wait for the agent to be ready by polling the GetStatus RPC
    pub async fn wait_for_ready(&self, max_wait: Duration, initial_delay: Duration) -> Result<()> {
        info!(
            instance_type = %self.instance_type,
            initial_delay_secs = initial_delay.as_secs(),
            max_wait_secs = max_wait.as_secs(),
            tls = true,
            "Waiting for agent to be ready"
        );

        // Initial delay before starting to poll
        tokio::time::sleep(initial_delay).await;

        let config = WaitConfig {
            initial_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(30),
            timeout: max_wait,
            jitter: 0.25,
        };

        wait_for_resource(
            config,
            None,
            || async {
                let builder = self
                    .channel_builder()
                    .with_options(ChannelOptions::for_readiness());

                match builder.connect().await {
                    Ok(channel) => {
                        let mut client = LogStreamClient::new(channel);
                        match client.get_status(StatusRequest {}).await {
                            Ok(response) => {
                                let status = response.into_inner();
                                info!(
                                    instance_type = %self.instance_type,
                                    status_code = status.status_code,
                                    run_progress = status.run_progress,
                                    total_runs = status.total_runs,
                                    "Agent is ready"
                                );
                                Ok(true) // Ready
                            }
                            Err(e) => {
                                debug!(
                                    instance_type = %self.instance_type,
                                    error = %e,
                                    "GetStatus failed, agent not ready yet"
                                );
                                Ok(false) // Not ready, keep waiting
                            }
                        }
                    }
                    Err(e) => {
                        debug!(
                            instance_type = %self.instance_type,
                            error = %e,
                            "Connection failed, agent not ready yet"
                        );
                        Ok(false) // Not ready, keep waiting
                    }
                }
            },
            &format!("agent {} readiness", self.instance_type),
        )
        .await
    }

    /// Stream logs from the agent and forward them to the TUI channel
    pub async fn stream_to_channel(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

        self.stream_to_channel_direct(tx).await
    }

    /// Stream logs to channel without wait_for_ready (for testing with local servers)
    pub async fn stream_to_channel_direct(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
        self.stream_logs_with(|log_entry| {
            let msg = TuiMessage::ConsoleOutputAppend {
                instance_type: self.instance_type.clone(),
                line: log_entry.message,
            };
            let tx = tx.clone();
            async move { tx.send(msg).await.is_ok() }
        })
        .await
    }

    /// Stream logs from the agent and print to stdout (for no-TUI mode)
    pub async fn stream_to_stdout(&self) -> Result<()> {
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

        let instance_type = self.instance_type.clone();
        self.stream_logs_with(|log_entry| {
            println!("[{}] {}", instance_type, log_entry.message);
            async { true }
        })
        .await
    }

    /// Shared log streaming implementation.
    ///
    /// Connects to the agent, starts the log stream, and calls `on_entry` for
    /// each received log entry. If `on_entry` returns `false`, streaming stops.
    async fn stream_logs_with<F, Fut>(&self, on_entry: F) -> Result<()>
    where
        F: Fn(nix_bench_common::proto::LogEntry) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let mut client = self
            .connect_with_retry(30, Duration::from_secs(2))
            .await
            .context("Failed to establish gRPC connection")?;

        let request = StreamLogsRequest {
            instance_type: self.instance_type.clone(),
            run_id: self.run_id.clone(),
        };

        info!(
            instance_type = %self.instance_type,
            run_id = %self.run_id,
            "Starting log stream"
        );

        let response = client
            .stream_logs(request)
            .await
            .context("Failed to start log stream")?;

        let mut stream = response.into_inner();

        while let Some(result) = stream.next().await {
            match result {
                Ok(log_entry) => {
                    if !on_entry(log_entry).await {
                        debug!(
                            instance_type = %self.instance_type,
                            "Log sink closed, stopping stream"
                        );
                        break;
                    }
                }
                Err(status) => {
                    error!(
                        instance_type = %self.instance_type,
                        status = %status,
                        "Log stream error"
                    );
                    return Err(anyhow::anyhow!("Log stream error: {}", status));
                }
            }
        }

        info!(instance_type = %self.instance_type, "Log stream completed");
        Ok(())
    }
}
