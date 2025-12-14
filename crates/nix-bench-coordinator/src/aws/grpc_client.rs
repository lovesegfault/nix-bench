//! gRPC client for streaming logs from benchmark agents

use crate::tui::TuiMessage;
use anyhow::{Context, Result};
use nix_bench_common::TlsConfig;
use nix_bench_proto::{LogStreamClient, StatusRequest, StreamLogsRequest};
use rand::Rng;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};

/// Add 0-25% jitter to a duration to prevent thundering herd
fn jittered_delay(base: Duration) -> Duration {
    let mut rng = rand::thread_rng();
    let jitter_factor = 1.0 + rng.gen_range(0.0..0.25);
    Duration::from_secs_f64(base.as_secs_f64() * jitter_factor)
}

// ============================================================================
// GrpcChannelBuilder - Unified channel construction
// ============================================================================

/// Options for building a gRPC channel
#[derive(Debug, Clone)]
pub struct ChannelOptions {
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Request timeout
    pub request_timeout: Duration,
}

impl Default for ChannelOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl ChannelOptions {
    /// Quick connect options for polling (shorter timeouts)
    pub fn for_polling() -> Self {
        Self {
            connect_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(5),
        }
    }

    /// Readiness check options
    pub fn for_readiness() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
        }
    }
}

/// Unified gRPC channel builder
///
/// Consolidates TLS and non-TLS channel construction into a single builder.
pub struct GrpcChannelBuilder<'a> {
    endpoint: String,
    tls_config: Option<&'a TlsConfig>,
    options: ChannelOptions,
}

impl<'a> GrpcChannelBuilder<'a> {
    /// Create a new channel builder for the given host and port
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            endpoint: format!("http://{}:{}", host, port),
            tls_config: None,
            options: ChannelOptions::default(),
        }
    }

    /// Enable TLS for this channel
    pub fn with_tls(mut self, tls_config: &'a TlsConfig) -> Self {
        self.endpoint = self.endpoint.replace("http://", "https://");
        self.tls_config = Some(tls_config);
        self
    }

    /// Set custom channel options
    pub fn with_options(mut self, options: ChannelOptions) -> Self {
        self.options = options;
        self
    }

    /// Get the endpoint URL
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Build and connect the channel
    pub async fn connect(self) -> Result<Channel> {
        let mut endpoint_builder = Channel::from_shared(self.endpoint.clone())
            .context("Invalid endpoint")?
            .connect_timeout(self.options.connect_timeout)
            .timeout(self.options.request_timeout);

        if let Some(tls) = self.tls_config {
            let tls_config = tls.client_tls_config().context("Failed to create TLS config")?;
            endpoint_builder = endpoint_builder
                .tls_config(tls_config)
                .context("Failed to configure TLS")?;
        }

        endpoint_builder.connect().await.context("gRPC connection failed")
    }

    /// Try to build and connect, returning None on failure (for polling)
    pub async fn try_connect(self) -> Option<Channel> {
        self.connect().await.ok()
    }
}

// ============================================================================
// LogStreamingOptions - Unified log streaming configuration
// ============================================================================

/// Output destination for log streaming
#[derive(Clone)]
pub enum LogOutput {
    /// Send to TUI via channel
    Channel(mpsc::Sender<TuiMessage>),
    /// Print to stdout with instance prefix
    Stdout,
}

/// Options for starting log streaming
#[derive(Clone)]
pub struct LogStreamingOptions {
    /// Instance type and IP pairs
    pub instances: Vec<(String, String)>,
    /// Run ID for filtering
    pub run_id: String,
    /// gRPC port
    pub port: u16,
    /// Optional TLS configuration
    pub tls_config: Option<TlsConfig>,
    /// Output destination
    pub output: LogOutput,
}

impl LogStreamingOptions {
    /// Create new options for the given instances
    pub fn new(instances: &[(String, String)], run_id: &str, port: u16) -> Self {
        Self {
            instances: instances.to_vec(),
            run_id: run_id.to_string(),
            port,
            tls_config: None,
            output: LogOutput::Stdout,
        }
    }

    /// Enable TLS
    pub fn with_tls(mut self, tls: TlsConfig) -> Self {
        self.tls_config = Some(tls);
        self
    }

    /// Set output to TUI channel
    pub fn with_channel(mut self, tx: mpsc::Sender<TuiMessage>) -> Self {
        self.output = LogOutput::Channel(tx);
        self
    }
}

/// Start log streaming for multiple instances (unified function)
///
/// Spawns a background task for each instance that streams logs via gRPC.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming_unified(
    options: LogStreamingOptions,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in options.instances {
        let client = if let Some(ref tls) = options.tls_config {
            GrpcLogClient::new_with_tls(
                &instance_type,
                &public_ip,
                options.port,
                &options.run_id,
                tls.clone(),
            )
        } else {
            GrpcLogClient::new(&instance_type, &public_ip, options.port, &options.run_id)
        };

        let handle = match &options.output {
            LogOutput::Channel(tx) => client.spawn_stream(tx.clone()),
            LogOutput::Stdout => client.spawn_stream_stdout(),
        };

        handles.push(handle);
    }

    handles
}

/// Fast TCP connect check - use in tests for faster execution
///
/// This performs a simple TCP connection attempt to verify the server is listening.
/// It's much faster than a full gRPC health check since it doesn't require
/// TLS handshake or RPC setup.
///
/// # Arguments
/// * `addr` - Socket address to connect to (e.g., "127.0.0.1:50051")
/// * `timeout` - Maximum time to wait for connection
///
/// # Returns
/// * `Ok(())` if TCP connection succeeds
/// * `Err` if connection fails or times out
pub async fn wait_for_tcp_ready(addr: &str, timeout: Duration) -> Result<()> {
    let start = std::time::Instant::now();
    let poll_interval = Duration::from_millis(10);

    while start.elapsed() < timeout {
        match tokio::net::TcpStream::connect(addr).await {
            Ok(_) => return Ok(()),
            Err(_) => tokio::time::sleep(poll_interval).await,
        }
    }

    Err(anyhow::anyhow!(
        "Server at {} not ready after {:?}",
        addr,
        timeout
    ))
}

/// gRPC log streaming client for connecting to benchmark agents
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
    /// Optional TLS configuration for mTLS
    tls_config: Option<TlsConfig>,
}

impl GrpcLogClient {
    /// Create a new gRPC log client (without TLS - insecure)
    pub fn new(instance_type: &str, public_ip: &str, port: u16, run_id: &str) -> Self {
        Self {
            instance_type: instance_type.to_string(),
            public_ip: public_ip.to_string(),
            port,
            run_id: run_id.to_string(),
            tls_config: None,
        }
    }

    /// Create a new gRPC log client with TLS configuration
    pub fn new_with_tls(
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
            tls_config: Some(tls_config),
        }
    }

    /// Create a channel builder for this client
    pub fn channel_builder(&self) -> GrpcChannelBuilder<'_> {
        let mut builder = GrpcChannelBuilder::new(&self.public_ip, self.port);
        if let Some(ref tls) = self.tls_config {
            builder = builder.with_tls(tls);
        }
        builder
    }

    /// Connect to the agent with retry logic
    ///
    /// Uses exponential backoff with jitter for retries.
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
                tls = self.tls_config.is_some(),
                "Attempting gRPC connection"
            );

            match builder.connect().await {
                Ok(channel) => {
                    info!(
                        instance_type = %self.instance_type,
                        endpoint = %endpoint,
                        tls = self.tls_config.is_some(),
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

                    warn!(
                        instance_type = %self.instance_type,
                        endpoint = %endpoint,
                        attempt = attempts,
                        max_retries = max_retries,
                        error = %e,
                        delay_ms = jittered.as_millis(),
                        "gRPC connection failed, retrying"
                    );

                    tokio::time::sleep(jittered).await;

                    // Exponential backoff with cap at 30 seconds
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
    }

    /// Wait for the agent to be ready by polling the GetStatus RPC
    ///
    /// This waits for the agent to start and be responsive before streaming logs.
    /// Uses exponential backoff between attempts. Respects TLS configuration.
    ///
    /// # Arguments
    /// * `max_wait` - Maximum time to wait for agent readiness
    /// * `initial_delay` - Initial delay before first check (to let agent bootstrap)
    pub async fn wait_for_ready(&self, max_wait: Duration, initial_delay: Duration) -> Result<()> {
        info!(
            instance_type = %self.instance_type,
            initial_delay_secs = initial_delay.as_secs(),
            max_wait_secs = max_wait.as_secs(),
            tls = self.tls_config.is_some(),
            "Waiting for agent to be ready"
        );

        // Initial delay to let the agent bootstrap
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

            // Try to connect using the channel builder
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

    /// Full RPC health check - use in production
    ///
    /// This performs a complete gRPC health check by:
    /// 1. Establishing a gRPC connection (with TLS if configured)
    /// 2. Calling the GetStatus RPC
    /// 3. Verifying the response is valid
    ///
    /// Unlike `wait_for_ready()`, this does not use exponential backoff or
    /// initial delays - it's a single-shot check with timeout.
    ///
    /// # Arguments
    /// * `timeout` - Maximum time to wait for the health check to complete
    ///
    /// # Returns
    /// * `Ok(())` if the agent responds to GetStatus
    /// * `Err` if connection fails, times out, or RPC fails
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

        // Build channel using the builder with custom timeout
        let channel = builder
            .with_options(ChannelOptions {
                connect_timeout: timeout,
                request_timeout: timeout,
            })
            .connect()
            .await?;

        let mut client = LogStreamClient::new(channel);

        // Call GetStatus with remaining timeout
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
    ///
    /// This function will:
    /// 1. Wait for agent to be ready (with initial delay for bootstrap)
    /// 2. Connect to the agent with retry
    /// 3. Start streaming logs
    /// 4. Forward each log entry as a TuiMessage::ConsoleOutput
    /// 5. Accumulate logs to build the full output
    ///
    /// Returns when the stream ends or an error occurs.
    pub async fn stream_to_channel(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
        // Wait for agent to be ready before attempting to stream
        // Initial delay of 30s to let cloud-init/Nix install complete
        // Max wait of 5 minutes for full bootstrap
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

        self.stream_to_channel_inner(tx).await
    }

    /// Inner streaming logic without wait_for_ready - used for testing
    async fn stream_to_channel_inner(&self, tx: mpsc::Sender<TuiMessage>) -> Result<()> {
        // Now connect with increased retry budget (30 retries covers edge cases)
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
                    // Send incremental update to TUI (avoids O(n²) memory copies)
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

        info!(
            instance_type = %self.instance_type,
            "Log stream completed"
        );

        Ok(())
    }

    /// Spawn the log streaming task in the background
    ///
    /// Returns a JoinHandle for the spawned task.
    pub fn spawn_stream(self, tx: mpsc::Sender<TuiMessage>) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move { self.stream_to_channel(tx).await })
    }

    /// Stream logs from the agent and print to stdout (for no-TUI mode)
    ///
    /// This function will:
    /// 1. Wait for agent to be ready (with initial delay for bootstrap)
    /// 2. Connect to the agent with retry
    /// 3. Start streaming logs
    /// 4. Print each log entry to stdout with instance prefix
    ///
    /// Returns when the stream ends or an error occurs.
    pub async fn stream_to_stdout(&self) -> Result<()> {
        // Wait for agent to be ready before attempting to stream
        // Initial delay of 30s to let cloud-init/Nix install complete
        // Max wait of 5 minutes for full bootstrap
        self.wait_for_ready(Duration::from_secs(300), Duration::from_secs(30))
            .await
            .context("Agent not ready for streaming")?;

        // Now connect with increased retry budget (30 retries covers edge cases)
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
                    // Print to stdout with instance prefix
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

        info!(
            instance_type = %self.instance_type,
            "Log stream completed"
        );

        Ok(())
    }

    /// Spawn the stdout streaming task in the background (for no-TUI mode)
    ///
    /// Returns a JoinHandle for the spawned task.
    pub fn spawn_stream_stdout(self) -> tokio::task::JoinHandle<Result<()>> {
        tokio::spawn(async move { self.stream_to_stdout().await })
    }
}

/// Start log streaming for multiple instances
///
/// Spawns a background task for each instance that streams logs via gRPC.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming(
    instances: &[(String, String)], // (instance_type, public_ip)
    run_id: &str,
    port: u16,
    tx: mpsc::Sender<TuiMessage>,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in instances {
        let client = GrpcLogClient::new(instance_type, public_ip, port, run_id);
        let handle = client.spawn_stream(tx.clone());
        handles.push(handle);
    }

    handles
}

/// Start log streaming for multiple instances to stdout (no-TUI mode)
///
/// Spawns a background task for each instance that streams logs via gRPC
/// and prints them to stdout with instance prefixes.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming_stdout(
    instances: &[(String, String)], // (instance_type, public_ip)
    run_id: &str,
    port: u16,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in instances {
        let client = GrpcLogClient::new(instance_type, public_ip, port, run_id);
        let handle = client.spawn_stream_stdout();
        handles.push(handle);
    }

    handles
}

/// Start log streaming for multiple instances with TLS
///
/// Spawns a background task for each instance that streams logs via gRPC with mTLS.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming_with_tls(
    instances: &[(String, String)], // (instance_type, public_ip)
    run_id: &str,
    port: u16,
    tls_config: TlsConfig,
    tx: mpsc::Sender<TuiMessage>,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in instances {
        let client =
            GrpcLogClient::new_with_tls(instance_type, public_ip, port, run_id, tls_config.clone());
        let handle = client.spawn_stream(tx.clone());
        handles.push(handle);
    }

    handles
}

/// Start log streaming for multiple instances to stdout with TLS (no-TUI mode)
///
/// Spawns a background task for each instance that streams logs via gRPC with mTLS
/// and prints them to stdout with instance prefixes.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming_stdout_with_tls(
    instances: &[(String, String)], // (instance_type, public_ip)
    run_id: &str,
    port: u16,
    tls_config: TlsConfig,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in instances {
        let client =
            GrpcLogClient::new_with_tls(instance_type, public_ip, port, run_id, tls_config.clone());
        let handle = client.spawn_stream_stdout();
        handles.push(handle);
    }

    handles
}

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
}

/// Polls status from multiple gRPC agents
pub struct GrpcStatusPoller {
    /// Map of instance_type to (public_ip, port)
    instances: Vec<(String, String, u16)>,
    /// Optional TLS configuration
    tls_config: Option<TlsConfig>,
}

impl GrpcStatusPoller {
    /// Create a new status poller for the given instances
    pub fn new(instances: &[(String, String)], port: u16) -> Self {
        Self {
            instances: instances
                .iter()
                .map(|(t, ip)| (t.clone(), ip.clone(), port))
                .collect(),
            tls_config: None,
        }
    }

    /// Create a new status poller with TLS configuration
    pub fn new_with_tls(instances: &[(String, String)], port: u16, tls_config: TlsConfig) -> Self {
        Self {
            instances: instances
                .iter()
                .map(|(t, ip)| (t.clone(), ip.clone(), port))
                .collect(),
            tls_config: Some(tls_config),
        }
    }

    /// Poll status from all instances
    ///
    /// Returns a map of instance_type to status. Failed connections are logged
    /// but don't fail the overall poll - missing entries indicate unreachable agents.
    pub async fn poll_status(
        &self,
    ) -> std::collections::HashMap<String, GrpcInstanceStatus> {
        use std::collections::HashMap;

        let mut results = HashMap::new();

        for (instance_type, public_ip, port) in &self.instances {
            // Build channel using the builder with polling options
            let mut builder = GrpcChannelBuilder::new(public_ip, *port)
                .with_options(ChannelOptions::for_polling());

            if let Some(ref tls) = self.tls_config {
                builder = builder.with_tls(tls);
            }

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
            match client.get_status(nix_bench_proto::StatusRequest {}).await {
                Ok(response) => {
                    let status = response.into_inner();
                    // Use the new status_code field if available, fall back to string parsing
                    let status_code = if status.status_code != 0 {
                        Some(status.status_code)
                    } else {
                        // Legacy: parse from status string
                        nix_bench_common::StatusCode::from_str(&status.status)
                            .map(|c| c.as_i32())
                    };
                    results.insert(
                        instance_type.clone(),
                        GrpcInstanceStatus {
                            status: status_code,
                            run_progress: Some(status.run_progress),
                            total_runs: Some(status.total_runs),
                            durations: status.durations,
                            dropped_log_count: status.dropped_log_count,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grpc_log_client_creation() {
        let client = GrpcLogClient::new("c6i.xlarge", "192.168.1.100", 50051, "run-123");

        assert_eq!(client.instance_type, "c6i.xlarge");
        assert_eq!(client.public_ip, "192.168.1.100");
        assert_eq!(client.port, 50051);
        assert_eq!(client.run_id, "run-123");
    }

    #[test]
    fn test_grpc_log_client_endpoint() {
        let client = GrpcLogClient::new("c6i.xlarge", "10.0.0.1", 50051, "run-456");
        assert_eq!(client.channel_builder().endpoint(), "http://10.0.0.1:50051");
    }

    #[test]
    fn test_grpc_log_client_endpoint_with_different_port() {
        let client = GrpcLogClient::new("c6g.xlarge", "192.168.1.50", 9000, "run-789");
        assert_eq!(
            client.channel_builder().endpoint(),
            "http://192.168.1.50:9000"
        );
    }

    #[tokio::test]
    async fn test_connect_with_retry_fails_after_max_retries() {
        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", 59999, "test-run");

        // Try to connect to a port where nothing is listening
        // Use minimal retries and delay for fast test
        let result = client
            .connect_with_retry(2, Duration::from_millis(10))
            .await;

        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("Failed to connect"));
        assert!(err_msg.contains("2 attempts"));
    }

    #[tokio::test]
    async fn test_start_log_streaming_creates_handles_for_all_instances() {
        // We can't actually run the streaming without a server,
        // but we can verify the handles are created
        let instances = vec![
            ("c6i.xlarge".to_string(), "10.0.0.1".to_string()),
            ("c6g.xlarge".to_string(), "10.0.0.2".to_string()),
            ("c7i.xlarge".to_string(), "10.0.0.3".to_string()),
        ];

        let (tx, _rx) = mpsc::channel(100);

        // Use the unified log streaming function
        let options = LogStreamingOptions::new(&instances, "test-run", 50051)
            .with_channel(tx);
        let handles = start_log_streaming_unified(options);

        assert_eq!(handles.len(), 3);

        // Cancel all handles to clean up
        for handle in handles {
            handle.abort();
        }
    }
}

/// Integration tests for gRPC client with actual server
/// These tests require the agent feature to be enabled since they use agent types
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::agent::config::Config;
    use crate::agent::grpc::{AgentStatus, LogBroadcaster, LogStreamServer, LogStreamService};
    use crate::testing::agent_fixtures::test_config;
    use crate::testing::test_utils::find_available_port;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    /// Start a gRPC server for testing and return the port
    async fn start_test_server(
        broadcaster: Arc<LogBroadcaster>,
        config: Arc<Config>,
        status: Arc<tokio::sync::RwLock<AgentStatus>>,
    ) -> u16 {
        let port = find_available_port().await;
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(LogStreamServer::new(service))
                .serve(addr)
                .await
                .unwrap();
        });

        // Wait for server to be ready using the fast TCP check
        wait_for_tcp_ready(&addr.to_string(), Duration::from_secs(5))
            .await
            .expect("Test server should be ready");

        port
    }

    #[tokio::test]
    async fn test_connect_with_retry_succeeds_when_server_available() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(tokio::sync::RwLock::new(AgentStatus::default()));

        let port = start_test_server(broadcaster, config, status).await;

        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", port, "test-run-123");

        let result = client
            .connect_with_retry(3, Duration::from_millis(100))
            .await;

        assert!(
            result.is_ok(),
            "Should connect successfully to running server"
        );
    }

    #[tokio::test]
    async fn test_stream_to_channel_receives_logs() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(tokio::sync::RwLock::new(AgentStatus::default()));

        let port = start_test_server(Arc::clone(&broadcaster), config, status).await;

        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", port, "test-run-123");

        let (tx, mut rx) = mpsc::channel(100);

        // Spawn streaming task (use inner method to skip wait_for_ready)
        let stream_handle = tokio::spawn({
            let client = client.clone();
            async move { client.stream_to_channel_inner(tx).await }
        });

        // Give time for connection to establish
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Broadcast some messages
        broadcaster.broadcast(1000, "[stdout] Building package...".to_string());
        broadcaster.broadcast(2000, "[stdout] Build complete".to_string());

        // Wait for messages (now using incremental ConsoleOutputAppend)
        let msg1 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("Should receive first message")
            .expect("Channel should not be closed");

        if let TuiMessage::ConsoleOutputAppend {
            instance_type,
            line,
        } = msg1
        {
            assert_eq!(instance_type, "c6i.xlarge");
            assert!(line.contains("Building package"));
        } else {
            panic!("Expected ConsoleOutputAppend message, got {:?}", msg1);
        }

        let msg2 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("Should receive second message")
            .expect("Channel should not be closed");

        if let TuiMessage::ConsoleOutputAppend {
            instance_type,
            line,
        } = msg2
        {
            assert_eq!(instance_type, "c6i.xlarge");
            // Each message is now incremental (just one line)
            assert!(line.contains("Build complete"));
        } else {
            panic!("Expected ConsoleOutputAppend message, got {:?}", msg2);
        }

        // Clean up
        stream_handle.abort();
    }

    #[tokio::test]
    async fn test_stream_to_channel_exits_gracefully_on_channel_close() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(tokio::sync::RwLock::new(AgentStatus::default()));

        let port = start_test_server(Arc::clone(&broadcaster), config, status).await;

        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", port, "test-run-123");

        let (tx, rx) = mpsc::channel(100);

        // Spawn streaming task
        let stream_handle = tokio::spawn({
            let client = client.clone();
            async move { client.stream_to_channel_inner(tx).await }
        });

        // Give time for connection to establish
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Broadcast a message to verify streaming is working
        broadcaster.broadcast(1000, "test message".to_string());
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop the receiver - this should cause the streaming task to exit gracefully
        drop(rx);

        // Send more messages after receiver is dropped
        broadcaster.broadcast(2000, "after drop".to_string());

        // The streaming task should exit gracefully (not hang or panic)
        let result = tokio::time::timeout(Duration::from_secs(2), stream_handle).await;

        assert!(
            result.is_ok(),
            "Streaming task should exit within timeout after channel closure"
        );

        // The task should complete without panic
        let inner_result = result.unwrap();
        assert!(
            inner_result.is_ok(),
            "Streaming task should not panic: {:?}",
            inner_result
        );

        // The streaming function should return Ok (graceful exit, not error)
        let stream_result = inner_result.unwrap();
        assert!(
            stream_result.is_ok(),
            "Stream should exit gracefully on channel close"
        );
    }

    #[tokio::test]
    async fn test_connect_with_retry_retries_on_failure_then_succeeds() {
        // First, find a port
        let port = find_available_port().await;

        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", port, "test-run-123");

        // Start connection attempt in background - will fail initially
        let connect_handle = tokio::spawn({
            let client = client.clone();
            async move {
                client
                    .connect_with_retry(10, Duration::from_millis(100))
                    .await
            }
        });

        // Wait a bit, then start the server
        tokio::time::sleep(Duration::from_millis(250)).await;

        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(tokio::sync::RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        // Start server on the same port
        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(LogStreamServer::new(service))
                .serve(addr)
                .await
                .unwrap();
        });

        // Connection should eventually succeed
        let result = tokio::time::timeout(Duration::from_secs(5), connect_handle)
            .await
            .expect("Should complete within timeout")
            .expect("Task should not panic");

        assert!(
            result.is_ok(),
            "Should eventually connect after server starts"
        );
    }
}
