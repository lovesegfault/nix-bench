//! CloudWatch Logs polling for real-time build output

use anyhow::{Context, Result};
use aws_sdk_cloudwatchlogs::Client;
use tracing::debug;

const LOG_GROUP_PREFIX: &str = "/nix-bench";

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

    /// Get logs for a specific instance type
    pub async fn get_logs(&self, instance_type: &str, start_time: Option<i64>) -> Result<Vec<String>> {
        let log_group = format!("{}/{}", LOG_GROUP_PREFIX, self.run_id);
        let log_stream = instance_type;

        let mut request = self
            .client
            .get_log_events()
            .log_group_name(&log_group)
            .log_stream_name(log_stream)
            .start_from_head(false); // Get newest first

        if let Some(ts) = start_time {
            request = request.start_time(ts);
        }

        let response = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                // Log group might not exist yet
                debug!(error = ?e, "Failed to get log events (may not exist yet)");
                return Ok(Vec::new());
            }
        };

        let events = response
            .events()
            .iter()
            .filter_map(|e| e.message().map(|s| s.to_string()))
            .collect();

        Ok(events)
    }

    /// Get all recent logs for an instance (last N lines)
    pub async fn get_recent_logs(&self, instance_type: &str, max_lines: usize) -> Result<String> {
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
            Err(_) => return Ok(String::new()),
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
