//! Resource waiting with exponential backoff and cancellation support.
//!
//! Provides a generic abstraction for waiting on AWS resources (or any async condition)
//! to become ready, with configurable exponential backoff, jitter, and cancellation.

use anyhow::Result;
use backon::{BackoffBuilder, ExponentialBuilder};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

/// Configuration for resource waiting with exponential backoff.
#[derive(Debug, Clone)]
pub struct WaitConfig {
    /// Initial delay between checks
    pub initial_delay: Duration,
    /// Maximum delay between checks (cap for exponential growth)
    pub max_delay: Duration,
    /// Maximum total time to wait before timeout
    pub timeout: Duration,
    /// Jitter factor (0.0 - 1.0) to add randomness to delays
    pub jitter: f64,
}

impl Default for WaitConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(10),
            timeout: Duration::from_secs(60),
            jitter: 0.25,
        }
    }
}

/// Wait for a resource to become ready with exponential backoff.
///
/// Uses `backon::ExponentialBuilder` for delay calculation and `tokio::select!`
/// for cancellation support.
///
/// # Arguments
/// * `config` - Wait configuration
/// * `cancel` - Optional cancellation token
/// * `check` - Async function that returns `Ok(true)` when ready, `Ok(false)` to retry
/// * `resource_name` - Name for logging
///
/// # Returns
/// * `Ok(())` - Resource is ready
/// * `Err` - Timeout, cancelled, or check returned an error
///
/// # Example
/// ```ignore
/// wait_for_resource(
///     WaitConfig::default(),
///     Some(&cancel_token),
///     || async {
///         let ready = check_if_resource_exists().await;
///         Ok(ready)
///     },
///     "my-resource",
/// ).await?;
/// ```
pub async fn wait_for_resource<F, Fut>(
    config: WaitConfig,
    cancel: Option<&CancellationToken>,
    check: F,
    resource_name: &str,
) -> Result<()>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<bool>>,
{
    let start = std::time::Instant::now();
    let mut attempts = 0u32;

    let backoff = ExponentialBuilder::default()
        .with_min_delay(config.initial_delay)
        .with_max_delay(config.max_delay)
        .with_factor(2.0)
        .with_jitter()
        .build();

    let mut delays = backoff.into_iter();

    loop {
        attempts += 1;

        // Check cancellation before each attempt
        if let Some(token) = cancel {
            if token.is_cancelled() {
                anyhow::bail!("Wait for {} cancelled", resource_name);
            }
        }

        // Check timeout
        if start.elapsed() >= config.timeout {
            anyhow::bail!(
                "Timeout waiting for {} after {:?} ({} attempts)",
                resource_name,
                config.timeout,
                attempts
            );
        }

        // Run the check
        match check().await {
            Ok(true) => {
                debug!(resource = %resource_name, attempts, "Resource ready");
                return Ok(());
            }
            Ok(false) => {
                let delay = delays.next().unwrap_or(config.max_delay);
                debug!(
                    resource = %resource_name,
                    attempt = attempts,
                    delay_ms = delay.as_millis(),
                    "Resource not ready, retrying"
                );

                // Wait with cancellation support
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    _ = async {
                        if let Some(token) = cancel {
                            token.cancelled().await
                        } else {
                            std::future::pending::<()>().await
                        }
                    } => {
                        anyhow::bail!("Wait for {} cancelled", resource_name);
                    }
                }
            }
            Err(e) => {
                warn!(resource = %resource_name, error = ?e, "Resource check failed");
                return Err(e);
            }
        }
    }
}
