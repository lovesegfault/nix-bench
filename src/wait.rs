//! Resource waiting with exponential backoff and cancellation support.
//!
//! Provides a generic abstraction for waiting on AWS resources (or any async condition)
//! to become ready, with configurable exponential backoff, jitter, and cancellation.

use anyhow::Result;
use rand::Rng;
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

impl WaitConfig {
    /// Create a new WaitConfig with the given timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            ..Default::default()
        }
    }
}

/// Wait for a resource to become ready with exponential backoff.
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
    let mut delay = config.initial_delay;
    let mut attempts = 0u32;

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
                // Add jitter to delay
                let jittered = jittered_delay(delay, config.jitter);
                debug!(
                    resource = %resource_name,
                    attempt = attempts,
                    delay_ms = jittered.as_millis(),
                    "Resource not ready, retrying"
                );

                // Wait with cancellation support
                tokio::select! {
                    _ = tokio::time::sleep(jittered) => {}
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

                // Exponential backoff
                delay = (delay * 2).min(config.max_delay);
            }
            Err(e) => {
                warn!(resource = %resource_name, error = ?e, "Resource check failed");
                return Err(e);
            }
        }
    }
}

/// Add jitter to a duration to prevent thundering herd.
fn jittered_delay(base: Duration, jitter_factor: f64) -> Duration {
    if jitter_factor <= 0.0 {
        return base;
    }
    let jitter = rand::thread_rng().gen_range(0.0..jitter_factor);
    Duration::from_secs_f64(base.as_secs_f64() * (1.0 + jitter))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_wait_succeeds_immediately() {
        let result = wait_for_resource(
            WaitConfig::default(),
            None,
            || async { Ok(true) },
            "test-resource",
        )
        .await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_wait_retries_then_succeeds() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(50),
                timeout: Duration::from_secs(5),
                jitter: 0.0,
            },
            None,
            || {
                let c = counter_clone.clone();
                async move {
                    let count = c.fetch_add(1, Ordering::SeqCst);
                    Ok(count >= 2) // Succeed on 3rd attempt
                }
            },
            "test-resource",
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_wait_timeout() {
        let result = wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(50),
                timeout: Duration::from_millis(100),
                jitter: 0.0,
            },
            None,
            || async { Ok(false) }, // Never ready
            "test-resource",
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Timeout"));
    }

    #[tokio::test]
    async fn test_wait_cancellation() {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        // Cancel after a short delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            cancel_clone.cancel();
        });

        let result = wait_for_resource(
            WaitConfig {
                initial_delay: Duration::from_millis(10),
                max_delay: Duration::from_millis(100),
                timeout: Duration::from_secs(10),
                jitter: 0.0,
            },
            Some(&cancel),
            || async { Ok(false) }, // Never ready
            "test-resource",
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cancelled"));
    }

    #[tokio::test]
    async fn test_wait_check_error() {
        let result = wait_for_resource(
            WaitConfig::default(),
            None,
            || async { anyhow::bail!("check failed") },
            "test-resource",
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("check failed"));
    }
}
