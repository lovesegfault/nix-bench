//! Nix store garbage collection

use crate::logging::GrpcLogger;
use anyhow::{Context, Result};
use tracing::{info, warn};

/// Run garbage collection.
///
/// Executes `sudo nix-collect-garbage -d` to remove all unreferenced store paths
/// and delete old profile generations.
///
/// # Arguments
/// * `logger` - gRPC logger for streaming output to coordinator
/// * `timeout_secs` - Optional timeout for the GC operation
pub async fn run_gc(logger: &GrpcLogger, timeout_secs: Option<u64>) -> Result<()> {
    info!("Running garbage collection");
    logger.write_line("=== Garbage Collection ===");

    let gc_success = logger
        .run_command("sudo", &["nix-collect-garbage", "-d"], timeout_secs)
        .await
        .context("Failed to run garbage collection")?;

    if !gc_success {
        warn!("Garbage collection returned non-zero exit code");
    }

    // Report disk usage after GC
    logger.write_line("Disk usage after GC:");
    let _ = logger
        .run_command("df", &["-h", "/nix/store"], Some(30))
        .await;

    info!("Garbage collection complete");
    logger.write_line("Garbage collection complete");

    Ok(())
}
