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
#[allow(dead_code)]
pub fn run_nix_build(attr: &str) -> Result<()> {
    info!(attr, "Running nix build");

    let flake_ref = format!("github:lovesegfault/nix-bench#{}", attr);

    let status = Command::new("nix")
        .args([
            "build",
            &flake_ref,
            "--impure",
            "--no-link",
            "--print-build-logs",
        ])
        .status()
        .context("Failed to run nix build")?;

    if !status.success() {
        anyhow::bail!("nix build failed with status: {}", status);
    }

    Ok(())
}

/// Run nix build with streaming output to CloudWatch Logs
pub async fn run_nix_build_with_logging(attr: &str, logging: &LoggingProcess) -> Result<()> {
    info!(attr, "Running nix build with log streaming");

    let flake_ref = format!("github:lovesegfault/nix-bench#{}", attr);

    let success = logging
        .run_command(
            "nix",
            &["build", &flake_ref, "--impure", "--no-link", "--print-build-logs"],
        )
        .await?;

    if !success {
        anyhow::bail!("nix build failed");
    }

    Ok(())
}
