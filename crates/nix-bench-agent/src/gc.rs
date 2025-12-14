//! Nix store garbage collection with fixed-output derivation preservation
//!
//! This module provides garbage collection functionality that preserves
//! fixed-output derivations (FODs) - typically fetched sources like tarballs,
//! git repos, etc. This allows rebuilding packages without re-fetching.

use crate::logging::GrpcLogger;
use anyhow::{Context, Result};
use tracing::{info, warn};

/// Directory for temporary GC roots to protect FODs
const GC_ROOTS_DIR: &str = "/nix/var/nix/gcroots/nix-bench-fod-preserve";

/// Run garbage collection while preserving fixed-output derivations.
///
/// This function:
/// 1. Finds all FODs in the store (derivations with outputHash)
/// 2. Creates temporary GC roots for their outputs
/// 3. Runs garbage collection
/// 4. Removes the temporary roots
///
/// # Arguments
/// * `logger` - gRPC logger for streaming output to coordinator
/// * `timeout_secs` - Optional timeout for the entire GC operation
pub async fn run_fod_preserving_gc(logger: &GrpcLogger, timeout_secs: Option<u64>) -> Result<()> {
    info!("Starting FOD-preserving garbage collection");
    logger.write_line("=== Garbage Collection (preserving fetched sources) ===");

    // Create the temporary GC roots directory
    logger.write_line("Finding fixed-output derivations to preserve...");
    let create_dir_success = logger
        .run_command("mkdir", &["-p", GC_ROOTS_DIR], Some(30))
        .await
        .context("Failed to create GC roots directory")?;

    if !create_dir_success {
        anyhow::bail!("Failed to create GC roots directory");
    }

    // Find FODs and create GC roots for them
    // This is done as a single shell command for efficiency
    let protect_script = format!(
        r#"
for drv in /nix/store/*.drv; do
    if nix-store -q --binding outputHash "$drv" &>/dev/null; then
        for out in $(nix-store -q --outputs "$drv" 2>/dev/null); do
            if [ -e "$out" ]; then
                ln -sf "$out" "{}/$(basename "$out")" 2>/dev/null || true
            fi
        done
    fi
done
echo "FOD protection complete"
"#,
        GC_ROOTS_DIR
    );

    let protect_success = logger
        .run_command("bash", &["-c", &protect_script], timeout_secs)
        .await
        .context("Failed to protect FODs")?;

    if !protect_success {
        warn!("FOD protection script returned non-zero, continuing anyway");
    }

    // Count how many paths we're protecting
    let count_result = logger
        .run_command(
            "bash",
            &["-c", &format!("ls -1 {} 2>/dev/null | wc -l", GC_ROOTS_DIR)],
            Some(30),
        )
        .await;

    if let Ok(true) = count_result {
        logger.write_line("Protected FOD outputs, running garbage collection...");
    }

    // Run garbage collection
    info!("Running nix-collect-garbage");
    let gc_success = logger
        .run_command("nix-collect-garbage", &[], timeout_secs)
        .await
        .context("Failed to run garbage collection")?;

    if !gc_success {
        warn!("Garbage collection returned non-zero exit code");
    }

    // Clean up temporary GC roots
    logger.write_line("Cleaning up temporary GC roots...");
    let cleanup_success = logger
        .run_command("rm", &["-rf", GC_ROOTS_DIR], Some(30))
        .await
        .context("Failed to clean up GC roots")?;

    if !cleanup_success {
        warn!("Failed to clean up GC roots directory");
    }

    // Report disk usage after GC
    logger.write_line("Disk usage after GC:");
    let _ = logger
        .run_command("df", &["-h", "/nix/store"], Some(30))
        .await;

    info!("FOD-preserving garbage collection complete");
    logger.write_line("Garbage collection complete");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gc_roots_path() {
        assert!(GC_ROOTS_DIR.starts_with("/nix/var/nix/gcroots/"));
    }
}
