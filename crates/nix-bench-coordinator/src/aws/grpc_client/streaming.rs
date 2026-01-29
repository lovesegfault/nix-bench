//! Log streaming configuration and launcher

use super::log_client::GrpcLogClient;
use crate::tui::TuiMessage;
use anyhow::Result;
use nix_bench_common::TlsConfig;
use tokio::sync::mpsc;

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
    /// TLS configuration (required)
    pub tls_config: TlsConfig,
    /// Output destination
    pub output: LogOutput,
}

impl LogStreamingOptions {
    /// Create new options for the given instances with TLS
    pub fn new(
        instances: &[(String, String)],
        run_id: &str,
        port: u16,
        tls_config: TlsConfig,
    ) -> Self {
        Self {
            instances: instances.to_vec(),
            run_id: run_id.to_string(),
            port,
            tls_config,
            output: LogOutput::Stdout,
        }
    }

    /// Set output to TUI channel
    pub fn with_channel(mut self, tx: mpsc::Sender<TuiMessage>) -> Self {
        self.output = LogOutput::Channel(tx);
        self
    }
}

/// Start log streaming for multiple instances (unified function)
///
/// Spawns a background task for each instance that streams logs via gRPC with mTLS.
/// Returns handles to all spawned tasks.
pub fn start_log_streaming_unified(
    options: LogStreamingOptions,
) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    let mut handles = Vec::new();

    for (instance_type, public_ip) in options.instances {
        let client = GrpcLogClient::new(
            &instance_type,
            &public_ip,
            options.port,
            &options.run_id,
            options.tls_config.clone(),
        );

        let handle = match &options.output {
            LogOutput::Channel(tx) => {
                let tx = tx.clone();
                tokio::spawn(async move { client.stream_to_channel(tx).await })
            }
            LogOutput::Stdout => tokio::spawn(async move { client.stream_to_stdout().await }),
        };

        handles.push(handle);
    }

    handles
}
