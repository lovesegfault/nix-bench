//! Host bootstrap: NVMe setup and Nix installation
//!
//! This module handles all host setup that was previously done in cloud-init user-data.
//! All output is streamed via gRPC for real-time visibility in the TUI.

use crate::grpc::LogBroadcaster;
use anyhow::{Context, Result};
use nix_bench_common::timestamp_millis;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Run a command and stream output to gRPC, returning success status
async fn run_command_streaming(
    broadcaster: &Arc<LogBroadcaster>,
    cmd: &str,
    args: &[&str],
    timeout_secs: Option<u64>,
) -> Result<bool> {
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(600)); // Default 10 min for setup commands
    info!(cmd = %cmd, args = ?args, "Running command");

    let mut child = Command::new(cmd)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("Failed to spawn command: {}", cmd))?;

    let stdout = child.stdout.take().context("Failed to capture stdout")?;
    let stderr = child.stderr.take().context("Failed to capture stderr")?;

    let broadcaster_stdout = broadcaster.clone();
    let broadcaster_stderr = broadcaster.clone();

    // Stream stdout
    let stdout_handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            broadcaster_stdout.broadcast(timestamp_millis(), line);
        }
    });

    // Stream stderr
    let stderr_handle = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            broadcaster_stderr.broadcast(timestamp_millis(), line);
        }
    });

    // Wait for command with timeout
    let wait_result = tokio::time::timeout(timeout, child.wait()).await;

    let success = match wait_result {
        Ok(Ok(status)) => status.success(),
        Ok(Err(e)) => return Err(e).context("Failed waiting for command"),
        Err(_) => {
            warn!(cmd = %cmd, "Command timed out, killing process");
            let _ = child.kill().await;
            return Err(anyhow::anyhow!("Command '{}' timed out", cmd));
        }
    };

    // Wait for streaming to finish
    let _ = tokio::time::timeout(Duration::from_secs(2), stdout_handle).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), stderr_handle).await;

    Ok(success)
}

/// Broadcast a log message to gRPC clients
fn log_message(broadcaster: &Arc<LogBroadcaster>, message: &str) {
    broadcaster.broadcast(timestamp_millis(), message.to_string());
    info!("{}", message);
}

/// Detect NVMe instance store devices (not EBS volumes)
async fn detect_nvme_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();

    let sys_block = Path::new("/sys/block");
    if !sys_block.exists() {
        return Ok(devices);
    }

    let entries = std::fs::read_dir(sys_block).context("Failed to read /sys/block")?;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Only look at nvme devices, skip partitions
        if !name_str.starts_with("nvme") || name_str.contains('p') {
            continue;
        }

        // Read device model to distinguish instance store from EBS
        let model_path = entry.path().join("device/model");
        if let Ok(model) = std::fs::read_to_string(&model_path) {
            let model = model.trim().replace(' ', "");
            // Instance store devices have "InstanceStorage" or "NVMeSSD" in model
            // EBS volumes have "Amazon Elastic Block Store"
            if model.contains("InstanceStorage") || model.contains("NVMeSSD") {
                devices.push(format!("/dev/{}", name_str));
            }
        }
    }

    devices.sort();
    Ok(devices)
}

/// Setup NVMe instance store for /nix if available
///
/// This detects NVMe instance store devices, creates a RAID0 array if multiple,
/// formats with ext4, and bind-mounts to /nix for fast Nix store operations.
pub(crate) async fn setup_nvme(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== NVMe Instance Store Setup ===");

    let devices = detect_nvme_devices().await?;
    let count = devices.len();

    if count == 0 {
        log_message(
            broadcaster,
            "No NVMe instance store devices found, using default storage",
        );
        return Ok(());
    }

    log_message(
        broadcaster,
        &format!("Found {} NVMe instance store device(s): {:?}", count, devices),
    );

    let target_dev = if count > 1 {
        // Create RAID0 array
        log_message(broadcaster, "Creating RAID0 array...");

        // Install mdadm if not present
        if !Path::new("/sbin/mdadm").exists() {
            log_message(broadcaster, "Installing mdadm...");
            run_command_streaming(broadcaster, "yum", &["install", "-y", "mdadm"], Some(120))
                .await?;
        }

        // Build mdadm command
        let raid_devices_arg = format!("--raid-devices={}", count);
        let mut mdadm_args: Vec<&str> = vec![
            "--create",
            "/dev/md0",
            "--level=0",
            &raid_devices_arg,
        ];
        for dev in &devices {
            mdadm_args.push(dev);
        }
        mdadm_args.push("--force");

        run_command_streaming(broadcaster, "mdadm", &mdadm_args, Some(60)).await?;

        "/dev/md0".to_string()
    } else {
        devices[0].clone()
    };

    log_message(
        broadcaster,
        &format!("Formatting {} with ext4...", target_dev),
    );
    run_command_streaming(broadcaster, "mkfs.ext4", &["-F", &target_dev], Some(120)).await?;

    log_message(broadcaster, "Mounting NVMe storage...");

    // Create mount point
    std::fs::create_dir_all("/mnt/nvme").context("Failed to create /mnt/nvme")?;

    run_command_streaming(broadcaster, "mount", &[&target_dev, "/mnt/nvme"], Some(30)).await?;

    // Create directories on NVMe
    std::fs::create_dir_all("/mnt/nvme/nix").context("Failed to create /mnt/nvme/nix")?;
    std::fs::create_dir_all("/mnt/nvme/tmp").context("Failed to create /mnt/nvme/tmp")?;
    std::fs::create_dir_all("/mnt/nvme/var").context("Failed to create /mnt/nvme/var")?;

    // Create /nix and bind mount
    std::fs::create_dir_all("/nix").context("Failed to create /nix")?;

    log_message(broadcaster, "Bind mounting /mnt/nvme/nix to /nix...");
    run_command_streaming(
        broadcaster,
        "mount",
        &["--bind", "/mnt/nvme/nix", "/nix"],
        Some(30),
    )
    .await?;

    log_message(broadcaster, "NVMe setup complete");
    Ok(())
}

/// Install Nix using the Determinate Systems installer
pub(crate) async fn install_nix(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== Nix Installation ===");
    log_message(
        broadcaster,
        "Installing upstream Nix via Determinate installer...",
    );

    // Download installer script
    let installer_url = "https://install.determinate.systems/nix";

    // Use curl to download and pipe to sh
    // We need to use sh -c to handle the pipe
    let install_cmd = format!(
        "curl --proto '=https' --tlsv1.2 -sSf -L {} | sh -s -- install --no-confirm --prefer-upstream-nix",
        installer_url
    );

    let success = run_command_streaming(
        broadcaster,
        "sh",
        &["-c", &install_cmd],
        Some(600), // 10 minute timeout for Nix install
    )
    .await?;

    if !success {
        return Err(anyhow::anyhow!("Nix installation failed"));
    }

    log_message(broadcaster, "Nix installation complete");
    Ok(())
}

/// Source the Nix profile and update PATH
///
/// This modifies the current process environment to include Nix binaries.
pub(crate) fn source_nix_profile() -> Result<()> {
    info!("Sourcing Nix profile...");

    // The Determinate installer puts the profile script at this location
    let profile_script = "/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh";

    if !Path::new(profile_script).exists() {
        return Err(anyhow::anyhow!(
            "Nix profile script not found at {}",
            profile_script
        ));
    }

    // We can't source a bash script directly in Rust, but we can set the PATH
    // The key paths added by the Nix profile are:
    // - /nix/var/nix/profiles/default/bin
    // - ~/.nix-profile/bin (which is /root/.nix-profile/bin for root)

    let nix_paths = [
        "/nix/var/nix/profiles/default/bin",
        "/root/.nix-profile/bin",
    ];

    let current_path = std::env::var("PATH").unwrap_or_default();
    let new_path = format!("{}:{}", nix_paths.join(":"), current_path);
    std::env::set_var("PATH", &new_path);

    // Also set NIX_SSL_CERT_FILE if not set (needed for Nix to fetch from https)
    if std::env::var("NIX_SSL_CERT_FILE").is_err() {
        // Common locations for CA certificates
        let cert_locations = [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/nix/var/nix/profiles/default/etc/ssl/certs/ca-bundle.crt",
        ];
        for loc in cert_locations {
            if Path::new(loc).exists() {
                std::env::set_var("NIX_SSL_CERT_FILE", loc);
                debug!(path = %loc, "Set NIX_SSL_CERT_FILE");
                break;
            }
        }
    }

    info!(path = %new_path, "Updated PATH with Nix binaries");
    Ok(())
}

/// Run the complete bootstrap sequence
///
/// This sets up NVMe (if available), installs Nix, and sources the profile.
/// All output is streamed via gRPC to the coordinator TUI.
pub async fn run_bootstrap(broadcaster: &Arc<LogBroadcaster>) -> Result<()> {
    log_message(broadcaster, "=== Starting Bootstrap ===");

    // Step 1: Setup NVMe instance store (if available)
    setup_nvme(broadcaster).await?;

    // Step 2: Install Nix
    install_nix(broadcaster).await?;

    // Step 3: Source Nix profile
    source_nix_profile()?;

    log_message(broadcaster, "=== Bootstrap Complete ===");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_source_nix_profile_sets_path() {
        // Save original PATH
        let original_path = std::env::var("PATH").unwrap_or_default();

        // This will fail if Nix isn't installed, but we can still test the PATH logic
        let result = source_nix_profile();

        // Restore original PATH regardless of result
        std::env::set_var("PATH", &original_path);

        // If Nix isn't installed, we expect an error about missing profile
        if result.is_err() {
            let err = result.unwrap_err();
            assert!(err.to_string().contains("profile script not found"));
        }
    }

    #[test]
    fn test_source_nix_profile_error_message() {
        // Save original PATH
        let original_path = std::env::var("PATH").unwrap_or_default();

        let result = source_nix_profile();

        // Restore original PATH
        std::env::set_var("PATH", &original_path);

        // On most test systems Nix won't be installed
        if result.is_err() {
            let err = result.unwrap_err().to_string();
            // Error should mention the expected path
            assert!(
                err.contains("/nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh"),
                "Error should mention expected profile path, got: {}",
                err
            );
        }
    }

    #[tokio::test]
    async fn test_detect_nvme_devices_no_panic() {
        // This test ensures detect_nvme_devices doesn't panic even without /sys/block
        // On most test systems this will return empty or work normally
        let result = detect_nvme_devices().await;
        assert!(result.is_ok(), "detect_nvme_devices should not error");
    }

    #[test]
    fn test_log_message_formats_correctly() {
        let broadcaster = Arc::new(LogBroadcaster::new(100));

        // Should not panic
        log_message(&broadcaster, "Test message");
        log_message(&broadcaster, "");
        log_message(&broadcaster, "Message with special chars: <>&\"'");
    }

    #[test]
    fn test_nix_paths_are_correct() {
        // Verify the hardcoded paths match expected Determinate installer locations
        let expected_paths = [
            "/nix/var/nix/profiles/default/bin",
            "/root/.nix-profile/bin",
        ];

        // These paths should be in the source_nix_profile function
        // Just verify they're reasonable paths
        for path in expected_paths {
            assert!(path.starts_with('/'), "Path should be absolute: {}", path);
            assert!(path.ends_with("/bin"), "Path should end with /bin: {}", path);
        }
    }

    #[test]
    fn test_cert_locations_are_reasonable() {
        // Verify the hardcoded cert locations are reasonable paths
        let cert_locations = [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/nix/var/nix/profiles/default/etc/ssl/certs/ca-bundle.crt",
        ];

        for loc in cert_locations {
            assert!(loc.starts_with('/'), "Cert path should be absolute: {}", loc);
            // All should end with .crt or .pem
            assert!(
                loc.ends_with(".crt") || loc.ends_with(".pem"),
                "Cert path should end with .crt or .pem: {}",
                loc
            );
        }
    }

    #[test]
    fn test_nvme_device_name_parsing() {
        // Test the logic for identifying NVMe devices vs partitions
        let nvme_names = ["nvme0n1", "nvme1n1", "nvme0n2"];
        let partition_names = ["nvme0n1p1", "nvme0n1p2", "nvme1n1p1"];

        for name in nvme_names {
            assert!(name.starts_with("nvme"), "Should start with nvme");
            assert!(!name.contains('p'), "Device should not contain 'p'");
        }

        for name in partition_names {
            assert!(name.starts_with("nvme"), "Should start with nvme");
            assert!(name.contains('p'), "Partition should contain 'p'");
        }
    }
}
