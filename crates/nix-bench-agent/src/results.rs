//! S3 operations: config download only
//!
//! Results are now sent via gRPC, not S3.

use crate::config::{Config, validate_config};
use anyhow::{Context, Result};
use aws_sdk_s3::Client;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

/// Download config from S3, polling until TLS certificates are present.
///
/// The coordinator uploads agent binaries first, then launches instances.
/// Once instances are running and have public IPs, the coordinator generates
/// TLS certificates and uploads the full config. This function polls S3
/// until the config includes TLS certificates.
///
/// # Arguments
/// * `bucket` - S3 bucket name
/// * `run_id` - Run identifier
/// * `instance_type` - Instance type for config lookup
/// * `timeout` - Maximum time to wait for TLS config
///
/// # Returns
/// Config with TLS certificates, validated and ready to use.
pub async fn download_config_with_tls(
    bucket: &str,
    run_id: &str,
    instance_type: &str,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Config> {
    let aws_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let client = Client::new(&aws_config);

    let start = Instant::now();
    let mut delay = Duration::from_secs(2);
    let max_delay = Duration::from_secs(30);

    loop {
        if start.elapsed() > timeout {
            anyhow::bail!(
                "Timeout waiting for TLS config after {:?}. \
                The coordinator may have failed to generate certificates.",
                timeout
            );
        }

        match download_config_raw(&client, bucket, run_id, instance_type).await {
            Ok(config) => {
                // Check if TLS certs are present
                if config.ca_cert_pem.is_some()
                    && config.agent_cert_pem.is_some()
                    && config.agent_key_pem.is_some()
                {
                    info!("TLS config available, validating...");
                    // Validate the full config (will check TLS certs too)
                    validate_config(&config)?;
                    return Ok(config);
                }

                // Config exists but no TLS yet - coordinator is still generating certs
                info!(
                    elapsed = ?start.elapsed(),
                    "Config found but TLS certs not yet available, waiting..."
                );
            }
            Err(e) => {
                // Config doesn't exist yet - very early in startup
                debug!(
                    elapsed = ?start.elapsed(),
                    error = %e,
                    "Config not found yet, retrying..."
                );
            }
        }

        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = cancel.cancelled() => {
                anyhow::bail!("Config download cancelled");
            }
        }
        delay = (delay * 2).min(max_delay);
    }
}

/// Download config from S3 without validation (for polling)
async fn download_config_raw(
    client: &Client,
    bucket: &str,
    run_id: &str,
    instance_type: &str,
) -> Result<Config> {
    let key = format!("{}/config-{}.json", run_id, instance_type);
    debug!(bucket, key = %key, "Fetching config object");

    let response = client
        .get_object()
        .bucket(bucket)
        .key(&key)
        .send()
        .await
        .with_context(|| format!("Failed to download config from s3://{}/{}", bucket, key))?;

    let body = response
        .body
        .collect()
        .await
        .context("Failed to read config body from S3")?;

    let json =
        String::from_utf8(body.into_bytes().to_vec()).context("Config file is not valid UTF-8")?;

    // Parse without validation (we'll validate once TLS is present)
    let config: Config = serde_json::from_str(&json).context("Failed to parse config JSON")?;

    Ok(config)
}
