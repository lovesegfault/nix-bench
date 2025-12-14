//! Shared command execution with streaming output
//!
//! Provides a unified command runner that streams output via gRPC broadcaster.

use crate::grpc::LogBroadcaster;
use anyhow::{Context, Result};
use nix_bench_common::timestamp_millis;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{info, warn};

/// Configuration for command execution
#[derive(Debug, Clone)]
pub struct CommandConfig {
    /// Command timeout (kills process if exceeded)
    pub timeout: Duration,
    /// Time to wait for streaming tasks to flush after command completes
    pub stream_flush_timeout: Duration,
}

impl CommandConfig {
    /// Configuration for long-running benchmark commands (2 hour timeout)
    pub fn for_benchmark() -> Self {
        Self {
            timeout: Duration::from_secs(7200),
            stream_flush_timeout: Duration::from_secs(5),
        }
    }

    /// Configuration for bootstrap/setup commands (10 minute timeout)
    pub fn for_bootstrap() -> Self {
        Self {
            timeout: Duration::from_secs(600),
            stream_flush_timeout: Duration::from_secs(2),
        }
    }

    /// Create with custom timeout, default stream flush timeout
    pub fn with_timeout_secs(timeout_secs: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_secs),
            stream_flush_timeout: Duration::from_secs(5),
        }
    }
}

/// Run a command and stream its output to gRPC clients
///
/// # Arguments
/// * `broadcaster` - The log broadcaster to stream output to
/// * `cmd` - The command to run
/// * `args` - Command arguments
/// * `config` - Execution configuration (timeout, stream flush)
///
/// # Returns
/// * `Ok(true)` if command succeeded
/// * `Ok(false)` if command failed with non-zero exit
/// * `Err` if timeout, spawn failure, or other error
pub async fn run_command_streaming(
    broadcaster: &Arc<LogBroadcaster>,
    cmd: &str,
    args: &[&str],
    config: &CommandConfig,
) -> Result<bool> {
    info!(
        cmd = %cmd,
        args = ?args,
        timeout_secs = config.timeout.as_secs(),
        "Running command"
    );

    let mut child = Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn command: {}", cmd))?;

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let broadcaster_stdout = broadcaster.clone();
    let broadcaster_stderr = broadcaster.clone();

    // Stream stdout
    let stdout_handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            broadcaster_stdout.broadcast(timestamp_millis(), line);
        }
    });

    // Stream stderr
    let stderr_handle = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            broadcaster_stderr.broadcast(timestamp_millis(), line);
        }
    });

    // Wait for command with timeout
    let wait_result = tokio::time::timeout(config.timeout, child.wait()).await;

    let success = match wait_result {
        Ok(Ok(status)) => status.success(),
        Ok(Err(e)) => return Err(e).context("Failed waiting for command"),
        Err(_) => {
            warn!(
                cmd = %cmd,
                timeout_secs = config.timeout.as_secs(),
                "Command timed out, killing process"
            );
            if let Err(e) = child.kill().await {
                warn!(error = %e, "Failed to kill timed-out process");
            }
            // Give streaming tasks a moment to flush remaining output
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Err(anyhow::anyhow!(
                "Command '{}' timed out after {}s",
                cmd,
                config.timeout.as_secs()
            ));
        }
    };

    // Wait for streaming to finish with timeout
    let _ = tokio::time::timeout(config.stream_flush_timeout, stdout_handle).await;
    let _ = tokio::time::timeout(config.stream_flush_timeout, stderr_handle).await;

    Ok(success)
}
