//! Nix build execution

use crate::logging::GrpcLogger;
use anyhow::Result;
use tracing::info;

/// Run nix build with streaming output to gRPC clients
///
/// # Arguments
/// * `flake_base` - Base flake reference (e.g., "github:lovesegfault/nix-bench")
/// * `attr` - Attribute to build (e.g., "large-deep")
/// * `logger` - gRPC logger for streaming output
/// * `timeout_secs` - Optional timeout in seconds
pub async fn run_nix_build(
    flake_base: &str,
    attr: &str,
    logger: &GrpcLogger,
    timeout_secs: Option<u64>,
) -> Result<()> {
    info!(flake_base, attr, "Running nix build with log streaming");

    let flake_ref = format!("{}#{}", flake_base, attr);

    let success = logger
        .run_command(
            "nix",
            &[
                "build",
                &flake_ref,
                "--impure",
                "--no-link",
                "-L", // Print build logs
                "--log-format",
                "bar-with-logs", // Show progress bar + build output
            ],
            timeout_secs,
        )
        .await?;

    if !success {
        anyhow::bail!("nix build failed");
    }

    Ok(())
}
