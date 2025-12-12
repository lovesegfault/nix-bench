//! Nix build execution

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

/// Run nix build for the specified attribute
pub fn run_nix_build(attr: &str) -> Result<()> {
    info!(attr, "Running nix build");

    // Build the flake reference
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
