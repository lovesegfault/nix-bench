//! gRPC client for streaming logs from benchmark agents

mod channel;
mod log_client;
mod status_poller;
mod streaming;

pub use channel::{ChannelOptions, GrpcChannelBuilder};
pub use log_client::GrpcLogClient;
pub use status_poller::{GrpcInstanceStatus, GrpcStatusPoller};
pub use streaming::{start_log_streaming_unified, LogOutput, LogStreamingOptions};

use anyhow::Result;
use std::time::Duration;

/// Fast TCP connect check - use in tests for faster execution
///
/// This performs a simple TCP connection attempt to verify the server is listening.
/// It's much faster than a full gRPC health check since it doesn't require
/// TLS handshake or RPC setup.
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

#[cfg(test)]
mod tests {
    use super::*;
    use nix_bench_common::TlsConfig;
    use tokio::sync::mpsc;

    fn test_tls_config() -> TlsConfig {
        TlsConfig {
            ca_cert_pem: "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----".to_string(),
            cert_pem: "-----BEGIN CERTIFICATE-----\nCERT\n-----END CERTIFICATE-----".to_string(),
            key_pem: "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----".to_string(),
        }
    }

    #[test]
    fn test_grpc_log_client_creation() {
        let tls = test_tls_config();
        let client = GrpcLogClient::new("c6i.xlarge", "192.168.1.100", 50051, "run-123", tls);

        assert_eq!(client.instance_type, "c6i.xlarge");
        assert_eq!(client.public_ip, "192.168.1.100");
        assert_eq!(client.port, 50051);
        assert_eq!(client.run_id, "run-123");
    }

    #[test]
    fn test_grpc_log_client_endpoint() {
        let tls = test_tls_config();
        let client = GrpcLogClient::new("c6i.xlarge", "10.0.0.1", 50051, "run-456", tls);
        assert_eq!(
            client.channel_builder().endpoint(),
            "https://10.0.0.1:50051"
        );
    }

    #[test]
    fn test_grpc_log_client_endpoint_with_different_port() {
        let tls = test_tls_config();
        let client = GrpcLogClient::new("c6g.xlarge", "192.168.1.50", 9000, "run-789", tls);
        assert_eq!(
            client.channel_builder().endpoint(),
            "https://192.168.1.50:9000"
        );
    }

    #[tokio::test]
    async fn test_connect_with_retry_fails_after_max_retries() {
        let tls = test_tls_config();
        let client = GrpcLogClient::new("c6i.xlarge", "127.0.0.1", 59999, "test-run", tls);

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
        let instances = vec![
            ("c6i.xlarge".to_string(), "10.0.0.1".to_string()),
            ("c6g.xlarge".to_string(), "10.0.0.2".to_string()),
            ("c7i.xlarge".to_string(), "10.0.0.3".to_string()),
        ];

        let (tx, _rx) = mpsc::channel(100);

        let tls = test_tls_config();
        let options = LogStreamingOptions::new(&instances, "test-run", 50051, tls).with_channel(tx);
        let handles = start_log_streaming_unified(options);

        assert_eq!(handles.len(), 3);

        for handle in handles {
            handle.abort();
        }
    }
}
