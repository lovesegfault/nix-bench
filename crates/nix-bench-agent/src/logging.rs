//! gRPC-only logging for real-time output streaming
//!
//! This module provides logging that streams directly to gRPC clients,
//! with no CloudWatch dependencies.

use crate::grpc::LogBroadcaster;
use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{info, warn};

/// gRPC-only logger for streaming output to connected clients
pub struct GrpcLogger {
    broadcaster: Arc<LogBroadcaster>,
}

impl GrpcLogger {
    /// Create a new gRPC logger
    pub fn new(broadcaster: Arc<LogBroadcaster>) -> Self {
        Self { broadcaster }
    }

    /// Get the current timestamp in milliseconds
    fn timestamp() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    /// Write a log line to gRPC clients
    pub fn write_line(&self, message: &str) {
        self.broadcaster.broadcast(Self::timestamp(), message.to_string());
    }

    /// Run a command and stream its output to gRPC clients
    ///
    /// # Arguments
    /// * `cmd` - The command to run
    /// * `args` - Command arguments
    /// * `timeout_secs` - Optional timeout in seconds (default: 2 hours)
    ///
    /// # Returns
    /// * `Ok(true)` if command succeeded
    /// * `Ok(false)` if command failed with non-zero exit
    /// * `Err` if timeout, spawn failure, or other error
    pub async fn run_command(
        &self,
        cmd: &str,
        args: &[&str],
        timeout_secs: Option<u64>,
    ) -> Result<bool> {
        // Default timeout: 2 hours
        let timeout = Duration::from_secs(timeout_secs.unwrap_or(7200));
        info!(
            cmd = %cmd,
            timeout_secs = timeout.as_secs(),
            "Starting command with timeout"
        );

        let mut child = Command::new(cmd)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn command")?;

        let stdout = child
            .stdout
            .take()
            .context("Failed to capture stdout - was Stdio::piped() used?")?;
        let stderr = child
            .stderr
            .take()
            .context("Failed to capture stderr - was Stdio::piped() used?")?;

        let broadcaster_stdout = self.broadcaster.clone();
        let broadcaster_stderr = self.broadcaster.clone();

        // Stream stdout
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                broadcaster_stdout.broadcast(timestamp, line);
            }
        });

        // Stream stderr
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                broadcaster_stderr.broadcast(timestamp, line);
            }
        });

        // Wait for command to complete with timeout
        let wait_result = tokio::time::timeout(timeout, child.wait()).await;

        // Handle timeout or completion
        let success = match wait_result {
            Ok(Ok(status)) => {
                // Command completed normally
                status.success()
            }
            Ok(Err(e)) => {
                // Error waiting for child
                return Err(e).context("Failed waiting for command");
            }
            Err(_) => {
                // Timeout - kill the child process
                warn!(
                    cmd = %cmd,
                    timeout_secs = timeout.as_secs(),
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
                    timeout.as_secs()
                ));
            }
        };

        // Wait for log streaming to finish (with a short timeout to avoid blocking forever)
        let stream_timeout = Duration::from_secs(5);

        match tokio::time::timeout(stream_timeout, stdout_handle).await {
            Err(_) => warn!("Timed out waiting for stdout streaming to finish"),
            Ok(Err(e)) if e.is_panic() => {
                warn!("Stdout streaming task panicked: {:?}", e);
            }
            Ok(Err(e)) => warn!(error = ?e, "Stdout streaming task failed"),
            Ok(Ok(())) => {}
        }

        match tokio::time::timeout(stream_timeout, stderr_handle).await {
            Err(_) => warn!("Timed out waiting for stderr streaming to finish"),
            Ok(Err(e)) if e.is_panic() => {
                warn!("Stderr streaming task panicked: {:?}", e);
            }
            Ok(Err(e)) => warn!(error = ?e, "Stderr streaming task failed"),
            Ok(Ok(())) => {}
        }

        Ok(success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grpc_logger_run_command_echo() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let logger = GrpcLogger::new(broadcaster.clone());

        // Subscribe before running command
        let _receiver = broadcaster.subscribe();

        // Run a simple echo command
        let result = logger.run_command("echo", &["hello"], Some(10)).await;

        assert!(result.is_ok());
        assert!(result.unwrap()); // Should succeed

        // Should have received the output
        // Note: We might receive it or not depending on timing
    }

    #[tokio::test]
    async fn test_grpc_logger_run_command_failure() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let logger = GrpcLogger::new(broadcaster);

        // Run a command that will fail
        let result = logger.run_command("false", &[], Some(10)).await;

        assert!(result.is_ok());
        assert!(!result.unwrap()); // Should return false for non-zero exit
    }

    #[tokio::test]
    async fn test_grpc_logger_run_command_not_found() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));
        let logger = GrpcLogger::new(broadcaster);

        // Run a command that doesn't exist
        let result = logger
            .run_command("this-command-does-not-exist-12345", &[], Some(10))
            .await;

        assert!(result.is_err()); // Should error on spawn failure
    }
}
