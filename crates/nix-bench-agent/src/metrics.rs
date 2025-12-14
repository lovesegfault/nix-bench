//! CloudWatch metrics reporting for the agent

use crate::config::Config;
use anyhow::{Context, Result};
use aws_sdk_cloudwatch::{
    types::{Dimension, MetricDatum, StandardUnit},
    Client,
};
use nix_bench_common::metrics::{self, Status, NAMESPACE};
use tracing::debug;

/// Build the dimension vector for CloudWatch metrics.
///
/// This function creates AWS SDK `Dimension` objects using the constants
/// from `nix_bench_common::metrics::dimensions`.
pub fn build_dimensions(run_id: &str, instance_type: &str, system: &str) -> Vec<Dimension> {
    vec![
        Dimension::builder()
            .name(metrics::dimensions::RUN_ID)
            .value(run_id)
            .build(),
        Dimension::builder()
            .name(metrics::dimensions::INSTANCE_TYPE)
            .value(instance_type)
            .build(),
        Dimension::builder()
            .name(metrics::dimensions::SYSTEM)
            .value(system)
            .build(),
    ]
}

/// CloudWatch client for pushing metrics
pub struct CloudWatchClient {
    client: Client,
    dimensions: Vec<Dimension>,
}

impl CloudWatchClient {
    /// Create a new CloudWatch client
    pub async fn new(config: &Config) -> Result<Self> {
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()))
            .load()
            .await;

        let client = Client::new(&aws_config);

        // Use local dimension builder
        let dimensions = build_dimensions(&config.run_id, &config.instance_type, &config.system);

        Ok(Self { client, dimensions })
    }

    /// Push a status metric
    pub async fn put_status(&self, status: Status) -> Result<()> {
        debug!(status = ?status, "Pushing status metric");

        let datum = MetricDatum::builder()
            .metric_name(metrics::names::STATUS)
            .set_dimensions(Some(self.dimensions.clone()))
            .value(status.as_f64())
            .unit(StandardUnit::Count)
            .build();

        self.client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(datum)
            .send()
            .await
            .context("Failed to put status metric")?;

        Ok(())
    }

    /// Push a run progress metric
    pub async fn put_progress(&self, run: u32) -> Result<()> {
        debug!(run, "Pushing progress metric");

        let datum = MetricDatum::builder()
            .metric_name(metrics::names::RUN_PROGRESS)
            .set_dimensions(Some(self.dimensions.clone()))
            .value(run as f64)
            .unit(StandardUnit::Count)
            .build();

        self.client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(datum)
            .send()
            .await
            .context("Failed to put progress metric")?;

        Ok(())
    }

    /// Push a run duration metric
    pub async fn put_duration(&self, duration_secs: f64) -> Result<()> {
        debug!(duration_secs, "Pushing duration metric");

        let datum = MetricDatum::builder()
            .metric_name(metrics::names::RUN_DURATION)
            .set_dimensions(Some(self.dimensions.clone()))
            .value(duration_secs)
            .unit(StandardUnit::Seconds)
            .build();

        self.client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(datum)
            .send()
            .await
            .context("Failed to put duration metric")?;

        Ok(())
    }

    /// Push a custom metric with a count value
    pub async fn put_metric(&self, name: &str, value: f64) -> Result<()> {
        debug!(name, value, "Pushing custom metric");

        let datum = MetricDatum::builder()
            .metric_name(name)
            .set_dimensions(Some(self.dimensions.clone()))
            .value(value)
            .unit(StandardUnit::Count)
            .build();

        self.client
            .put_metric_data()
            .namespace(NAMESPACE)
            .metric_data(datum)
            .send()
            .await
            .with_context(|| format!("Failed to put {} metric", name))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_dimensions() {
        let dims = build_dimensions("run-123", "c7i.metal", "x86_64-linux");
        assert_eq!(dims.len(), 3);

        // Verify all dimension names are present
        let names: Vec<_> = dims.iter().filter_map(|d| d.name()).collect();
        assert!(names.contains(&metrics::dimensions::RUN_ID));
        assert!(names.contains(&metrics::dimensions::INSTANCE_TYPE));
        assert!(names.contains(&metrics::dimensions::SYSTEM));
    }
}
