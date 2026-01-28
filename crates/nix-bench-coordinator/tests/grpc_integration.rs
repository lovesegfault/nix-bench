//! Integration tests for gRPC client with actual agent server
//!
//! These tests spin up a real gRPC server using the agent crate and test
//! the coordinator's client against it with mTLS.

mod test_utils;

use nix_bench_agent::grpc::{AgentStatus, LogBroadcaster, LogStreamService, StatusCode};
use nix_bench_coordinator::aws::{GrpcLogClient, GrpcStatusPoller};
use nix_bench_coordinator::tui::TuiMessage;
use nix_bench_proto::LogStreamServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use test_utils::{
    find_available_port, generate_test_certs, GrpcTestFixture, TEST_INSTANCE_TYPE, TEST_RUN_ID,
};
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn test_connect_with_retry_succeeds_when_server_available() {
    let fixture = GrpcTestFixture::new().await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        fixture.port,
        TEST_RUN_ID,
        fixture.coordinator_tls,
    );

    let result = client
        .connect_with_retry(3, Duration::from_millis(100))
        .await;

    assert!(
        result.is_ok(),
        "Should connect successfully to running server: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn test_stream_to_channel_receives_logs() {
    let fixture = GrpcTestFixture::new().await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        fixture.port,
        TEST_RUN_ID,
        fixture.coordinator_tls,
    );

    let (tx, mut rx) = mpsc::channel(100);

    // Spawn streaming task (use inner method to skip wait_for_ready)
    let stream_handle = tokio::spawn({
        let client = client.clone();
        async move { client.stream_to_channel_inner(tx).await }
    });

    // Give time for connection to establish
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Broadcast some messages
    fixture
        .broadcaster
        .broadcast(1000, "[stdout] Building package...".to_string());
    fixture
        .broadcaster
        .broadcast(2000, "[stdout] Build complete".to_string());

    // Wait for messages
    let msg1 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("Should receive first message")
        .expect("Channel should not be closed");

    if let TuiMessage::ConsoleOutputAppend {
        instance_type,
        line,
    } = msg1
    {
        assert_eq!(instance_type, TEST_INSTANCE_TYPE);
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
        assert_eq!(instance_type, TEST_INSTANCE_TYPE);
        assert!(line.contains("Build complete"));
    } else {
        panic!("Expected ConsoleOutputAppend message, got {:?}", msg2);
    }

    // Clean up
    stream_handle.abort();
}

#[tokio::test]
async fn test_stream_to_channel_exits_gracefully_on_channel_close() {
    let fixture = GrpcTestFixture::new().await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        fixture.port,
        TEST_RUN_ID,
        fixture.coordinator_tls,
    );

    let (tx, rx) = mpsc::channel(100);

    // Spawn streaming task
    let stream_handle = tokio::spawn({
        let client = client.clone();
        async move { client.stream_to_channel_inner(tx).await }
    });

    // Give time for connection to establish
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Broadcast a message to verify streaming is working
    fixture
        .broadcaster
        .broadcast(1000, "test message".to_string());
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Drop the receiver - this should cause the streaming task to exit gracefully
    drop(rx);

    // Send more messages after receiver is dropped
    fixture
        .broadcaster
        .broadcast(2000, "after drop".to_string());

    // The streaming task should exit gracefully (not hang or panic)
    let result = tokio::time::timeout(Duration::from_secs(2), stream_handle).await;

    assert!(
        result.is_ok(),
        "Streaming task should exit within timeout after channel closure"
    );

    let inner_result = result.unwrap();
    assert!(
        inner_result.is_ok(),
        "Streaming task should not panic: {:?}",
        inner_result
    );

    let stream_result = inner_result.unwrap();
    assert!(
        stream_result.is_ok(),
        "Stream should exit gracefully on channel close"
    );
}

#[tokio::test]
async fn test_connect_with_retry_retries_on_failure_then_succeeds() {
    let certs = generate_test_certs();

    // First, find a port
    let port = find_available_port().await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        port,
        TEST_RUN_ID,
        certs.coordinator_tls,
    );

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
    let status = Arc::new(RwLock::new(AgentStatus::default()));
    let shutdown_token = CancellationToken::new();

    // Start server on the same port with TLS
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let service = LogStreamService::new(
        broadcaster,
        TEST_RUN_ID.to_string(),
        TEST_INSTANCE_TYPE.to_string(),
        status,
        shutdown_token,
    );

    let tls = certs
        .agent_tls
        .server_tls_config()
        .expect("Failed to create server TLS config");

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .tls_config(tls)
            .expect("Failed to configure TLS")
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
        "Should eventually connect after server starts: {:?}",
        result.err()
    );
}

/// Integration test that simulates the bootstrap flow
#[tokio::test]
async fn test_bootstrap_streaming_integration() {
    use nix_bench_agent::logging::GrpcLogger;

    let initial_status = AgentStatus {
        status: StatusCode::Bootstrap,
        run_progress: 0,
        total_runs: 0,
        durations: Vec::new(),
        run_results: Vec::new(),
        attr: String::new(),
        system: String::new(),
    };

    let fixture = GrpcTestFixture::with_status(initial_status).await;

    // Connect client (simulating coordinator)
    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        fixture.port,
        TEST_RUN_ID,
        fixture.coordinator_tls.clone(),
    );
    let (tx, mut rx) = mpsc::channel(100);

    // Start streaming in background
    let stream_handle = tokio::spawn({
        let client = client.clone();
        async move { client.stream_to_channel_inner(tx).await }
    });

    // Wait for connection to establish
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Create logger (simulating agent bootstrap)
    let logger = GrpcLogger::new(fixture.broadcaster.clone());

    // Simulate bootstrap phase - log messages
    logger.write_line("=== Starting Bootstrap ===");
    logger.write_line("=== NVMe Instance Store Setup ===");
    logger.write_line("No NVMe instance store devices found, using default storage");

    // Simulate running a command
    let result = logger
        .run_command("echo", &["Installing Nix..."], Some(10))
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap(), "Echo command should succeed");

    logger.write_line("=== Bootstrap Complete ===");

    // Update status to show bootstrap is done
    fixture
        .set_status(AgentStatus {
            status: StatusCode::Running,
            ..Default::default()
        })
        .await;

    // Collect received messages
    let mut received_lines = Vec::new();
    let timeout = Duration::from_secs(2);
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        match tokio::time::timeout(Duration::from_millis(100), rx.recv()).await {
            Ok(Some(msg)) => {
                if let TuiMessage::ConsoleOutputAppend { line, .. } = msg {
                    received_lines.push(line);
                }
            }
            Ok(None) => break,
            Err(_) => {
                if received_lines.len() >= 5 {
                    break;
                }
            }
        }
    }

    // Verify we received the expected bootstrap messages
    assert!(
        received_lines
            .iter()
            .any(|l| l.contains("Starting Bootstrap")),
        "Should receive bootstrap start message. Got: {:?}",
        received_lines
    );
    assert!(
        received_lines.iter().any(|l| l.contains("NVMe")),
        "Should receive NVMe setup message. Got: {:?}",
        received_lines
    );
    assert!(
        received_lines.iter().any(|l| l.contains("Installing Nix")),
        "Should receive Nix install output. Got: {:?}",
        received_lines
    );
    assert!(
        received_lines
            .iter()
            .any(|l| l.contains("Bootstrap Complete")),
        "Should receive bootstrap complete message. Got: {:?}",
        received_lines
    );

    // Clean up
    stream_handle.abort();
}

/// Test that verifies status transitions during bootstrap are visible via gRPC
#[tokio::test]
async fn test_bootstrap_status_transitions() {
    let initial_status = AgentStatus {
        status: StatusCode::Bootstrap,
        run_progress: 0,
        total_runs: 0,
        durations: Vec::new(),
        run_results: Vec::new(),
        attr: String::new(),
        system: String::new(),
    };

    let fixture = GrpcTestFixture::with_status(initial_status).await;

    // Create status poller with TLS
    let instances = vec![(TEST_INSTANCE_TYPE.to_string(), "127.0.0.1".to_string())];
    let poller = GrpcStatusPoller::new(&instances, fixture.port, fixture.coordinator_tls.clone());

    // Check initial bootstrap status
    let statuses = poller.poll_status().await;
    let initial_status = statuses
        .get(TEST_INSTANCE_TYPE)
        .expect("Should have status");
    assert!(initial_status.run_progress == Some(0));
    assert!(initial_status.total_runs == Some(0));

    // Simulate status transition after bootstrap completes
    fixture
        .set_status(AgentStatus {
            status: StatusCode::Warmup,
            total_runs: 5,
            ..Default::default()
        })
        .await;

    let statuses = poller.poll_status().await;
    let warmup_status = statuses
        .get(TEST_INSTANCE_TYPE)
        .expect("Should have status");
    assert_eq!(warmup_status.total_runs, Some(5));

    // Simulate running status
    fixture
        .set_status(AgentStatus {
            status: StatusCode::Running,
            run_progress: 1,
            total_runs: 5,
            ..Default::default()
        })
        .await;

    let statuses = poller.poll_status().await;
    let running_status = statuses
        .get(TEST_INSTANCE_TYPE)
        .expect("Should have status");
    assert_eq!(running_status.status, Some(StatusCode::Running));
    assert_eq!(running_status.run_progress, Some(1));

    // Simulate completion
    fixture
        .set_status(AgentStatus {
            status: StatusCode::Complete,
            run_progress: 5,
            total_runs: 5,
            durations: vec![10.5, 11.2, 10.8, 11.0, 10.9],
            ..Default::default()
        })
        .await;

    let statuses = poller.poll_status().await;
    let complete_status = statuses
        .get(TEST_INSTANCE_TYPE)
        .expect("Should have status");
    assert_eq!(complete_status.status, Some(StatusCode::Complete));
    assert_eq!(complete_status.run_progress, Some(5));
    assert_eq!(complete_status.durations.len(), 5);
}
