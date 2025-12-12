//! Nix build execution

use crate::logs::LoggingProcess;
use anyhow::{Context, Result};
use std::process::Command;
use tracing::{debug, info};

/// Run nix-collect-garbage to clean the store
pub fn nix_collect_garbage() -> Result<()> {
    info!("Running nix-collect-garbage");

    let output = Command::new("nix-collect-garbage")
        .arg("-d")
        .output()
        .context("Failed to run nix-collect-garbage")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        debug!(stderr = %stderr, "nix-collect-garbage failed (may be normal)");
    }

    Ok(())
}

/// Run nix build for the specified attribute (blocking, no streaming)
///
/// # Arguments
/// * `flake_base` - Base flake reference (e.g., "github:lovesegfault/nix-bench")
/// * `attr` - Attribute to build (e.g., "large-deep")
#[allow(dead_code)]
pub fn run_nix_build(flake_base: &str, attr: &str) -> Result<()> {
    info!(flake_base, attr, "Running nix build");

    let flake_ref = format!("{}#{}", flake_base, attr);

    let status = Command::new("nix")
        .args([
            "build",
            &flake_ref,
            "--impure",
            "--no-link",
            "-L",
            "--log-format",
            "raw",
        ])
        .status()
        .context("Failed to run nix build")?;

    if !status.success() {
        anyhow::bail!("nix build failed with status: {}", status);
    }

    Ok(())
}

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
                "-L",
                "--log-format",
                "raw",
            ],
            timeout_secs,
        )
        .await?;

    if !success {
        anyhow::bail!("nix build failed");
    }

    Ok(())
}
