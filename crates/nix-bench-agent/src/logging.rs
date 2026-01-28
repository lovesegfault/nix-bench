//! gRPC-only logging for real-time output streaming
//!
//! This module provides logging that streams directly to gRPC clients,
//! with no CloudWatch dependencies.

use crate::command::{run_command_streaming, CommandConfig};
use crate::grpc::LogBroadcaster;
use anyhow::Result;
use nix_bench_common::timestamp_millis;
use std::sync::Arc;

/// gRPC-only logger for streaming output to connected clients
pub struct GrpcLogger {
    broadcaster: Arc<LogBroadcaster>,
}

impl GrpcLogger {
    /// Create a new gRPC logger
    pub fn new(broadcaster: Arc<LogBroadcaster>) -> Self {
        Self { broadcaster }
    }

    /// Write a log line to gRPC clients
    pub fn write_line(&self, message: &str) {
        self.broadcaster
            .broadcast(timestamp_millis(), message.to_string());
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
        let config = match timeout_secs {
            Some(secs) => CommandConfig::with_timeout_secs(secs),
            None => CommandConfig::for_benchmark(), // Default 2 hours
        };
        run_command_streaming(&self.broadcaster, cmd, args, &config).await
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
