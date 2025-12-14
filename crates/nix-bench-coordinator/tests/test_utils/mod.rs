//! Shared test utilities for integration tests

use nix_bench_agent::grpc::{AgentStatus, LogBroadcaster, LogStreamService};
use nix_bench_common::tls::{generate_agent_cert, generate_ca, generate_coordinator_cert, TlsConfig};
use nix_bench_coordinator::aws::wait_for_tcp_ready;
use nix_bench_proto::LogStreamServer;
use std::net::SocketAddr;
use std::sync::{Arc, Once};
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

/// Test run ID for integration tests
pub const TEST_RUN_ID: &str = "test-run-123";

/// Test instance type for integration tests
pub const TEST_INSTANCE_TYPE: &str = "c6i.xlarge";

/// Install the rustls crypto provider (once per process)
static INIT: Once = Once::new();

/// Initialize crypto provider for TLS tests
pub fn init_crypto() {
    INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install rustls crypto provider");
    });
}

/// Test TLS configuration for agent and coordinator
pub struct TestTlsCerts {
    pub agent_tls: TlsConfig,
    pub coordinator_tls: TlsConfig,
}

/// Generate test TLS certificates for integration tests
pub fn generate_test_certs() -> TestTlsCerts {
    init_crypto();
    let ca = generate_ca("test-integration").expect("Failed to generate CA");

    let agent_cert =
        generate_agent_cert(&ca.cert_pem, &ca.key_pem, TEST_INSTANCE_TYPE, Some("127.0.0.1"))
            .expect("Failed to generate agent cert");

    let coord_cert =
        generate_coordinator_cert(&ca.cert_pem, &ca.key_pem).expect("Failed to generate coordinator cert");

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
pub async fn find_available_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// Test fixture for gRPC integration tests
///
/// Provides a pre-configured test server with TLS, broadcaster, and status.
pub struct GrpcTestFixture {
    /// Port the test server is listening on
    pub port: u16,
    /// Log broadcaster for the test server
    pub broadcaster: Arc<LogBroadcaster>,
    /// Agent status for the test server
    pub status: Arc<RwLock<AgentStatus>>,
    /// TLS config for the coordinator to connect
    pub coordinator_tls: TlsConfig,
}

impl GrpcTestFixture {
    /// Create a new test fixture with default status
    pub async fn new() -> Self {
        Self::with_status(AgentStatus::default()).await
    }

    /// Create a new test fixture with custom initial status
    pub async fn with_status(initial_status: AgentStatus) -> Self {
        let certs = generate_test_certs();
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let status = Arc::new(RwLock::new(initial_status));

        let port = start_test_server(
            broadcaster.clone(),
            status.clone(),
            certs.agent_tls,
        )
        .await;

        Self {
            port,
            broadcaster,
            status,
            coordinator_tls: certs.coordinator_tls,
        }
    }

    /// Update the agent status
    pub async fn set_status(&self, new_status: AgentStatus) {
        *self.status.write().await = new_status;
    }
}

/// Start a gRPC server with TLS for testing
async fn start_test_server(
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

    let tls = tls_config
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

    // Wait for server to be ready
    wait_for_tcp_ready(&addr.to_string(), Duration::from_secs(5))
        .await
        .expect("Test server should be ready");

    port
}
