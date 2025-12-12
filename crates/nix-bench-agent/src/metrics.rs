//! CloudWatch metrics reporting

use crate::config::Config;
use anyhow::{Context, Result};
use aws_sdk_cloudwatch::{
    types::{Dimension, MetricDatum, StandardUnit},
    Client,
};
use tracing::debug;

/// Status values for the benchmark
#[derive(Debug, Clone, Copy)]
pub enum Status {
    Pending = 0,
    Running = 1,
    Complete = 2,
    Failed = -1,
}

/// CloudWatch client for pushing metrics
pub struct CloudWatchClient {
    client: Client,
    dimensions: Vec<Dimension>,
}

const NAMESPACE: &str = "NixBench";

impl CloudWatchClient {
    /// Create a new CloudWatch client
    pub async fn new(config: &Config) -> Result<Self> {
        let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new(config.region.clone()))
            .load()
            .await;

        let client = Client::new(&aws_config);

        let dimensions = vec![
            Dimension::builder()
                .name("RunId")
                .value(&config.run_id)
                .build(),
            Dimension::builder()
                .name("InstanceType")
                .value(&config.instance_type)
                .build(),
            Dimension::builder()
                .name("System")
                .value(&config.system)
                .build(),
        ];

        Ok(Self { client, dimensions })
    }

    /// Push a status metric
    pub async fn put_status(&self, status: Status) -> Result<()> {
        debug!(status = ?status, "Pushing status metric");

        let datum = MetricDatum::builder()
            .metric_name("Status")
            .set_dimensions(Some(self.dimensions.clone()))
            .value(status as i32 as f64)
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
            .metric_name("RunProgress")
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
            .metric_name("RunDuration")
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
}
