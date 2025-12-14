//! Integration tests for gRPC client with actual agent server
//!
//! These tests spin up a real gRPC server using the agent crate and test
//! the coordinator's client against it.

use nix_bench_agent::config::Config;
use nix_bench_agent::grpc::{AgentStatus, LogBroadcaster, LogStreamService};
use nix_bench_coordinator::aws::GrpcLogClient;
use nix_bench_coordinator::tui::TuiMessage;
use nix_bench_proto::LogStreamServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// Create a test configuration for the agent
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

/// Find an available TCP port for testing
async fn find_available_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Wait for TCP server to be ready
async fn wait_for_tcp_ready(addr: &str, timeout: Duration) -> Result<(), String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err(format!("Timeout waiting for TCP server at {}", addr))
}

/// Start a gRPC server for testing and return the port
async fn start_test_server(
    broadcaster: Arc<LogBroadcaster>,
    config: Arc<Config>,
    status: Arc<RwLock<AgentStatus>>,
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
    let status = Arc::new(RwLock::new(AgentStatus::default()));

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
    let status = Arc::new(RwLock::new(AgentStatus::default()));

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
    let status = Arc::new(RwLock::new(AgentStatus::default()));

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
    let status = Arc::new(RwLock::new(AgentStatus::default()));
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
