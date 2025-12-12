//! CloudWatch metrics polling

use anyhow::{Context, Result};
use aws_sdk_cloudwatch::{
    primitives::DateTime as AwsDateTime,
    types::{Dimension, MetricDataQuery, MetricStat},
    Client,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use tracing::debug;

const NAMESPACE: &str = "NixBench";

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
}

impl CloudWatchClient {
    /// Create a new CloudWatch client
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

    /// Poll metrics for all instances in this run
    pub async fn poll_metrics(
        &self,
        instance_types: &[String],
    ) -> Result<HashMap<String, InstanceMetrics>> {
        let mut results = HashMap::new();

        let end_time = Utc::now();
        let start_time = end_time - Duration::minutes(10);

        for instance_type in instance_types {
            let metrics = self
                .get_instance_metrics(instance_type, start_time, end_time)
                .await?;
            results.insert(instance_type.clone(), metrics);
        }

        Ok(results)
    }

    async fn get_instance_metrics(
        &self,
        instance_type: &str,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    ) -> Result<InstanceMetrics> {
        let dimensions = vec![
            Dimension::builder()
                .name("RunId")
                .value(&self.run_id)
                .build(),
            Dimension::builder()
                .name("InstanceType")
                .value(instance_type)
                .build(),
        ];

        // Query Status metric
        let status = self
            .get_latest_metric("Status", &dimensions, start_time, end_time)
            .await?;

        // Query RunProgress metric
        let progress = self
            .get_latest_metric("RunProgress", &dimensions, start_time, end_time)
            .await?;

        // Query RunDuration metrics
        let durations = self
            .get_all_metrics("RunDuration", &dimensions, start_time, end_time)
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
