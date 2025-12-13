//! CloudWatch metrics reporting for the agent

use crate::agent::config::Config;
use crate::metrics::{self, Status, NAMESPACE};
use anyhow::{Context, Result};
use aws_sdk_cloudwatch::{
    types::{Dimension, MetricDatum, StandardUnit},
    Client,
};
use tracing::debug;

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

        // Use shared dimension builder to ensure consistency with coordinator
        let dimensions =
            metrics::build_dimensions(&config.run_id, &config.instance_type, &config.system);

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
