//! Integration tests for gRPC client with actual agent server
//!
//! These tests spin up a real gRPC server using the agent crate and test
//! the coordinator's client against it with mTLS.

use std::sync::Once;

use nix_bench_agent::grpc::{AgentStatus, LogBroadcaster, LogStreamService};

/// Install the rustls crypto provider (once per process)
static INIT: Once = Once::new();

fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}
use nix_bench_common::tls::{generate_agent_cert, generate_ca, generate_coordinator_cert, TlsConfig};
use nix_bench_coordinator::aws::{GrpcLogClient, GrpcStatusPoller};
use nix_bench_coordinator::tui::TuiMessage;
use nix_bench_proto::LogStreamServer;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

/// Test run ID and instance type for integration tests
const TEST_RUN_ID: &str = "test-run-123";
const TEST_INSTANCE_TYPE: &str = "c6i.xlarge";

/// Test TLS configuration for agent and coordinator
struct TestTlsCerts {
    agent_tls: TlsConfig,
    coordinator_tls: TlsConfig,
}

/// Generate test TLS certificates for integration tests
fn generate_test_certs() -> TestTlsCerts {
    init_crypto();
    let ca = generate_ca("test-integration").expect("Failed to generate CA");

    let agent_cert = generate_agent_cert(&ca.cert_pem, &ca.key_pem, TEST_INSTANCE_TYPE, Some("127.0.0.1"))
        .expect("Failed to generate agent cert");

    let coord_cert = generate_coordinator_cert(&ca.cert_pem, &ca.key_pem)
        .expect("Failed to generate coordinator cert");

    TestTlsCerts {
        agent_tls: TlsConfig {
            ca_cert_pem: ca.cert_pem.clone(),
            cert_pem: agent_cert.cert_pem,
            key_pem: agent_cert.key_pem,
        },
        coordinator_tls: TlsConfig {
            ca_cert_pem: ca.cert_pem,
            cert_pem: coord_cert.cert_pem,
            key_pem: coord_cert.key_pem,
        },
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

/// Start a gRPC server with TLS for testing and return the port
async fn start_test_server_with_tls(
    broadcaster: Arc<LogBroadcaster>,
    status: Arc<RwLock<AgentStatus>>,
    tls_config: TlsConfig,
) -> u16 {
    let port = find_available_port().await;
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let shutdown_token = CancellationToken::new();

    let service = LogStreamService::new(
        broadcaster,
        TEST_RUN_ID.to_string(),
        TEST_INSTANCE_TYPE.to_string(),
        status,
        shutdown_token,
    );

    let tls = tls_config.server_tls_config().expect("Failed to create server TLS config");

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .tls_config(tls)
            .expect("Failed to configure TLS")
            .add_service(LogStreamServer::new(service))
            .serve(addr)
            .await
            .unwrap();
    });

    // Wait for server to be ready
    wait_for_tcp_ready(&addr.to_string(), Duration::from_secs(5))
        .await
        .expect("Test server should be ready");

    port
}

#[tokio::test]
async fn test_connect_with_retry_succeeds_when_server_available() {
    let certs = generate_test_certs();
    let broadcaster = Arc::new(LogBroadcaster::new(100));
    let status = Arc::new(RwLock::new(AgentStatus::default()));

    let port = start_test_server_with_tls(broadcaster, status, certs.agent_tls).await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        port,
        TEST_RUN_ID,
        certs.coordinator_tls,
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
    let certs = generate_test_certs();
    let broadcaster = Arc::new(LogBroadcaster::new(100));
    let status = Arc::new(RwLock::new(AgentStatus::default()));

    let port = start_test_server_with_tls(Arc::clone(&broadcaster), status, certs.agent_tls).await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        port,
        TEST_RUN_ID,
        certs.coordinator_tls,
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
    broadcaster.broadcast(1000, "[stdout] Building package...".to_string());
    broadcaster.broadcast(2000, "[stdout] Build complete".to_string());

    // Wait for messages
    let msg1 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("Should receive first message")
        .expect("Channel should not be closed");

    if let TuiMessage::ConsoleOutputAppend { instance_type, line } = msg1 {
        assert_eq!(instance_type, TEST_INSTANCE_TYPE);
        assert!(line.contains("Building package"));
    } else {
        panic!("Expected ConsoleOutputAppend message, got {:?}", msg1);
    }

    let msg2 = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("Should receive second message")
        .expect("Channel should not be closed");

    if let TuiMessage::ConsoleOutputAppend { instance_type, line } = msg2 {
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
    let certs = generate_test_certs();
    let broadcaster = Arc::new(LogBroadcaster::new(100));
    let status = Arc::new(RwLock::new(AgentStatus::default()));

    let port = start_test_server_with_tls(Arc::clone(&broadcaster), status, certs.agent_tls).await;

    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        port,
        TEST_RUN_ID,
        certs.coordinator_tls,
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

    let tls = certs.agent_tls.server_tls_config().expect("Failed to create server TLS config");

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

/// Integration test that simulates the bootstrap flow:
/// 1. Agent starts gRPC server immediately
/// 2. Coordinator connects and starts streaming
/// 3. Agent runs bootstrap commands (simulated with echo)
/// 4. Coordinator receives all bootstrap output via gRPC
#[tokio::test]
async fn test_bootstrap_streaming_integration() {
    use nix_bench_agent::logging::GrpcLogger;

    let certs = generate_test_certs();
    let broadcaster = Arc::new(LogBroadcaster::new(1024));
    let status = Arc::new(RwLock::new(AgentStatus {
        status: "bootstrap".to_string(),
        run_progress: 0,
        total_runs: 0,
        durations: Vec::new(),
        run_results: Vec::new(),
        attr: String::new(),
        system: String::new(),
    }));

    let port = start_test_server_with_tls(Arc::clone(&broadcaster), Arc::clone(&status), certs.agent_tls).await;

    // Connect client (simulating coordinator)
    let client = GrpcLogClient::new(
        TEST_INSTANCE_TYPE,
        "127.0.0.1",
        port,
        TEST_RUN_ID,
        certs.coordinator_tls,
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
    let logger = GrpcLogger::new(Arc::clone(&broadcaster));

    // Simulate bootstrap phase - log messages
    logger.write_line("=== Starting Bootstrap ===");
    logger.write_line("=== NVMe Instance Store Setup ===");
    logger.write_line("No NVMe instance store devices found, using default storage");

    // Simulate running a command (like the Nix installer would)
    let result = logger
        .run_command("echo", &["Installing Nix..."], Some(10))
        .await;
    assert!(result.is_ok());
    assert!(result.unwrap(), "Echo command should succeed");

    logger.write_line("=== Bootstrap Complete ===");

    // Update status to show bootstrap is done
    {
        let mut s = status.write().await;
        s.status = "running".to_string();
    }

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
        received_lines.iter().any(|l| l.contains("Starting Bootstrap")),
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
        received_lines.iter().any(|l| l.contains("Bootstrap Complete")),
        "Should receive bootstrap complete message. Got: {:?}",
        received_lines
    );

    // Clean up
    stream_handle.abort();
}

/// Test that verifies status transitions during bootstrap are visible via gRPC
#[tokio::test]
async fn test_bootstrap_status_transitions() {
    use nix_bench_common::StatusCode;

    let certs = generate_test_certs();
    let broadcaster = Arc::new(LogBroadcaster::new(100));
    let status = Arc::new(RwLock::new(AgentStatus {
        status: "bootstrap".to_string(),
        run_progress: 0,
        total_runs: 0,
        durations: Vec::new(),
        run_results: Vec::new(),
        attr: String::new(),
        system: String::new(),
    }));

    let port = start_test_server_with_tls(Arc::clone(&broadcaster), Arc::clone(&status), certs.agent_tls).await;

    // Create status poller with TLS
    let instances = vec![(TEST_INSTANCE_TYPE.to_string(), "127.0.0.1".to_string())];
    let poller = GrpcStatusPoller::new(&instances, port, certs.coordinator_tls);

    // Check initial bootstrap status
    let statuses = poller.poll_status().await;
    let initial_status = statuses.get(TEST_INSTANCE_TYPE).expect("Should have status");
    assert!(initial_status.run_progress == Some(0));
    assert!(initial_status.total_runs == Some(0));

    // Simulate status transition after bootstrap completes
    {
        let mut s = status.write().await;
        s.status = "warmup".to_string();
        s.total_runs = 5;
    }

    let statuses = poller.poll_status().await;
    let warmup_status = statuses.get(TEST_INSTANCE_TYPE).expect("Should have status");
    assert_eq!(warmup_status.total_runs, Some(5));

    // Simulate running status
    {
        let mut s = status.write().await;
        s.status = "running".to_string();
        s.run_progress = 1;
    }

    let statuses = poller.poll_status().await;
    let running_status = statuses.get(TEST_INSTANCE_TYPE).expect("Should have status");
    assert_eq!(running_status.status, Some(StatusCode::Running.as_i32()));
    assert_eq!(running_status.run_progress, Some(1));

    // Simulate completion
    {
        let mut s = status.write().await;
        s.status = "complete".to_string();
        s.run_progress = 5;
        s.durations = vec![10.5, 11.2, 10.8, 11.0, 10.9];
    }

    let statuses = poller.poll_status().await;
    let complete_status = statuses.get(TEST_INSTANCE_TYPE).expect("Should have status");
    assert_eq!(complete_status.status, Some(StatusCode::Complete.as_i32()));
    assert_eq!(complete_status.run_progress, Some(5));
    assert_eq!(complete_status.durations.len(), 5);
}
