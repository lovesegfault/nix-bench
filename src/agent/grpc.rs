//! gRPC server for real-time log streaming

use super::config::Config;
use crate::tls::TlsConfig;
use anyhow::Result;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};
use tokio_stream::Stream;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

// Include generated proto code
pub mod proto {
    tonic::include_proto!("nix_bench");
}

pub use proto::log_stream_server::{LogStream, LogStreamServer};
pub use proto::{LogEntry, StatusRequest, StatusResponse, StreamLogsRequest};

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
        let status_code = crate::status::StatusCode::from_str(&status.status)
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
    use crate::testing::agent_fixtures::test_config;
    use std::time::Duration;
    use tokio::time::timeout;

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
    async fn test_log_broadcaster_no_subscribers() {
        let broadcaster = LogBroadcaster::new(100);

        // Broadcasting with no subscribers should not panic
        broadcaster.broadcast(3000, "orphan message".to_string());

        // Now subscribe and send another
        let mut receiver = broadcaster.subscribe();
        broadcaster.broadcast(3001, "received message".to_string());

        let result = timeout(Duration::from_millis(100), receiver.recv()).await;
        assert!(result.is_ok());
        let entry = result.unwrap().unwrap();
        assert_eq!(entry.message, "received message");
    }

    #[tokio::test]
    async fn test_log_broadcaster_multiple_messages() {
        let broadcaster = LogBroadcaster::new(100);
        let mut receiver = broadcaster.subscribe();

        // Send multiple messages
        for i in 0..5 {
            broadcaster.broadcast(i * 1000, format!("message {}", i));
        }

        // Receive all messages
        for i in 0..5 {
            let result = timeout(Duration::from_millis(100), receiver.recv()).await;
            assert!(result.is_ok(), "Should receive message {}", i);
            let entry = result.unwrap().unwrap();
            assert_eq!(entry.message, format!("message {}", i));
        }
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
    async fn test_stream_logs_validates_run_id() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(broadcaster, config, status, shutdown_token);

        let request = Request::new(StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "wrong-run-id".to_string(),
        });

        let result = service.stream_logs(request).await;
        match result {
            Err(status) => {
                assert_eq!(status.code(), tonic::Code::InvalidArgument);
                assert!(status.message().contains("Run ID mismatch"));
            }
            Ok(_) => panic!("Expected error for wrong run ID"),
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

    #[tokio::test]
    async fn test_stream_logs_receives_broadcast_messages() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(Arc::clone(&broadcaster), config, status, shutdown_token);

        let request = Request::new(StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        });

        // Start streaming
        let response = service.stream_logs(request).await.unwrap();
        let mut stream = response.into_inner();

        // Broadcast a message
        broadcaster.broadcast(12345, "test log line".to_string());

        // Stream should receive it
        use tokio_stream::StreamExt;
        let result = timeout(Duration::from_millis(500), stream.next()).await;
        assert!(result.is_ok(), "Should receive message within timeout");

        let entry = result.unwrap().unwrap().unwrap();
        assert_eq!(entry.timestamp, 12345);
        assert_eq!(entry.message, "test log line");
    }

    #[tokio::test]
    async fn test_status_updates_reflected_in_get_status() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus {
            status: "starting".to_string(),
            run_progress: 0,
            total_runs: 5,
            durations: Vec::new(),
        }));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(broadcaster, Arc::clone(&config), Arc::clone(&status), shutdown_token);

        // Initial status
        let response = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap();
        assert_eq!(response.into_inner().status, "starting");

        // Update status
        {
            let mut s = status.write().await;
            s.status = "running".to_string();
            s.run_progress = 3;
        }

        // Check updated status
        let response = service
            .get_status(Request::new(StatusRequest {}))
            .await
            .unwrap();
        let inner = response.into_inner();
        assert_eq!(inner.status, "running");
        assert_eq!(inner.run_progress, 3);
    }

    #[tokio::test]
    async fn test_log_broadcaster_lagged_subscriber_continues() {
        // Create a broadcaster with very small capacity to trigger lagging
        let broadcaster = LogBroadcaster::new(2);
        let mut receiver = broadcaster.subscribe();

        // Flood the channel with more messages than capacity (causes lag)
        for i in 0..10 {
            broadcaster.broadcast(i * 1000, format!("flood message {}", i));
        }

        // First receive will get a Lagged error (messages were dropped)
        let result = receiver.try_recv();
        assert!(
            result.is_err(),
            "Should get lagged error after overflow: {:?}",
            result
        );

        // Subscribe fresh and verify new messages work after overflow
        let mut new_receiver = broadcaster.subscribe();
        broadcaster.broadcast(99999, "after flood".to_string());

        let result = timeout(Duration::from_millis(100), new_receiver.recv()).await;
        assert!(result.is_ok(), "Should receive message after flood");
        let entry = result.unwrap().unwrap();
        assert_eq!(entry.message, "after flood");
    }

    #[tokio::test]
    async fn test_stream_logs_handles_lagged_client_gracefully() {
        // Create broadcaster with tiny capacity
        let broadcaster = Arc::new(LogBroadcaster::new(2));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(Arc::clone(&broadcaster), config, status, shutdown_token);

        let request = Request::new(StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        });

        // Start streaming
        let response = service.stream_logs(request).await.unwrap();
        let mut stream = response.into_inner();

        // Flood the broadcaster to cause lagging (more msgs than capacity)
        for i in 0..10 {
            broadcaster.broadcast(i * 1000, format!("flood {}", i));
        }

        // Stream should still work - lagged messages are filtered out (return None)
        // Send a new message that should be received
        broadcaster.broadcast(88888, "after lag".to_string());

        // The stream filters out Lagged errors, so we should eventually get messages
        use tokio_stream::StreamExt;
        let result = timeout(Duration::from_millis(500), stream.next()).await;

        // We expect to receive something (either a flood message or "after lag")
        // The key point is the stream doesn't error out - it continues working
        assert!(
            result.is_ok(),
            "Stream should continue after lagged messages"
        );
    }
}

/// Integration tests requiring actual gRPC server/client communication
#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::testing::agent_fixtures::test_config;
    use crate::testing::test_utils::find_available_port;
    use std::net::SocketAddr;
    use std::time::Duration;
    use tokio::time::timeout;
    use tonic::transport::Channel;

    /// Wait for server to be ready by polling TCP connection
    async fn wait_for_server_ready(addr: SocketAddr, max_wait: Duration) {
        let start = std::time::Instant::now();
        while start.elapsed() < max_wait {
            match tokio::net::TcpStream::connect(addr).await {
                Ok(_) => return, // Server is ready
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
        panic!("Server did not become ready within {:?}", max_wait);
    }

    /// Start a gRPC server for testing and return the address
    async fn start_test_server(
        broadcaster: Arc<LogBroadcaster>,
        config: Arc<Config>,
        status: Arc<RwLock<AgentStatus>>,
    ) -> SocketAddr {
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

        // Wait for server to be ready (poll instead of fixed sleep)
        wait_for_server_ready(addr, Duration::from_secs(5)).await;

        addr
    }

    #[tokio::test]
    async fn test_server_starts_and_accepts_connections() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));

        let addr = start_test_server(broadcaster, config, status).await;

        // Try to connect
        let endpoint = format!("http://{}", addr);
        let result = Channel::from_shared(endpoint)
            .unwrap()
            .connect_timeout(Duration::from_secs(5))
            .connect()
            .await;

        assert!(result.is_ok(), "Should be able to connect to server");
    }

    #[tokio::test]
    async fn test_client_can_get_status() {
        use proto::log_stream_client::LogStreamClient;

        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus {
            status: "running".to_string(),
            run_progress: 2,
            total_runs: 10,
            durations: Vec::new(),
        }));

        let addr = start_test_server(broadcaster, config, status).await;

        // Connect client
        let endpoint = format!("http://{}", addr);
        let channel = Channel::from_shared(endpoint)
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = LogStreamClient::new(channel);

        // Get status
        let response = client.get_status(StatusRequest {}).await.unwrap();
        let inner = response.into_inner();

        assert_eq!(inner.status, "running");
        assert_eq!(inner.run_progress, 2);
        assert_eq!(inner.total_runs, 10);
        assert_eq!(inner.dropped_log_count, 0); // No dropped messages yet
    }

    #[tokio::test]
    async fn test_client_can_stream_logs() {
        use proto::log_stream_client::LogStreamClient;
        use tokio_stream::StreamExt;

        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));

        let addr = start_test_server(Arc::clone(&broadcaster), config, status).await;

        // Connect client
        let endpoint = format!("http://{}", addr);
        let channel = Channel::from_shared(endpoint)
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = LogStreamClient::new(channel);

        // Start streaming
        let request = StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        };

        let response = client.stream_logs(request).await.unwrap();
        let mut stream = response.into_inner();

        // Broadcast messages
        for i in 0..3 {
            broadcaster.broadcast(i * 1000, format!("log line {}", i));
        }

        // Receive messages
        for i in 0..3 {
            let result = timeout(Duration::from_secs(1), stream.next()).await;
            assert!(
                result.is_ok(),
                "Should receive message {} within timeout",
                i
            );

            let entry = result.unwrap().unwrap().unwrap();
            assert_eq!(entry.message, format!("log line {}", i));
        }
    }

    #[tokio::test]
    async fn test_client_receives_invalid_argument_for_wrong_instance() {
        use proto::log_stream_client::LogStreamClient;

        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));

        let addr = start_test_server(broadcaster, config, status).await;

        // Connect client
        let endpoint = format!("http://{}", addr);
        let channel = Channel::from_shared(endpoint)
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client = LogStreamClient::new(channel);

        // Try to stream with wrong instance type
        let request = StreamLogsRequest {
            instance_type: "wrong-instance".to_string(),
            run_id: "test-run-123".to_string(),
        };

        let result = client.stream_logs(request).await;
        assert!(result.is_err());

        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn test_connection_retry_fails_when_server_unavailable() {
        // Try to connect to a port where nothing is listening
        let port = find_available_port().await;
        let endpoint = format!("http://127.0.0.1:{}", port);

        let result = Channel::from_shared(endpoint)
            .unwrap()
            .connect_timeout(Duration::from_millis(100))
            .connect()
            .await;

        assert!(result.is_err(), "Connection should fail when no server");
    }

    #[tokio::test]
    async fn test_multiple_clients_can_stream_simultaneously() {
        use proto::log_stream_client::LogStreamClient;
        use tokio_stream::StreamExt;

        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let config = Arc::new(test_config());
        let status = Arc::new(RwLock::new(AgentStatus::default()));

        let addr = start_test_server(Arc::clone(&broadcaster), config, status).await;
        let endpoint = format!("http://{}", addr);

        // Create two clients
        let channel1 = Channel::from_shared(endpoint.clone())
            .unwrap()
            .connect()
            .await
            .unwrap();
        let channel2 = Channel::from_shared(endpoint)
            .unwrap()
            .connect()
            .await
            .unwrap();

        let mut client1 = LogStreamClient::new(channel1);
        let mut client2 = LogStreamClient::new(channel2);

        let request = StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        };

        // Start streaming on both clients
        let response1 = client1.stream_logs(request.clone()).await.unwrap();
        let response2 = client2.stream_logs(request).await.unwrap();

        let mut stream1 = response1.into_inner();
        let mut stream2 = response2.into_inner();

        // Broadcast a message
        broadcaster.broadcast(9999, "shared message".to_string());

        // Both clients should receive it
        let result1 = timeout(Duration::from_secs(1), stream1.next()).await;
        let result2 = timeout(Duration::from_secs(1), stream2.next()).await;

        assert!(result1.is_ok());
        assert!(result2.is_ok());

        let entry1 = result1.unwrap().unwrap().unwrap();
        let entry2 = result2.unwrap().unwrap().unwrap();

        assert_eq!(entry1.message, "shared message");
        assert_eq!(entry2.message, "shared message");
    }
}
