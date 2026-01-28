//! gRPC server for real-time log streaming

use anyhow::Result;
use nix_bench_common::{Architecture, RunResult, TlsConfig};
use nix_bench_proto::{
    AckCompleteRequest, AckCompleteResponse, CancelBenchmarkRequest, CancelBenchmarkResponse,
    LogEntry, LogStream, LogStreamServer, RunResult as ProtoRunResult,
    StatusCode as ProtoStatusCode, StatusRequest, StatusResponse, StreamLogsRequest,
};
use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast};
use tokio_stream::Stream;
use tokio_stream::wrappers::{BroadcastStream, errors::BroadcastStreamRecvError};
use tokio_util::sync::CancellationToken;
use tonic::{Request, Response, Status};
use tracing::{debug, info, warn};

// Re-export StatusCode from common for convenience
pub use nix_bench_common::StatusCode;

/// Agent status for gRPC queries
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub status: StatusCode,
    pub run_progress: u32,
    pub total_runs: u32,
    /// Detailed run results with success/failure
    pub run_results: Vec<RunResult>,
    /// Nix attribute being built
    pub attr: String,
    /// System architecture
    pub system: Architecture,
    /// Error message (populated when status is Failed)
    pub error_message: String,
}

impl Default for AgentStatus {
    fn default() -> Self {
        Self {
            status: StatusCode::Pending,
            run_progress: 0,
            total_runs: 0,
            run_results: Vec::new(),
            attr: String::new(),
            system: Architecture::X86_64,
            error_message: String::new(),
        }
    }
}

/// Maximum number of messages to buffer for late subscribers
const LOG_BUFFER_SIZE: usize = 1000;

/// Broadcasts log entries to all connected gRPC clients
pub struct LogBroadcaster {
    sender: broadcast::Sender<LogEntry>,
    /// Buffer for replaying messages to late subscribers
    buffer: Mutex<VecDeque<LogEntry>>,
}

impl LogBroadcaster {
    /// Create a new broadcaster with the given channel capacity
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            buffer: Mutex::new(VecDeque::with_capacity(LOG_BUFFER_SIZE)),
        }
    }

    /// Broadcast a log entry to all connected clients
    pub fn broadcast(&self, timestamp: i64, message: String) {
        let entry = LogEntry { timestamp, message };

        // Buffer the message for late subscribers (non-blocking try_lock)
        if let Ok(mut buffer) = self.buffer.try_lock() {
            if buffer.len() >= LOG_BUFFER_SIZE {
                buffer.pop_front();
            }
            buffer.push_back(entry.clone());
        }

        // Send to live subscribers (ignore errors if no subscribers)
        let _ = self.sender.send(entry);
    }

    /// Get a receiver for subscribing to log entries
    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.sender.subscribe()
    }

    /// Get buffered messages for replay to late subscribers
    pub async fn get_buffered_messages(&self) -> Vec<LogEntry> {
        self.buffer.lock().await.iter().cloned().collect()
    }
}

/// gRPC service implementation for log streaming
pub struct LogStreamService {
    broadcaster: Arc<LogBroadcaster>,
    run_id: String,
    instance_type: String,
    status: Arc<RwLock<AgentStatus>>,
    shutdown_token: CancellationToken,
    /// Counter for total dropped messages across all clients (for monitoring)
    dropped_messages: Arc<AtomicU64>,
    /// Set when coordinator acknowledges completion
    acknowledged: Arc<std::sync::atomic::AtomicBool>,
}

impl LogStreamService {
    pub fn new(
        broadcaster: Arc<LogBroadcaster>,
        run_id: String,
        instance_type: String,
        status: Arc<RwLock<AgentStatus>>,
        shutdown_token: CancellationToken,
    ) -> Self {
        Self {
            broadcaster,
            run_id,
            instance_type,
            status,
            shutdown_token,
            dropped_messages: Arc::new(AtomicU64::new(0)),
            acknowledged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Wait for the coordinator to acknowledge completion, with a fallback timeout.
    pub async fn wait_for_ack(&self, timeout: std::time::Duration) -> Result<()> {
        let ack = self.acknowledged.clone();
        tokio::time::timeout(timeout, async move {
            while !ack.load(Ordering::Relaxed) {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Timed out waiting for coordinator acknowledgment"))
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
        if req.instance_type != self.instance_type {
            return Err(Status::invalid_argument(format!(
                "Instance type mismatch: requested {} but this is {}",
                req.instance_type, self.instance_type
            )));
        }

        if req.run_id != self.run_id {
            return Err(Status::invalid_argument(format!(
                "Run ID mismatch: requested {} but this is {}",
                req.run_id, self.run_id
            )));
        }

        // Get buffered messages BEFORE subscribing to avoid duplicates
        let buffered = self.broadcaster.get_buffered_messages().await;
        let buffered_count = buffered.len();

        info!(
            instance_type = %req.instance_type,
            run_id = %req.run_id,
            buffered_messages = buffered_count,
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

        // Wrap stream to first replay buffered messages, then stream live, with shutdown support
        let shutdown_stream = async_stream::stream! {
            // First, replay buffered messages from before this client connected
            for entry in buffered {
                yield Ok(entry);
            }

            // Then stream live messages
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

        // Convert StatusCode enum to proto enum
        let proto_status_code = match status.status {
            StatusCode::Pending => ProtoStatusCode::Pending,
            StatusCode::Running => ProtoStatusCode::Running,
            StatusCode::Complete => ProtoStatusCode::Complete,
            StatusCode::Failed => ProtoStatusCode::Failed,
            StatusCode::Bootstrap => ProtoStatusCode::Bootstrap,
            StatusCode::Warmup => ProtoStatusCode::Warmup,
        };

        // Convert run_results to proto format
        let run_results: Vec<ProtoRunResult> = status
            .run_results
            .iter()
            .map(|r| ProtoRunResult {
                run_number: r.run_number,
                duration_secs: r.duration_secs,
                success: r.success,
            })
            .collect();

        Ok(Response::new(StatusResponse {
            status_code: proto_status_code.into(),
            run_progress: status.run_progress,
            total_runs: status.total_runs,
            dropped_log_count: self.dropped_messages.load(Ordering::Relaxed),
            run_results,
            attr: status.attr.clone(),
            system: status.system.to_string(),
            protocol_version: 1,
            error_message: status.error_message.clone(),
        }))
    }

    async fn acknowledge_complete(
        &self,
        request: Request<AckCompleteRequest>,
    ) -> Result<Response<AckCompleteResponse>, Status> {
        let req = request.into_inner();
        info!(run_id = %req.run_id, "Received completion acknowledgment from coordinator");
        self.acknowledged.store(true, Ordering::Relaxed);
        Ok(Response::new(AckCompleteResponse { acknowledged: true }))
    }

    async fn cancel_benchmark(
        &self,
        request: Request<CancelBenchmarkRequest>,
    ) -> Result<Response<CancelBenchmarkResponse>, Status> {
        let req = request.into_inner();
        let reason = if req.reason.is_empty() {
            "coordinator requested cancellation".to_string()
        } else {
            req.reason
        };

        warn!(run_id = %req.run_id, reason = %reason, "Cancellation requested by coordinator");

        // Signal the benchmark to stop via the cancellation token
        self.shutdown_token.cancel();

        // Update status to Failed with the cancellation reason
        {
            let mut status = self.status.write().await;
            if status.status != StatusCode::Complete && status.status != StatusCode::Failed {
                status.status = StatusCode::Failed;
                status.error_message = format!("Cancelled: {}", reason);
            }
        }

        Ok(Response::new(CancelBenchmarkResponse {
            cancelled: true,
            message: "Cancellation requested".to_string(),
        }))
    }
}

/// Run the gRPC server on the specified port with mTLS and graceful shutdown
///
/// # Arguments
/// * `port` - Port to listen on
/// * `broadcaster` - Log broadcaster for streaming
/// * `run_id` - Unique run identifier for request validation
/// * `instance_type` - EC2 instance type for request validation
/// * `status` - Shared agent status
/// * `tls_config` - TLS configuration for mTLS (required)
/// * `shutdown_token` - Token for graceful shutdown signaling
///
/// # Returns
/// * `Ok(())` when server shuts down gracefully
/// * `Err` on server error
pub type AckFlag = Arc<std::sync::atomic::AtomicBool>;

/// Create a new acknowledgment flag.
pub fn new_ack_flag() -> AckFlag {
    Arc::new(std::sync::atomic::AtomicBool::new(false))
}

/// Wait for the acknowledgment flag to be set, with a fallback timeout.
pub async fn wait_for_ack(ack: &AckFlag, timeout: std::time::Duration) -> Result<()> {
    tokio::time::timeout(timeout, async {
        while !ack.load(Ordering::Relaxed) {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Timed out waiting for coordinator acknowledgment"))
}

#[allow(clippy::too_many_arguments)]
pub async fn run_grpc_server(
    port: u16,
    broadcaster: Arc<LogBroadcaster>,
    run_id: String,
    instance_type: String,
    status: Arc<RwLock<AgentStatus>>,
    tls_config: TlsConfig,
    shutdown_token: CancellationToken,
    ack_flag: AckFlag,
) -> Result<()> {
    let addr = format!("0.0.0.0:{}", port).parse()?;

    let mut service = LogStreamService::new(
        broadcaster,
        run_id,
        instance_type,
        status,
        shutdown_token.clone(),
    );
    service.acknowledged = ack_flag;

    let tls = tls_config.server_tls_config()?;
    info!(%addr, "Starting gRPC server with mTLS");

    tonic::transport::Server::builder()
        .tls_config(tls)?
        .add_service(LogStreamServer::new(service))
        .serve_with_shutdown(addr, shutdown_token.cancelled_owned())
        .await?;

    info!("gRPC server shut down gracefully");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

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
    async fn test_get_status_returns_current_status() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let status = Arc::new(RwLock::new(AgentStatus {
            status: StatusCode::Running,
            run_progress: 3,
            total_runs: 10,
            run_results: Vec::new(),
            attr: "shallow.hello".to_string(),
            system: Architecture::X86_64,
            error_message: String::new(),
        }));

        let shutdown_token = CancellationToken::new();
        let service = LogStreamService::new(
            broadcaster,
            "test-run-123".to_string(),
            "c6i.xlarge".to_string(),
            status,
            shutdown_token,
        );

        let request = Request::new(StatusRequest {});
        let response = service.get_status(request).await.unwrap();
        let inner = response.into_inner();

        assert_eq!(inner.status_code, ProtoStatusCode::Running as i32);
        assert_eq!(inner.run_progress, 3);
        assert_eq!(inner.total_runs, 10);
        assert_eq!(inner.dropped_log_count, 0); // No dropped messages yet
    }

    #[tokio::test]
    async fn test_stream_logs_validates_instance_type() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(
            broadcaster,
            "test-run-123".to_string(),
            "c6i.xlarge".to_string(),
            status,
            shutdown_token,
        );

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
        let status = Arc::new(RwLock::new(AgentStatus::default()));
        let shutdown_token = CancellationToken::new();

        let service = LogStreamService::new(
            broadcaster,
            "test-run-123".to_string(),
            "c6i.xlarge".to_string(),
            status,
            shutdown_token,
        );

        let request = Request::new(StreamLogsRequest {
            instance_type: "c6i.xlarge".to_string(),
            run_id: "test-run-123".to_string(),
        });

        let result = service.stream_logs(request).await;
        assert!(result.is_ok(), "Valid request should be accepted");
    }
}
