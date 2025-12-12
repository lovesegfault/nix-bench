//! CloudWatch Logs streaming for real-time build output

use crate::config::Config;
use anyhow::{Context, Result};
use aws_sdk_cloudwatchlogs::Client;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

const LOG_GROUP_PREFIX: &str = "/nix-bench";

/// CloudWatch Logs client for streaming build output
pub struct LogsClient {
    client: Client,
    log_group: String,
    log_stream: String,
    sequence_token: Arc<Mutex<Option<String>>>,
}

impl LogsClient {
    /// Create a new logs client and ensure log group/stream exist
    pub async fn new(config: &Config) -> Result<Self> {
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()))
            .load()
            .await;

        let client = Client::new(&aws_config);

        let log_group = format!("{}/{}", LOG_GROUP_PREFIX, config.run_id);
        let log_stream = config.instance_type.clone();

        // Create log group (ignore if exists)
        let _ = client
            .create_log_group()
            .log_group_name(&log_group)
            .send()
            .await;

        // Create log stream (ignore if exists)
        let _ = client
            .create_log_stream()
            .log_group_name(&log_group)
            .log_stream_name(&log_stream)
            .send()
            .await;

        debug!(log_group = %log_group, log_stream = %log_stream, "Created CloudWatch Logs stream");

        Ok(Self {
            client,
            log_group,
            log_stream,
            sequence_token: Arc::new(Mutex::new(None)),
        })
    }

    /// Write a log line
    pub async fn write_line(&self, message: &str) -> Result<()> {
        use aws_sdk_cloudwatchlogs::types::InputLogEvent;

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("System time is before UNIX epoch")?
            .as_millis() as i64;

        let event = InputLogEvent::builder()
            .timestamp(timestamp)
            .message(message)
            .build()
            .context("Failed to build log event")?;

        let mut token = self.sequence_token.lock().await;

        let mut request = self
            .client
            .put_log_events()
            .log_group_name(&self.log_group)
            .log_stream_name(&self.log_stream)
            .log_events(event);

        if let Some(ref t) = *token {
            request = request.sequence_token(t);
        }

        let response = request.send().await.context("Failed to put log events")?;

        // Update sequence token for next call
        *token = response.next_sequence_token().map(|s| s.to_string());

        Ok(())
    }

    /// Write multiple log lines
    pub async fn write_lines(&self, lines: &[String]) -> Result<()> {
        for line in lines {
            self.write_line(line).await?;
        }
        Ok(())
    }
}

/// Wrapper that captures command output and streams to CloudWatch Logs
pub struct LoggingProcess {
    logs: Arc<LogsClient>,
}

impl LoggingProcess {
    pub fn new(logs: Arc<LogsClient>) -> Self {
        Self { logs }
    }

    /// Run a command and stream its output to CloudWatch Logs
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
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;

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

        let logs_stdout = self.logs.clone();
        let logs_stderr = self.logs.clone();

        // Stream stdout
        let stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Err(e) = logs_stdout.write_line(&format!("[stdout] {}", line)).await {
                    warn!(error = %e, "Failed to write stdout line to CloudWatch");
                }
            }
        });

        // Stream stderr
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Err(e) = logs_stderr.write_line(&format!("[stderr] {}", line)).await {
                    warn!(error = %e, "Failed to write stderr line to CloudWatch");
                }
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
        if let Err(e) = tokio::time::timeout(stream_timeout, stdout_handle).await {
            warn!(error = %e, "Timed out waiting for stdout streaming to finish");
        }
        if let Err(e) = tokio::time::timeout(stream_timeout, stderr_handle).await {
            warn!(error = %e, "Timed out waiting for stderr streaming to finish");
        }

        Ok(success)
    }
}
