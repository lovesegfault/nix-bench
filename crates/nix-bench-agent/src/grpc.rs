//! gRPC server for real-time log streaming

use crate::config::Config;
use anyhow::Result;
use nix_bench_common::{StatusCode, TlsConfig};
use nix_bench_proto::{LogEntry, LogStream, LogStreamServer, StatusRequest, StatusResponse, StreamLogsRequest};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

/// Agent status for gRPC queries
#[derive(Debug, Clone, Default)]
pub struct AgentStatus {
    pub status: String,
    pub run_progress: u32,
    pub total_runs: u32,
    /// Build durations in seconds for completed runs
    pub durations: Vec<f64>,
}

/// Broadcasts log entries to all connected gRPC clients
pub struct LogBroadcaster {
    sender: broadcast::Sender<LogEntry>,
}

impl LogBroadcaster {
    /// Create a new broadcaster with the given channel capacity
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Broadcast a log entry to all connected clients
    pub fn broadcast(&self, timestamp: i64, message: String) {
        let entry = LogEntry { timestamp, message };
        // Ignore send errors (no subscribers)
        let _ = self.sender.send(entry);
    }

    /// Get a receiver for subscribing to log entries
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.sender.subscribe()
    }
}

/// gRPC service implementation for log streaming
pub struct LogStreamService {
    broadcaster: Arc<LogBroadcaster>,
    config: Arc<Config>,
    status: Arc<RwLock<AgentStatus>>,
    shutdown_token: CancellationToken,
    /// Counter for total dropped messages across all clients (for monitoring)
    dropped_messages: Arc<AtomicU64>,
}

impl LogStreamService {
    pub fn new(
        broadcaster: Arc<LogBroadcaster>,
        config: Arc<Config>,
        status: Arc<RwLock<AgentStatus>>,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self {
            broadcaster,
            config,
            status,
            shutdown_token,
            dropped_messages: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Get the total number of dropped messages across all clients
    pub fn total_dropped_messages(&self) -> u64 {
        self.dropped_messages.load(Ordering::Relaxed)
    }
}

type LogStreamResult = Pin<Box<dyn Stream<Item = Result<LogEntry, Status>> + Send>>;

#[tonic::async_trait]
impl LogStream for LogStreamService {
    type StreamLogsStream = LogStreamResult;

    async fn stream_logs(
        &self,
        request: Request<StreamLogsRequest>,
    ) -> Result<Response<Self::StreamLogsStream>, Status> {
        let req = request.into_inner();

        // Validate request matches this agent
        if req.instance_type != self.config.instance_type {
            return Err(Status::invalid_argument(format!(
                "Instance type mismatch: requested {} but this is {}",
                req.instance_type, self.config.instance_type
            )));
        }

        if req.run_id != self.config.run_id {
            return Err(Status::invalid_argument(format!(
                "Run ID mismatch: requested {} but this is {}",
                req.run_id, self.config.run_id
            )));
        }

        info!(
            instance_type = %req.instance_type,
            run_id = %req.run_id,
            "Client connected for log streaming"
        );

        let receiver = self.broadcaster.subscribe();
        let shutdown_token = self.shutdown_token.clone();
        let dropped_counter = self.dropped_messages.clone();
        let instance_type = req.instance_type.clone();

        // Convert broadcast receiver to a stream
        let stream = BroadcastStream::new(receiver);

        // Map broadcast results to gRPC results, tracking dropped messages
        let output_stream = tokio_stream::StreamExt::filter_map(stream, move |result| {
            match result {
                Ok(entry) => Some(Ok(entry)),
                Err(BroadcastStreamRecvError::Lagged(n)) => {
                    // Track dropped messages for monitoring
                    dropped_counter.fetch_add(n, Ordering::Relaxed);
                    warn!(
                        skipped = n,
                        instance_type = %instance_type,
                        total_dropped = dropped_counter.load(Ordering::Relaxed),
                        "Client lagged, log entries were skipped"
                    );
                    None
                }
            }
        });

        // Wrap stream to terminate on shutdown signal
        let shutdown_stream = async_stream::stream! {
            tokio::pin!(output_stream);
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_token.cancelled() => {
                        debug!("Shutdown signal received, terminating stream");
                        break;
                    }
                    item = tokio_stream::StreamExt::next(&mut output_stream) => {
                        match item {
                            Some(entry) => yield entry,
                            None => break,
                        }
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(shutdown_stream)))
    }

    async fn get_status(
        &self,
        _request: Request<StatusRequest>,
    ) -> Result<Response<StatusResponse>, Status> {
        let status = self.status.read().await;

        // Convert status string to canonical status code
        let status_code = StatusCode::from_str(&status.status)
            .map(|c| c.as_i32())
            .unwrap_or(0); // Default to pending if unknown

        Ok(Response::new(StatusResponse {
            status: status.status.clone(),
            run_progress: status.run_progress,
            total_runs: status.total_runs,
            dropped_log_count: self.dropped_messages.load(Ordering::Relaxed),
            durations: status.durations.clone(),
            status_code,
        }))
    }
}

/// Run the gRPC server on the specified port with optional TLS and graceful shutdown
///
/// # Arguments
/// * `port` - Port to listen on
/// * `broadcaster` - Log broadcaster for streaming
/// * `config` - Agent configuration
/// * `status` - Shared agent status
/// * `tls_config` - Optional TLS configuration for mTLS
/// * `shutdown_token` - Token for graceful shutdown signaling
///
/// # Returns
/// * `Ok(())` when server shuts down gracefully
/// * `Err` on server error
pub async fn run_grpc_server(
    port: u16,
    broadcaster: Arc<LogBroadcaster>,
    config: Arc<Config>,
    status: Arc<RwLock<AgentStatus>>,
    tls_config: Option<TlsConfig>,
    shutdown_token: CancellationToken,
) -> Result<()> {
    let addr = format!("0.0.0.0:{}", port).parse()?;

    let service = LogStreamService::new(broadcaster, config, status, shutdown_token.clone());

    let mut server = tonic::transport::Server::builder();

    // Configure TLS if provided
    if let Some(tls) = tls_config {
        let tls_config = tls.server_tls_config()?;
        info!(%addr, "Starting gRPC server with mTLS");
        server
            .tls_config(tls_config)?
            .add_service(LogStreamServer::new(service))
            .serve_with_shutdown(addr, shutdown_token.cancelled_owned())
            .await?;
    } else {
        debug!(%addr, "Starting gRPC server without TLS");
        server
            .add_service(LogStreamServer::new(service))
            .serve_with_shutdown(addr, shutdown_token.cancelled_owned())
            .await?;
    }

    info!("gRPC server shut down gracefully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    fn test_config() -> Config {
        Config {
            run_id: "test-run-123".to_string(),
            bucket: "test-bucket".to_string(),
            region: "us-east-2".to_string(),
            attr: "test-attr".to_string(),
            runs: 5,
            instance_type: "c6i.xlarge".to_string(),
            system: "x86_64-linux".to_string(),
            flake_ref: "github:test/test".to_string(),
            build_timeout: 7200,
            max_failures: 3,
            broadcast_capacity: 1024,
            grpc_port: 50051,
            require_tls: false,
            ca_cert_pem: None,
            agent_cert_pem: None,
            agent_key_pem: None,
        }
    }

    #[test]
    fn test_log_broadcaster_creation() {
        let broadcaster = LogBroadcaster::new(100);
        // Should be able to subscribe
        let _receiver = broadcaster.subscribe();
    }

    #[tokio::test]
    async fn test_log_broadcaster_single_subscriber() {
        let broadcaster = LogBroadcaster::new(100);
        let mut receiver = broadcaster.subscribe();

        // Broadcast a message
        broadcaster.broadcast(1000, "test message".to_string());

        // Subscriber should receive it
        let result = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok(), "Should receive message within timeout");

        let entry = result.unwrap().expect("Should have received an entry");
        assert_eq!(entry.timestamp, 1000);
        assert_eq!(entry.message, "test message");
    }

    #[tokio::test]
    async fn test_log_broadcaster_multiple_subscribers() {
        let broadcaster = LogBroadcaster::new(100);
        let mut receiver1 = broadcaster.subscribe();
        let mut receiver2 = broadcaster.subscribe();

        // Broadcast a message
        broadcaster.broadcast(2000, "shared message".to_string());

        // Both subscribers should receive it
        let result1 = timeout(Duration::from_millis(100), receiver1.recv()).await;
        let result2 = timeout(Duration::from_millis(100), receiver2.recv()).await;

        assert!(result1.is_ok());
        assert!(result2.is_ok());

        let entry1 = result1.unwrap().unwrap();
        let entry2 = result2.unwrap().unwrap();

        assert_eq!(entry1.message, "shared message");
        assert_eq!(entry2.message, "shared message");
    }

    #[tokio::test]
    async fn test_agent_status_default() {
        let status = AgentStatus::default();
        assert_eq!(status.status, "");
        assert_eq!(status.run_progress, 0);
        assert_eq!(status.total_runs, 0);
    }

    #[tokio::test]
    async fn test_get_status_returns_current_status() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus {
            status: "running".to_string(),
            run_progress: 3,
            total_runs: 10,
            durations: Vec::new(),
        }));

        let shutdown_token = CancellationToken::new();
        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        let request = Request::new(StatusRequest {});
        let response = service.get_status(request).await.unwrap();
        let inner = response.into_inner();

        assert_eq!(inner.status, "running");
        assert_eq!(inner.run_progress, 3);
        assert_eq!(inner.total_runs, 10);
        assert_eq!(inner.dropped_log_count, 0); // No dropped messages yet
    }

    #[tokio::test]
    async fn test_stream_logs_validates_instance_type() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        let request = Request::new(StreamLogsRequest {
            instance_type: "wrong-instance-type".to_string(),
            run_id: "test-run-123".to_string(),
        });

        let result = service.stream_logs(request).await;
        match result {
            Err(status) => {
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
                assert!(status.message().contains("Instance type mismatch"));
            }
            Ok(_) => panic!("Expected error for wrong instance type"),
        }
    }

    #[tokio::test]
    async fn test_stream_logs_accepts_valid_request() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        let request = Request::new(StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        });

        let result = service.stream_logs(request).await;
        assert!(result.is_ok(), "Valid request should be accepted");
    }
}
