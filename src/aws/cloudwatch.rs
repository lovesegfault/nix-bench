//! CloudWatch metrics polling

use crate::metrics::{self, NAMESPACE};
use anyhow::{Context, Result};
use aws_sdk_cloudwatch::{
    primitives::DateTime as AwsDateTime,
    types::{Dimension, MetricDataQuery, MetricStat},
    Client,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use tracing::debug;

/// Metrics for an instance
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct InstanceMetrics {
    pub status: Option<i32>,
    pub run_progress: Option<u32>,
    pub last_duration: Option<f64>,
    pub durations: Vec<f64>,
}

/// CloudWatch client for polling metrics
pub struct CloudWatchClient {
    client: Client,
    run_id: String,
    /// Maps instance_type to system (e.g., "x86_64-linux" or "aarch64-linux")
    instance_systems: HashMap<String, String>,
}

impl CloudWatchClient {
    /// Create a new CloudWatch client
    ///
    /// `instance_systems` maps each instance type to its system architecture
    /// (e.g., "c7i.metal" -> "x86_64-linux", "c7g.metal" -> "aarch64-linux")
    pub async fn new(
        region: &str,
        run_id: &str,
        instance_systems: HashMap<String, String>,
    ) -> Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(region.to_string()))
            .load()
            .await;

        let client = Client::new(&config);

        Ok(Self {
            client,
            run_id: run_id.to_string(),
            instance_systems,
        })
    }

    /// Poll metrics for all instances in this run
    pub async fn poll_metrics(
        &self,
        instance_types: &[String],
    ) -> Result<HashMap<String, InstanceMetrics>> {
        let mut results = HashMap::new();

        let end_time = Utc::now();
        let start_time = end_time - Duration::minutes(10);

        for instance_type in instance_types {
            let system = self
                .instance_systems
                .get(instance_type)
                .map(|s| s.as_str())
                .unwrap_or("x86_64-linux");

            let metrics = self
                .get_instance_metrics(instance_type, system, start_time, end_time)
                .await?;
            results.insert(instance_type.clone(), metrics);
        }

        Ok(results)
    }

    async fn get_instance_metrics(
        &self,
        instance_type: &str,
        system: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> Result<InstanceMetrics> {
        // Use shared dimension builder to ensure consistency with agent
        let dimensions = metrics::build_dimensions(&self.run_id, instance_type, system);

        // Query Status metric
        let status = self
            .get_latest_metric(metrics::names::STATUS, &dimensions, start_time, end_time)
            .await?;

        // Query RunProgress metric
        let progress = self
            .get_latest_metric(metrics::names::RUN_PROGRESS, &dimensions, start_time, end_time)
            .await?;

        // Query RunDuration metrics
        let durations = self
            .get_all_metrics(metrics::names::RUN_DURATION, &dimensions, start_time, end_time)
            .await?;

        Ok(InstanceMetrics {
            status: status.map(|v| v as i32),
            run_progress: progress.map(|v| v as u32),
            last_duration: durations.last().copied(),
            durations,
        })
    }

    async fn get_latest_metric(
        &self,
        metric_name: &str,
        dimensions: &[Dimension],
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> Result<Option<f64>> {
        let response = self
            .client
            .get_metric_data()
            .start_time(AwsDateTime::from_secs(start_time.timestamp()))
            .end_time(AwsDateTime::from_secs(end_time.timestamp()))
            .metric_data_queries(
                MetricDataQuery::builder()
                    .id("m1")
                    .metric_stat(
                        MetricStat::builder()
                            .metric(
                                aws_sdk_cloudwatch::types::Metric::builder()
                                    .namespace(NAMESPACE)
                                    .metric_name(metric_name)
                                    .set_dimensions(Some(dimensions.to_vec()))
                                    .build(),
                            )
                            .period(60)
                            .stat("Maximum")
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await
            .context("Failed to get metric data")?;

        let value = response
            .metric_data_results()
            .first()
            .and_then(|r| r.values().last().copied());

        debug!(metric = %metric_name, value = ?value, "Got metric");

        Ok(value)
    }

    async fn get_all_metrics(
        &self,
        metric_name: &str,
        dimensions: &[Dimension],
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> Result<Vec<f64>> {
        let response = self
            .client
            .get_metric_data()
            .start_time(AwsDateTime::from_secs(start_time.timestamp()))
            .end_time(AwsDateTime::from_secs(end_time.timestamp()))
            .metric_data_queries(
                MetricDataQuery::builder()
                    .id("m1")
                    .metric_stat(
                        MetricStat::builder()
                            .metric(
                                aws_sdk_cloudwatch::types::Metric::builder()
                                    .namespace(NAMESPACE)
                                    .metric_name(metric_name)
                                    .set_dimensions(Some(dimensions.to_vec()))
                                    .build(),
                            )
                            .period(60)
                            .stat("Maximum")
                            .build(),
                    )
                    .build(),
            )
            .send()
            .await
            .context("Failed to get metric data")?;

        let values = response
            .metric_data_results()
            .first()
            .map(|r| r.values().to_vec())
            .unwrap_or_default();

        Ok(values)
    }
}
