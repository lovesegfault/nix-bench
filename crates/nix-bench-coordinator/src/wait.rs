//! Resource waiting with exponential backoff and cancellation support.

use anyhow::Result;
use backon::{BackoffBuilder, ExponentialBuilder};
use std::future::Future;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::debug;

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
/// Returns `Ok(())` when `check` returns `Ok(true)`. Errors from `check` are
/// propagated immediately. Times out after `config.timeout`.
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
    let timeout = config.timeout;
    tokio::time::timeout(timeout, async {
        let backoff = ExponentialBuilder::default()
            .with_min_delay(config.initial_delay)
            .with_max_delay(config.max_delay)
            .with_factor(2.0)
            .with_jitter()
            .build();

        for delay in backoff {
            if cancel.is_some_and(|t| t.is_cancelled()) {
                anyhow::bail!("Wait for {resource_name} cancelled");
            }
            match check().await {
                Ok(true) => {
                    debug!(resource = %resource_name, "Resource ready");
                    return Ok(());
                }
                Ok(false) => {
                    debug!(resource = %resource_name, delay_ms = delay.as_millis(), "Retrying");
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel_or_pending(cancel) => {
                            anyhow::bail!("Wait for {resource_name} cancelled");
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        anyhow::bail!("Backoff exhausted waiting for {resource_name}")
    })
    .await
    .map_err(|_| anyhow::anyhow!("Timeout waiting for {resource_name} after {timeout:?}"))?
}

/// Await cancellation if a token is provided, otherwise pend forever.
async fn cancel_or_pending(cancel: Option<&CancellationToken>) {
    match cancel {
        Some(t) => t.cancelled().await,
        None => std::future::pending().await,
    }
}
