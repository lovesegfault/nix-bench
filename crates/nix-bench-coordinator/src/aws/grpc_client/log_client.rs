//! gRPC log streaming client

use super::channel::{jittered_delay, ChannelOptions, GrpcChannelBuilder};
use crate::tui::TuiMessage;
use anyhow::{Context, Result};
use nix_bench_common::TlsConfig;
use nix_bench_proto::{LogStreamClient, StatusRequest, StreamLogsRequest};
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

    /// Connect to the agent with retry logic
    pub async fn connect_with_retry(
        &self,
        max_retries: u32,
        initial_delay: Duration,
    ) -> Result<LogStreamClient<Channel>> {
        let mut delay = initial_delay;
        let mut attempts = 0;

        loop {
            attempts += 1;
            let builder = self.channel_builder();
            let endpoint = builder.endpoint().to_string();

            debug!(
                instance_type = %self.instance_type,
                endpoint = %endpoint,
                attempt = attempts,
                tls = true,
                "Attempting gRPC connection"
            );

            match builder.connect().await {
                Ok(channel) => {
                    info!(
                        instance_type = %self.instance_type,
                        endpoint = %endpoint,
                        tls = true,
                        "gRPC connection established"
                    );
                    return Ok(LogStreamClient::new(channel));
                }
                Err(e) => {
                    if attempts >= max_retries {
                        return Err(anyhow::anyhow!(
                            "Failed to connect to {} after {} attempts: {}",
                            endpoint,
                            attempts,
                            e
                        ));
                    }

                    let jittered = jittered_delay(delay);

                    debug!(
                        instance_type = %self.instance_type,
                        endpoint = %endpoint,
                        attempt = attempts,
                        max_retries = max_retries,
                        error = %e,
                        delay_ms = jittered.as_millis(),
                        "gRPC connection failed, retrying"
                    );

                    tokio::time::sleep(jittered).await;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
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

        tokio::time::sleep(initial_delay).await;

        let start = std::time::Instant::now();
        let mut delay = Duration::from_secs(5);
        let max_delay = Duration::from_secs(30);

        loop {
            if start.elapsed() > max_wait {
                return Err(anyhow::anyhow!(
                    "Agent {} not ready after {:?}",
                    self.instance_type,
                    max_wait
                ));
            }

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
                                status = %status.status,
                                run_progress = status.run_progress,
                                total_runs = status.total_runs,
                                "Agent is ready"
                            );
                            return Ok(());
                        }
                        Err(e) => {
                            debug!(
                                instance_type = %self.instance_type,
                                error = %e,
                                "GetStatus failed, agent not ready yet"
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        instance_type = %self.instance_type,
                        error = %e,
                        elapsed_secs = start.elapsed().as_secs(),
                        "Connection failed, agent not ready yet"
                    );
                }
            }

            tokio::time::sleep(jittered_delay(delay)).await;
            delay = (delay * 2).min(max_delay);
        }
    }

    /// Full RPC health check
    pub async fn wait_for_grpc_ready(&self, timeout: Duration) -> Result<()> {
        let start = std::time::Instant::now();
        let builder = self.channel_builder();
        let endpoint = builder.endpoint().to_string();

        debug!(
            instance_type = %self.instance_type,
            endpoint = %endpoint,
            timeout_secs = timeout.as_secs(),
            "Performing gRPC health check"
        );

        let channel = builder
            .with_options(ChannelOptions {
                connect_timeout: timeout,
                request_timeout: timeout,
            })
            .connect()
            .await?;

        let mut client = LogStreamClient::new(channel);

        let remaining = timeout.saturating_sub(start.elapsed());
        let result = tokio::time::timeout(remaining, client.get_status(StatusRequest {}))
            .await
            .context("GetStatus RPC timed out")?
            .context("GetStatus RPC failed")?;

        let status = result.into_inner();
        debug!(
            instance_type = %self.instance_type,
            status = %status.status,
            run_progress = status.run_progress,
            total_runs = status.total_runs,
            elapsed_ms = start.elapsed().as_millis(),
            "gRPC health check passed"
        );

        Ok(())
    }

    /// Stream logs from the agent and forward them to the TUI channel
    pub async fn stream_to_channel(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

        self.stream_to_channel_inner(tx).await
    }

    /// Inner streaming logic without wait_for_ready - used for testing
    pub async fn stream_to_channel_inner(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
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
                    let msg = TuiMessage::ConsoleOutputAppend {
                        instance_type: self.instance_type.clone(),
                        line: log_entry.message,
                    };

                    if tx.send(msg).await.is_err() {
                        debug!(
                            instance_type = %self.instance_type,
                            "TUI channel closed, stopping log stream"
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

    /// Spawn the log streaming task in the background
    pub fn spawn_stream(self, tx: mpsc::Sender<TuiMessage>) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move { self.stream_to_channel(tx).await })
    }

    /// Stream logs from the agent and print to stdout (for no-TUI mode)
    pub async fn stream_to_stdout(&self) -> Result<()> {
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

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
            "Starting log stream to stdout"
        );

        let response = client
            .stream_logs(request)
            .await
            .context("Failed to start log stream")?;

        let mut stream = response.into_inner();

        while let Some(result) = stream.next().await {
            match result {
                Ok(log_entry) => {
                    println!("[{}] {}", self.instance_type, log_entry.message);
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

    /// Spawn the stdout streaming task in the background
    pub fn spawn_stream_stdout(self) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move { self.stream_to_stdout().await })
    }
}
