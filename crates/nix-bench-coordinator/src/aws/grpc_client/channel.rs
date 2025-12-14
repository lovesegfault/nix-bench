//! gRPC channel building with mTLS

use anyhow::{Context, Result};
use nix_bench_common::TlsConfig;
use std::time::Duration;
use tonic::transport::Channel;

// Re-export for backward compatibility
pub use nix_bench_common::jittered_delay_25 as jittered_delay;

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

/// Unified gRPC channel builder with required mTLS
pub struct GrpcChannelBuilder<'a> {
    endpoint: String,
    tls_config: &'a TlsConfig,
    options: ChannelOptions,
}

impl<'a> GrpcChannelBuilder<'a> {
    /// Create a new channel builder for the given host and port with TLS
    pub fn new(host: &str, port: u16, tls_config: &'a TlsConfig) -> Self {
        Self {
            endpoint: format!("https://{}:{}", host, port),
            tls_config,
            options: ChannelOptions::default(),
        }
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
        let tls_config = self
            .tls_config
            .client_tls_config()
            .context("Failed to create TLS config")?;

        Channel::from_shared(self.endpoint.clone())
            .context("Invalid endpoint")?
            .connect_timeout(self.options.connect_timeout)
            .timeout(self.options.request_timeout)
            .tls_config(tls_config)
            .context("Failed to configure TLS")?
            .connect()
            .await
            .context("gRPC connection failed")
    }

    /// Try to build and connect, returning None on failure (for polling)
    pub async fn try_connect(self) -> Option<Channel> {
        self.connect().await.ok()
    }
}
