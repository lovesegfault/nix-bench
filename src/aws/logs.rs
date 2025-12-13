//! CloudWatch Logs polling for real-time build output

use anyhow::Result;
use aws_sdk_cloudwatchlogs::Client;
use thiserror::Error;
use tracing::{debug, warn};

const LOG_GROUP_PREFIX: &str = "/nix-bench";

/// Errors that can occur when fetching logs
#[derive(Debug, Error)]
pub enum LogsError {
    /// CloudWatch API is rate limiting requests
    #[error("CloudWatch Logs rate limited")]
    RateLimited,

    /// Log stream doesn't exist yet (expected during bootstrap)
    #[error("Log stream not found")]
    NotFound,

    /// Other AWS SDK error
    #[error("CloudWatch Logs error: {0}")]
    Sdk(#[from] aws_sdk_cloudwatchlogs::error::SdkError<aws_sdk_cloudwatchlogs::operation::get_log_events::GetLogEventsError>),
}

/// CloudWatch Logs client for reading build output
pub struct LogsClient {
    client: Client,
    run_id: String,
}

impl LogsClient {
    /// Create a new logs client
    pub async fn new(region: &str, run_id: &str) -> Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;

        let client = Client::new(&config);

        Ok(Self {
            client,
            run_id: run_id.to_string(),
        })
    }

    /// Get all recent logs for an instance (last N lines)
    ///
    /// Returns `Ok(String)` on success, or `Err(LogsError)` with specific error types:
    /// - `LogsError::NotFound` - Log stream doesn't exist yet (expected during bootstrap)
    /// - `LogsError::RateLimited` - CloudWatch is throttling requests
    /// - `LogsError::Sdk` - Other AWS SDK errors
    pub async fn get_recent_logs(&self, instance_type: &str, max_lines: usize) -> std::result::Result<String, LogsError> {
        let log_group = format!("{}/{}", LOG_GROUP_PREFIX, self.run_id);
        let log_stream = instance_type;

        let request = self
            .client
            .get_log_events()
            .log_group_name(&log_group)
            .log_stream_name(log_stream)
            .start_from_head(false)
            .limit(max_lines as i32);

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                // Check for specific error types
                let error_str = format!("{:?}", e);

                if error_str.contains("ResourceNotFoundException") {
                    // Log stream doesn't exist yet - expected during bootstrap
                    debug!(log_group = %log_group, log_stream = %log_stream, "Log stream not found yet");
                    return Err(LogsError::NotFound);
                } else if error_str.contains("ThrottlingException") || error_str.contains("Rate exceeded") {
                    warn!("CloudWatch Logs rate limited");
                    return Err(LogsError::RateLimited);
                } else {
                    warn!(error = ?e, "Failed to fetch CloudWatch Logs");
                    return Err(LogsError::Sdk(e));
                }
            }
        };

        let lines: Vec<String> = response
            .events()
            .iter()
            .filter_map(|e| e.message().map(|s| s.to_string()))
            .collect();

        // Events come in reverse order when start_from_head=false
        let mut lines = lines;
        lines.reverse();

        Ok(lines.join("\n"))
    }
}
