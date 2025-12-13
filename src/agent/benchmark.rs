//! Nix build execution

use super::logs::LoggingProcess;
use anyhow::Result;
use tracing::info;

/// Run nix build with streaming output to CloudWatch Logs
///
/// # Arguments
/// * `flake_base` - Base flake reference (e.g., "github:lovesegfault/nix-bench")
/// * `attr` - Attribute to build (e.g., "large-deep")
/// * `logging` - Logging process for streaming output
/// * `timeout_secs` - Optional timeout in seconds
pub async fn run_nix_build_with_logging(
    flake_base: &str,
    attr: &str,
    logging: &LoggingProcess,
    timeout_secs: Option<u64>,
) -> Result<()> {
    info!(flake_base, attr, "Running nix build with log streaming");

    let flake_ref = format!("{}#{}", flake_base, attr);

    let success = logging
        .run_command(
            "nix",
            &[
                "build",
                &flake_ref,
                "--impure",
                "--no-link",
                "-L",              // Print build logs
                "-v",              // Verbose mode for more output
                "--log-format",
                "bar-with-logs",   // Show progress bar + build output
            ],
            timeout_secs,
        )
        .await?;

    if !success {
        anyhow::bail!("nix build failed");
    }

    Ok(())
}
